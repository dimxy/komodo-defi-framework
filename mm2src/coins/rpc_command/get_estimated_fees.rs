//! RPCs to start/stop gas fee estimator and get estimated base and priority fee per gas

use crate::{lp_coinfind_or_err, AsyncMutex, CoinFindError, MmCoinEnum, NumConversError};
use crate::eth::{EthCoin, EthCoinType, FeeEstimatorContext, FeeEstimatorState, FeePerGasEstimated};
use common::executor::{spawn_abortable, Timer};
use common::log::debug;
use common::{HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;
use std::sync::Arc;

const FEE_ESTIMATOR_NAME: &str = "eth_gas_fee_estimator_loop";


#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FeeEstimatorError {
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Gas fee estimation not supported for this coin")]
    CoinNotSupported,
    #[display(fmt = "Platform coin needs to be enabled for gas fee estimation")]
    PlatformCoinRequired,
    #[display(fmt = "Gas fee estimator is already started")]
    AlreadyStarted,
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Gas fee estimator is not running")]
    NotRunning,
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}

impl HttpStatusCode for FeeEstimatorError {
    fn status_code(&self) -> StatusCode {
        match self {
            FeeEstimatorError::NoSuchCoin { .. }
            | FeeEstimatorError::CoinNotSupported
            | FeeEstimatorError::PlatformCoinRequired
            | FeeEstimatorError::AlreadyStarted
            | FeeEstimatorError::NotRunning => StatusCode::BAD_REQUEST,
            FeeEstimatorError::Transport(_) | FeeEstimatorError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<NumConversError> for FeeEstimatorError {
    fn from(e: NumConversError) -> Self { FeeEstimatorError::InternalError(e.to_string()) }
}

impl From<String> for FeeEstimatorError {
    fn from(e: String) -> Self { FeeEstimatorError::InternalError(e) }
}

impl From<CoinFindError> for FeeEstimatorError {
    fn from(e: CoinFindError) -> Self {
        match e {
            CoinFindError::NoSuchCoin { coin } => FeeEstimatorError::NoSuchCoin { coin },
        }
    }
}

/// Gas fee estimator configuration
#[derive(Deserialize)]
enum FeeEstimatorConf {
    NotConfigured,
    #[serde(rename = "simple")]
    Simple,
    #[serde(rename = "provider")]
    Provider,
}

impl Default for FeeEstimatorConf {
    fn default() -> Self { Self::NotConfigured }
}

impl FeeEstimatorState {
    /// Creates gas FeeEstimatorContext if configured for this coin and chain id, otherwise returns None.
    /// The created context object (or None) is wrapped into a FeeEstimatorState so a gas fee rpc caller may know the reason why it was not created
    pub(crate) async fn init_fee_estimator(
        ctx: &MmArc,
        conf: &Json,
        coin_type: &EthCoinType,
    ) -> Result<Arc<FeeEstimatorState>, String> {
        let fee_estimator_json = conf["gas_fee_estimator"].clone();
        let fee_estimator_conf: FeeEstimatorConf = if !fee_estimator_json.is_null() {
            try_s!(json::from_value(fee_estimator_json))
        } else {
            Default::default()
        };
        match fee_estimator_conf {
            FeeEstimatorConf::Simple | FeeEstimatorConf::Provider => match coin_type {
                EthCoinType::Eth => {
                    let fee_estimator_ctx = AsyncMutex::new(FeeEstimatorContext {
                        estimated_fees: Default::default(),
                        abort_handler: AsyncMutex::new(None),
                    });
                    let fee_estimator_state = if matches!(fee_estimator_conf, FeeEstimatorConf::Simple) {
                        FeeEstimatorState::Simple(fee_estimator_ctx)
                    } else {
                        FeeEstimatorState::Provider(fee_estimator_ctx)
                    };
                    Ok(Arc::new(fee_estimator_state))
                },
                EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform, .. } => {
                    let platform_coin = lp_coinfind_or_err(ctx, platform).await;
                    match platform_coin {
                        Ok(MmCoinEnum::EthCoin(eth_coin)) => Ok(eth_coin.platform_fee_estimator_state.clone()),
                        _ => Ok(Arc::new(FeeEstimatorState::PlatformCoinRequired)),
                    }
                },
            },
            FeeEstimatorConf::NotConfigured => Ok(Arc::new(FeeEstimatorState::CoinNotSupported)),
        }
    }
}

impl FeeEstimatorContext {
    /// Fee estimation update period in secs, basically equals to eth blocktime
    const fn get_refresh_interval() -> f64 { 15.0 }

    fn get_estimator_ctx(coin: &EthCoin) -> Result<&AsyncMutex<FeeEstimatorContext>, MmError<FeeEstimatorError>> {
        match coin.platform_fee_estimator_state.deref() {
            FeeEstimatorState::CoinNotSupported => MmError::err(FeeEstimatorError::CoinNotSupported),
            FeeEstimatorState::PlatformCoinRequired => MmError::err(FeeEstimatorError::PlatformCoinRequired),
            FeeEstimatorState::Simple(fee_estimator_ctx) | FeeEstimatorState::Provider(fee_estimator_ctx) => {
                Ok(fee_estimator_ctx)
            },
        }
    }

    async fn start_if_not_running(coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::get_estimator_ctx(coin)?;
        let estimator_ctx = estimator_ctx.lock().await;
        let mut handler = estimator_ctx.abort_handler.lock().await;
        if handler.is_some() {
            return MmError::err(FeeEstimatorError::AlreadyStarted);
        }
        *handler = Some(spawn_abortable(Self::fee_estimator_loop(coin.clone())));
        Ok(())
    }

    async fn request_to_stop(coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::get_estimator_ctx(coin)?;
        let estimator_ctx = estimator_ctx.lock().await;
        let mut handle_guard = estimator_ctx.abort_handler.lock().await;
        // Handler will be dropped here, stopping the spawned loop immediately
        handle_guard
            .take()
            .map(|_| ())
            .or_mm_err(|| FeeEstimatorError::NotRunning)
    }

    async fn get_estimated_fees(coin: &EthCoin) -> Result<FeePerGasEstimated, MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::get_estimator_ctx(coin)?;
        let estimator_ctx = estimator_ctx.lock().await;
        let estimated_fees = estimator_ctx.estimated_fees.lock().await;
        Ok(estimated_fees.clone())
    }

    async fn check_if_estimator_supported(ctx: &MmArc, ticker: &str) -> Result<EthCoin, MmError<FeeEstimatorError>> {
        let eth_coin = match lp_coinfind_or_err(ctx, ticker).await? {
            MmCoinEnum::EthCoin(eth) => eth,
            _ => return MmError::err(FeeEstimatorError::CoinNotSupported),
        };
        let _ = Self::get_estimator_ctx(&eth_coin)?;
        Ok(eth_coin)
    }

    /// Loop polling gas fee estimator
    ///
    /// This loop periodically calls get_eip1559_gas_fee which fetches fee per gas estimations from a gas api provider or calculates them internally
    /// The retrieved data are stored in the fee estimator context
    /// To connect to the chain and gas api provider the web3 instances are used from an EthCoin coin passed in the start rpc param,
    /// so this coin must be enabled first.
    /// Once the loop started any other EthCoin in mainnet may request fee estimations.
    /// It is up to GUI to start and stop the loop when it needs it (considering that the data in context may be used
    /// for any coin with Eth or Erc20 type from the mainnet).
    async fn fee_estimator_loop(coin: EthCoin) {
        loop {
            let started = common::now_float();
            if let Ok(estimator_ctx) = Self::get_estimator_ctx(&coin) {
                let estimated_fees = coin.get_eip1559_gas_fee().await.unwrap_or_default();
                let estimator_ctx = estimator_ctx.lock().await;
                *estimator_ctx.estimated_fees.lock().await = estimated_fees;
            }

            let elapsed = common::now_float() - started;
            debug!("{FEE_ESTIMATOR_NAME} call to provider processed in {} seconds", elapsed);

            let wait_secs = FeeEstimatorContext::get_refresh_interval() - elapsed;
            let wait_secs = if wait_secs < 0.0 { 0.0 } else { wait_secs };
            Timer::sleep(wait_secs).await;
        }
    }
}

/// Rpc request to start or stop gas fee estimator
#[derive(Deserialize)]
pub struct FeeEstimatorStartStopRequest {
    coin: String,
}

/// Rpc response to request to start or stop gas fee estimator
#[derive(Serialize)]
pub struct FeeEstimatorStartStopResponse {
    result: String,
}

impl FeeEstimatorStartStopResponse {
    #[allow(dead_code)]
    pub fn get_result(&self) -> &str { &self.result }
}

pub type FeeEstimatorStartStopResult = Result<FeeEstimatorStartStopResponse, MmError<FeeEstimatorError>>;

/// Rpc request to get latest estimated fee per gas
#[derive(Deserialize)]
pub struct FeeEstimatorRequest {
    /// coin ticker
    coin: String,
}

pub type FeeEstimatorResult = Result<FeePerGasEstimated, MmError<FeeEstimatorError>>;

/// Start gas priority fee estimator loop
pub async fn start_eth_fee_estimator(ctx: MmArc, req: FeeEstimatorStartStopRequest) -> FeeEstimatorStartStopResult {
    let coin = FeeEstimatorContext::check_if_estimator_supported(&ctx, &req.coin).await?;
    FeeEstimatorContext::start_if_not_running(&coin).await?;
    Ok(FeeEstimatorStartStopResponse {
        result: "Success".to_string(),
    })
}

/// Stop gas priority fee estimator loop
pub async fn stop_eth_fee_estimator(ctx: MmArc, req: FeeEstimatorStartStopRequest) -> FeeEstimatorStartStopResult {
    let coin = FeeEstimatorContext::check_if_estimator_supported(&ctx, &req.coin).await?;
    FeeEstimatorContext::request_to_stop(&coin).await?;
    Ok(FeeEstimatorStartStopResponse {
        result: "Success".to_string(),
    })
}

/// Get latest estimated fee per gas for a eth coin
///
/// Estimation loop for this coin must be stated.
/// Only main chain is supported
///
/// Returns latest estimated fee per gas for the next block
pub async fn get_eth_estimated_fee_per_gas(ctx: MmArc, req: FeeEstimatorRequest) -> FeeEstimatorResult {
    let coin = FeeEstimatorContext::check_if_estimator_supported(&ctx, &req.coin).await?;
    let estimated_fees = FeeEstimatorContext::get_estimated_fees(&coin).await?;
    Ok(estimated_fees)
}
