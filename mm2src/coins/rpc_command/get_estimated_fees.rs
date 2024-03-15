//! RPCs to start/stop gas fee estimator and get estimated base and priority fee per gas

use std::sync::Arc;
use futures::compat::Future01CompatExt;
use crate::eth::{EthCoin, FeePerGasEstimated};
use crate::AsyncMutex;
use crate::{from_ctx, lp_coinfind, MmCoinEnum, NumConversError};
use common::executor::{spawn_abortable, AbortOnDropHandle, Timer};
use common::log::debug;
use common::{HttpStatusCode, StatusCode};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

const FEE_ESTIMATOR_NAME: &str = "eth_gas_fee_estimator_loop";
const ETH_SUPPORTED_CHAIN_ID: u64 = 1; // only eth mainnet is suppported (Blocknative gas platform currently supports Ethereum and Polygon/Matic mainnets.)
                                       // To support fee estimations for other chains add a FeeEstimatorContext for a new chain

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FeeEstimatorError {
    #[display(fmt = "Coin not activated")]
    CoinNotActivated,
    #[display(fmt = "Gas fee estimation not supported for this coin")]
    CoinNotSupported,
    #[display(fmt = "Chain id not supported")]
    ChainNotSupported,
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
            FeeEstimatorError::CoinNotActivated
            | FeeEstimatorError::CoinNotSupported
            | FeeEstimatorError::ChainNotSupported
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

/// Gas fee estimator loop context,
/// runs a loop to estimate max fee and max priority fee per gas according to EIP-1559 for the next block
///
/// This FeeEstimatorContext handles rpc requests which start and stop gas fee estimation loop and handles the loop itself.
/// FeeEstimatorContext maintains a list of eth coins or tokens which connected and use the estimator.
/// The loop estimation starts when first eth coin or token calls the start rpc and stops when the last coin or token, using it, calls the stop rpc.
/// FeeEstimatorContext keeps the latest estimated gas fees in the context and returns them as rpc response
pub struct FeeEstimatorContext {
    estimated_fees: Arc<AsyncMutex<FeePerGasEstimated>>,
    abort_handler: AsyncMutex<Option<AbortOnDropHandle>>,
}

impl FeeEstimatorContext {
    fn new() -> Result<Self, String> {
        Ok(Self {
            estimated_fees: Default::default(),
            abort_handler: AsyncMutex::new(None),
        })
    }

    fn from_ctx(ctx: MmArc) -> Result<Arc<FeeEstimatorContext>, String> {
        Ok(try_s!(from_ctx(&ctx.fee_estimator_ctx, move || {
            FeeEstimatorContext::new()
        })))
    }

    /// Fee estimation update period in secs, basically equals to eth blocktime
    const fn get_refresh_interval() -> f64 { 15.0 }

    async fn start_if_not_running(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let mut handler = estimator_ctx.abort_handler.lock().await;
        if handler.is_some() {
            return MmError::err(FeeEstimatorError::AlreadyStarted);
        }
        *handler = Some(spawn_abortable(fee_estimator_loop(
            estimator_ctx.estimated_fees.clone(),
            coin.clone(),
        )));
        Ok(())
    }

    async fn request_to_stop(ctx: MmArc) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx)?;
        let mut handle_guard = estimator_ctx.abort_handler.lock().await;
        // Handler will be dropped here, stopping the spawned loop immediately
        handle_guard
            .take()
            .map(|_| ())
            .or_mm_err(|| FeeEstimatorError::NotRunning)
    }

    async fn get_estimated_fees(ctx: MmArc) -> Result<FeePerGasEstimated, MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let estimated_fees = estimator_ctx.estimated_fees.lock().await;
        Ok(estimated_fees.clone())
    }

    fn check_if_chain_id_supported(coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        if let Some(chain_id) = coin.chain_id {
            if chain_id != ETH_SUPPORTED_CHAIN_ID {
                return MmError::err(FeeEstimatorError::ChainNotSupported);
            }
        }
        Ok(())
    }

    async fn check_if_coin_supported(ctx: &MmArc, ticker: &str) -> Result<EthCoin, MmError<FeeEstimatorError>> {
        let coin = match lp_coinfind(ctx, ticker).await {
            Ok(Some(MmCoinEnum::EthCoin(eth))) => eth,
            Ok(Some(_)) => return MmError::err(FeeEstimatorError::CoinNotSupported),
            Ok(None) | Err(_) => return MmError::err(FeeEstimatorError::CoinNotActivated),
        };
        Self::check_if_chain_id_supported(&coin)?;
        Ok(coin)
    }
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
async fn fee_estimator_loop(estimated_fees: Arc<AsyncMutex<FeePerGasEstimated>>, coin: EthCoin) {
    loop {
        let started = common::now_float();
        *estimated_fees.lock().await = coin.get_eip1559_gas_fee().compat().await.unwrap_or_default();

        let elapsed = common::now_float() - started;
        debug!(
            "{FEE_ESTIMATOR_NAME} getting estimated values processed in {} seconds",
            elapsed
        );

        let wait_secs = FeeEstimatorContext::get_refresh_interval() - elapsed;
        let wait_secs = if wait_secs < 0.0 { 0.0 } else { wait_secs };
        Timer::sleep(wait_secs).await;
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
    let coin = FeeEstimatorContext::check_if_coin_supported(&ctx, &req.coin).await?;
    FeeEstimatorContext::start_if_not_running(ctx, &coin).await?;
    Ok(FeeEstimatorStartStopResponse {
        result: "Success".to_string(),
    })
}

/// Stop gas priority fee estimator loop
pub async fn stop_eth_fee_estimator(ctx: MmArc, req: FeeEstimatorStartStopRequest) -> FeeEstimatorStartStopResult {
    FeeEstimatorContext::check_if_coin_supported(&ctx, &req.coin).await?;
    FeeEstimatorContext::request_to_stop(ctx).await?;
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
    FeeEstimatorContext::check_if_coin_supported(&ctx, &req.coin).await?;
    let estimated_fees = FeeEstimatorContext::get_estimated_fees(ctx).await?;
    Ok(estimated_fees)
}
