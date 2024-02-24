//! RPCs to start/stop gas price estimator and get estimated base and priority fee per gas

use crate::eth::{EthCoin, FeePerGasEstimated};
use crate::AsyncMutex;
use crate::{from_ctx, lp_coinfind, MarketCoinOps, MmCoinEnum, NumConversError};
use common::executor::{SpawnFuture, Timer};
use common::log::debug;
use common::{HttpStatusCode, StatusCode};
use futures::channel::mpsc;
use futures::{select, Future, StreamExt};
use futures_util::FutureExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::mm_error::MmError;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

const FEE_ESTIMATOR_NAME: &str = "eth_fee_estimator_loop";
const MAX_CONCURRENT_STOP_REQUESTS: usize = 10;
const ETH_PLATFORM_COIN: &str = "ETH";
const ETH_SUPPORTED_CHAIN_ID: u64 = 1;

pub(crate) type FeeEstimatorStopListener = mpsc::Receiver<String>;
pub(crate) type FeeEstimatorStopHandle = mpsc::Sender<String>;

/// Gas fee estimator running loop state
enum FeeEstimatorState {
    Starting,
    Running,
    Stopping,
    Stopped,
}

impl Default for FeeEstimatorState {
    fn default() -> Self { Self::Stopped }
}

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum FeeEstimatorError {
    #[display(fmt = "Coin not activated")]
    CoinNotActivated,
    #[display(fmt = "Gas estimation not supported for this coin")]
    CoinNotSupported,
    #[display(fmt = "Coin not connected to fee estimator")]
    CoinNotConnected,
    #[display(fmt = "Platform coin ETH must be activated")]
    PlatformCoinNotActivated,
    #[display(fmt = "Chain id not supported")]
    ChainNotSupported,
    #[display(fmt = "Fee estimator is already started")]
    AlreadyStarted,
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Cannot start fee estimator if it's currently stopping")]
    CannotStartFromStopping,
    #[display(fmt = "Fee estimator is already stopping")]
    AlreadyStopping,
    #[display(fmt = "Fee estimator is not running")]
    NotRunning,
    #[display(fmt = "Internal error: {}", _0)]
    InternalError(String),
}

impl HttpStatusCode for FeeEstimatorError {
    fn status_code(&self) -> StatusCode {
        match self {
            FeeEstimatorError::CoinNotActivated
            | FeeEstimatorError::CoinNotSupported
            | FeeEstimatorError::CoinNotConnected
            | FeeEstimatorError::PlatformCoinNotActivated
            | FeeEstimatorError::ChainNotSupported
            | FeeEstimatorError::AlreadyStarted
            | FeeEstimatorError::AlreadyStopping
            | FeeEstimatorError::NotRunning
            | FeeEstimatorError::CannotStartFromStopping => StatusCode::BAD_REQUEST,
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
    estimated_fees: AsyncMutex<FeePerGasEstimated>,
    run_state: AsyncMutex<FeeEstimatorState>,
    /// coins that connected to loop and can get fee estimates
    using_coins: AsyncMutex<HashSet<String>>,
    /// receiver of signals to stop estimator (if it is not used)
    stop_listener: Arc<AsyncMutex<FeeEstimatorStopListener>>,
    /// sender of signal to stop estimator
    stop_handle: FeeEstimatorStopHandle,
    /// mm2 shutdown listener
    shutdown_listener: AsyncMutex<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
}

impl FeeEstimatorContext {
    fn new(ctx: MmArc) -> Result<Self, String> {
        let shutdown_listener = try_s!(ctx.graceful_shutdown_registry.register_listener());
        let (tx, rx) = mpsc::channel(MAX_CONCURRENT_STOP_REQUESTS);
        Ok(Self {
            estimated_fees: Default::default(),
            run_state: AsyncMutex::new(FeeEstimatorState::default()),
            using_coins: AsyncMutex::new(HashSet::new()),
            stop_listener: Arc::new(AsyncMutex::new(rx)),
            stop_handle: tx,
            shutdown_listener: AsyncMutex::new(Box::pin(shutdown_listener)),
        })
    }

    fn from_ctx(ctx: MmArc) -> Result<Arc<FeeEstimatorContext>, String> {
        Ok(try_s!(from_ctx(&ctx.clone().fee_estimator_ctx, move || {
            FeeEstimatorContext::new(ctx)
        })))
    }

    /// Fee estimation update period in secs, basically equals to eth blocktime
    const fn get_refresh_interval() -> f64 { 15.0 }

    async fn start_if_not_running(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let mut run_state = estimator_ctx.run_state.lock().await;
            match *run_state {
                FeeEstimatorState::Stopped => {
                    let platform_coin = Self::get_eth_platform_coin(&ctx).await?;
                    *run_state = FeeEstimatorState::Starting;
                    drop(run_state);
                    // we use platform coin to access its web3 connection, so it must be available
                    ctx.spawner()
                        .spawn(Self::fee_estimator_loop(ctx.clone(), platform_coin));
                    let mut using_coins = estimator_ctx.using_coins.lock().await;
                    using_coins.insert(coin.ticker().to_string());
                    debug!("{FEE_ESTIMATOR_NAME} coin {} connected", coin.ticker());
                    return Ok(());
                },
                FeeEstimatorState::Running => {
                    let mut using_coins = estimator_ctx.using_coins.lock().await;
                    using_coins.insert(coin.ticker().to_string());
                    debug!("{FEE_ESTIMATOR_NAME} coin {} connected", coin.ticker());
                    return Ok(());
                },
                FeeEstimatorState::Stopping => {
                    drop(run_state);
                    let _ = Self::wait_for_stopped(ctx.clone()).await;
                },
                FeeEstimatorState::Starting => {
                    drop(run_state);
                    let _ = Self::wait_for_running(ctx.clone()).await;
                },
            }
        }
    }

    async fn request_to_stop(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        Self::check_if_coin_connected(ctx.clone(), coin).await?;
        let estimator_ctx = Self::from_ctx(ctx)?;
        let run_state = estimator_ctx.run_state.lock().await;
        if let FeeEstimatorState::Running = *run_state {
            let mut stop_handle = estimator_ctx.stop_handle.clone();
            stop_handle
                .try_send(coin.ticker().to_owned())
                .map_err(|_| MmError::new(FeeEstimatorError::InternalError("could not stop".to_string())))?;
            debug!("{FEE_ESTIMATOR_NAME} sent stop request for {}", coin.ticker());
        } else {
            debug!(
                "{FEE_ESTIMATOR_NAME} could not stop for {}: coin not connected",
                coin.ticker()
            );
            return MmError::err(FeeEstimatorError::NotRunning);
        }
        Ok(())
    }

    /// run listen cycle: wait for loop stop or shutdown
    /// returns true if shutdown started to exist quickly
    async fn listen_for_stop(&self) -> Result<bool, MmError<FeeEstimatorError>> {
        let stop_listener = self.stop_listener.clone();
        let mut stop_listener = stop_listener.lock().await;
        let mut listen_fut = stop_listener.next().fuse();
        let mut shutdown_listener = self.shutdown_listener.lock().await;
        let shutdown_fut = async { shutdown_listener.as_mut().await }.fuse();

        let (disconnected_coin, shutdown_detected) = select! {
            disconnected_coin = listen_fut => (disconnected_coin, false),
            _ = Box::pin(shutdown_fut) => (None, true)
        };

        if shutdown_detected {
            debug!("{FEE_ESTIMATOR_NAME} received shutdown request");
            return Ok(true);
        } else if let Some(disconnected_coin) = disconnected_coin {
            let mut using_coins = self.using_coins.lock().await;
            if using_coins.remove(&disconnected_coin) {
                debug!("{FEE_ESTIMATOR_NAME} coin {} disconnected", disconnected_coin);
            }
            // stop loop if all coins disconnected
            if using_coins.is_empty() {
                let mut run_state = self.run_state.lock().await;
                *run_state = FeeEstimatorState::Stopping;
            }
        }
        Ok(false)
    }

    /// wait until the estimator loop state becomes Running
    async fn wait_for_running(ctx: MmArc) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let run_state = estimator_ctx.run_state.lock().await;
            if let FeeEstimatorState::Running = *run_state {
                break;
            }
            drop(run_state);
            Timer::sleep(0.1).await;
        }
        Ok(())
    }

    /// wait until the estimator loop state becomes Stopped
    async fn wait_for_stopped(ctx: MmArc) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let run_state = estimator_ctx.run_state.lock().await;
            if let FeeEstimatorState::Stopped = *run_state {
                break;
            }
            drop(run_state);
            Timer::sleep(0.1).await;
        }
        Ok(())
    }

    /// Gas fee estimator loop wrapper
    async fn fee_estimator_loop(ctx: MmArc, coin: EthCoin) {
        let estimator_ctx = Self::from_ctx(ctx.clone());
        if let Ok(estimator_ctx) = estimator_ctx {
            let _ = estimator_ctx.fee_estimator_loop_inner(coin).await;
        }
    }

    /// Loop polling gas fee estimator
    ///
    /// This loop periodically calls get_eip1559_gas_price which fetches fee per gas estimations from a gas api provider or calculates them internally
    /// The retrieved data are stored in the fee estimator context
    /// To connect to the gas api provider the web3 instances from the platform coin are used so ETH coin must be enabled
    /// TODO: assumed that once the plaform coin is enabled it is always available and never can be disabled. Should we track it disabled?
    async fn fee_estimator_loop_inner(&self, coin: EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let mut run_state = self.run_state.lock().await;
        if let FeeEstimatorState::Starting = *run_state {
            *run_state = FeeEstimatorState::Running;
            debug!("{FEE_ESTIMATOR_NAME} started");
        } else {
            debug!("{FEE_ESTIMATOR_NAME} could not start from this state, probably already running");
            return MmError::err(FeeEstimatorError::InternalError("could not start".to_string()));
        }
        // release lock:
        drop(run_state);

        loop {
            let mut run_state = self.run_state.lock().await;
            if let FeeEstimatorState::Stopping = *run_state {
                *run_state = FeeEstimatorState::Stopped;
                break;
            }
            drop(run_state);

            let started = common::now_float();
            let estimate_fut = coin.get_eip1559_gas_price().fuse();
            let stop_fut = self.listen_for_stop().fuse();
            let (estimated_res, shutdown_started) = select! {
                estimated = Box::pin(estimate_fut) => (estimated, Ok(false)),
                shutdown_started = Box::pin(stop_fut) => (Ok(FeePerGasEstimated::default()), shutdown_started)
            };
            // use returned bool (instead of run_state) to check if shutdown has just started and exit quickly
            if shutdown_started.is_ok() && shutdown_started.unwrap() {
                break;
            }

            let mut run_state = self.run_state.lock().await;
            if let FeeEstimatorState::Stopping = *run_state {
                *run_state = FeeEstimatorState::Stopped;
                break;
            }
            drop(run_state);

            let estimated = match estimated_res {
                Ok(estimated) => estimated,
                Err(_) => FeePerGasEstimated::default(), // TODO: if fee estimates could not be obtained I guess we should set a error?
            };
            let mut estimated_fees = self.estimated_fees.lock().await;
            *estimated_fees = estimated;
            drop(estimated_fees);

            let elapsed = common::now_float() - started;
            debug!(
                "{FEE_ESTIMATOR_NAME} getting estimated values processed in {} seconds",
                elapsed
            );

            let wait_secs = FeeEstimatorContext::get_refresh_interval() - elapsed;
            let wait_secs = if wait_secs < 0.0 { 0.0 } else { wait_secs };
            let sleep_fut = Timer::sleep(wait_secs).fuse();
            let stop_fut = self.listen_for_stop().fuse();
            let shutdown_started = select! {
                _ = Box::pin(sleep_fut) => Ok(false),
                shutdown_started = Box::pin(stop_fut) => shutdown_started
            };
            if shutdown_started.is_ok() && shutdown_started.unwrap() {
                break;
            }
        }
        debug!("{FEE_ESTIMATOR_NAME} stopped");
        Ok(())
    }

    async fn get_estimated_fees(ctx: MmArc, coin: &EthCoin) -> Result<FeePerGasEstimated, MmError<FeeEstimatorError>> {
        Self::check_if_coin_connected(ctx.clone(), coin).await?;
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let estimated_fees = estimator_ctx.estimated_fees.lock().await;
        Ok(estimated_fees.clone())
    }

    async fn check_if_coin_connected(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<FeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let using_coins = estimator_ctx.using_coins.lock().await;
        if using_coins.get(&coin.ticker().to_string()).is_none() {
            return MmError::err(FeeEstimatorError::CoinNotConnected);
        }
        Ok(())
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

    async fn get_eth_platform_coin(ctx: &MmArc) -> Result<EthCoin, MmError<FeeEstimatorError>> {
        let coin = match lp_coinfind(ctx, ETH_PLATFORM_COIN).await {
            Ok(Some(MmCoinEnum::EthCoin(eth))) => eth,
            _ => return MmError::err(FeeEstimatorError::PlatformCoinNotActivated),
        };
        Self::check_if_chain_id_supported(&coin)?;
        Ok(coin)
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
    let coin = FeeEstimatorContext::check_if_coin_supported(&ctx, &req.coin).await?;
    FeeEstimatorContext::request_to_stop(ctx, &coin).await?;
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
pub async fn get_eth_gas_price_estimated(ctx: MmArc, req: FeeEstimatorRequest) -> FeeEstimatorResult {
    let coin = FeeEstimatorContext::check_if_coin_supported(&ctx, &req.coin).await?;
    let estimated_fees = FeeEstimatorContext::get_estimated_fees(ctx, &coin).await?;
    Ok(estimated_fees)
}
