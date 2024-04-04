//! RPCs to start/stop gas fee estimator and get estimated base and priority fee per gas

use crate::eth::{EthCoin, EthCoinType, FeeEstimatorContext, FeePerGasEstimated};
use crate::{lp_coinfind_or_err, AsyncMutex, CoinFindError, MmCoinEnum, NumConversError};
use common::executor::{spawn_abortable, Timer};
use common::log::debug;
use common::{HttpStatusCode, StatusCode};
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use serde_json::Value as Json;
use std::sync::Arc;

const FEE_ESTIMATOR_NAME: &str = "eth_gas_fee_estimator_loop";

/// Chain id for which fee per gas estimations are supported.
/// Only eth mainnet currently is supported (Blocknative gas platform currently supports Ethereum and Polygon/Matic mainnets.)
/// TODO: make a setting in the coins file (platform coin) to indicate which chains support:
/// typed transactions, fee estimations by fee history and/or a gas provider.
const ETH_SUPPORTED_CHAIN_ID: u64 = 1;

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FeeEstimatorError {
    #[display(fmt = "No such coin {}", coin)]
    NoSuchCoin { coin: String },
    #[display(fmt = "Gas fee estimation not supported for this coin or chain id")]
    CoinNotSupported,
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

impl FeeEstimatorContext {
    /// Creates gas fee estimator context if supported for this coin and chain id. Otherwise returns None.
    /// When gas fee estimator rpc is called and no fee estimator was created
    /// it is assumed it is not supported for the coin or chain or coin config is inappropriate
    pub(crate) async fn new(ctx: &MmArc, conf: &Json, coin_type: &EthCoinType) -> Option<Arc<AsyncMutex<Self>>> {
        let chain_id = conf["chain_id"].as_u64()?;
        if chain_id != ETH_SUPPORTED_CHAIN_ID {
            return None;
        };
        match coin_type {
            EthCoinType::Eth => Some(Arc::new(AsyncMutex::new(Self {
                estimated_fees: Default::default(),
                abort_handler: AsyncMutex::new(None),
            }))),
            EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform, .. } => {
                let platform_coin = lp_coinfind_or_err(ctx, platform).await.ok()?;
                match platform_coin {
                    MmCoinEnum::EthCoin(eth_coin) => eth_coin.platform_fee_estimator_ctx.as_ref().cloned(),
                    _ => None,
                }
            },
        }
    }

    /// Fee estimation update period in secs, basically equals to eth blocktime
    const fn get_refresh_interval() -> f64 { 15.0 }

    fn get_estimator_ctx(coin: &EthCoin) -> Result<Arc<AsyncMutex<FeeEstimatorContext>>, MmError<FeeEstimatorError>> {
        Ok(coin
            .platform_fee_estimator_ctx
            .as_ref()
            .ok_or_else(|| MmError::new(FeeEstimatorError::CoinNotSupported))?
            .clone())
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
        if eth_coin.platform_fee_estimator_ctx.is_none() {
            return MmError::err(FeeEstimatorError::CoinNotSupported);
        }
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
                let estimated_fees = coin.get_eip1559_gas_fee().compat().await.unwrap_or_default();
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
