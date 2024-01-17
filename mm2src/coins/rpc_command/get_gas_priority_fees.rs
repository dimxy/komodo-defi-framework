use crate::eth::{u256_to_big_decimal, EthCoin, GasFeeEstimatedInternal, ETH_DECIMALS};
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
use mm2_number::BigDecimal;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

/// get_gas_priority_fees.rs
/// RPCs to start/stop priority fees estimator and get estimated fees

const PRIORITY_FEES_REFRESH_INTERVAL: u32 = 15; // in sec
const MAX_CONCURRENT_STOP_REQUESTS: usize = 10;
const GAS_FEE_ESTIMATOR_NAME: &str = "gas_fee_estimator_loop";

pub(crate) type GasFeeEstimatorStopListener = mpsc::Receiver<String>;
pub(crate) type GasFeeEstimatorStopHandle = mpsc::Sender<String>;

/// Gas fee estimator running loop state
enum GasFeeEstimatorState {
    Starting,
    Running,
    Stopping,
    Stopped,
}

impl Default for GasFeeEstimatorState {
    fn default() -> Self { Self::Stopped }
}

// Errors

#[derive(Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum GasFeeEstimatorError {
    #[display(fmt = "Coin not activated or not a EVM coin")]
    CoinNotFoundOrSupported,
    #[display(fmt = "Coin not connected to fee estimator")]
    CoinNotConnected,
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

impl HttpStatusCode for GasFeeEstimatorError {
    fn status_code(&self) -> StatusCode {
        match self {
            GasFeeEstimatorError::CoinNotFoundOrSupported
            | GasFeeEstimatorError::CoinNotConnected
            | GasFeeEstimatorError::AlreadyStarted
            | GasFeeEstimatorError::AlreadyStopping
            | GasFeeEstimatorError::NotRunning
            | GasFeeEstimatorError::CannotStartFromStopping => StatusCode::BAD_REQUEST,
            GasFeeEstimatorError::Transport(_) | GasFeeEstimatorError::InternalError(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }
}

impl From<NumConversError> for GasFeeEstimatorError {
    fn from(e: NumConversError) -> Self { GasFeeEstimatorError::InternalError(e.to_string()) }
}

impl From<String> for GasFeeEstimatorError {
    fn from(e: String) -> Self { GasFeeEstimatorError::InternalError(e) }
}

/// Gas fee estimator loop context
pub struct GasFeeEstimatorContext {
    estimated_fees: AsyncMutex<GasFeeEstimatedInternal>,
    run_state: AsyncMutex<GasFeeEstimatorState>,
    /// coins that connected to loop and can get fee estimates
    using_coins: AsyncMutex<HashSet<String>>,
    /// receiver of signals to stop estimator (if it is not used)
    stop_listener: Arc<AsyncMutex<GasFeeEstimatorStopListener>>,
    /// sender of signal to stop estimator
    stop_handle: GasFeeEstimatorStopHandle,
    /// mm2 shutdown listener
    shutdown_listener: AsyncMutex<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
}

impl GasFeeEstimatorContext {
    fn new(ctx: MmArc) -> Result<Self, String> {
        let shutdown_listener = try_s!(ctx.graceful_shutdown_registry.register_listener());
        let (tx, rx) = mpsc::channel(MAX_CONCURRENT_STOP_REQUESTS);
        Ok(Self {
            estimated_fees: Default::default(),
            run_state: AsyncMutex::new(GasFeeEstimatorState::default()),
            using_coins: AsyncMutex::new(HashSet::new()),
            stop_listener: Arc::new(AsyncMutex::new(rx)),
            stop_handle: tx,
            shutdown_listener: AsyncMutex::new(Box::pin(shutdown_listener)),
        })
    }

    fn from_ctx(ctx: MmArc) -> Result<Arc<GasFeeEstimatorContext>, String> {
        Ok(try_s!(from_ctx(&ctx.clone().gas_fee_estimator_ctx, move || {
            GasFeeEstimatorContext::new(ctx)
        })))
    }

    /// update period in secs
    fn get_refresh_rate() -> f64 { PRIORITY_FEES_REFRESH_INTERVAL as f64 }

    async fn start_if_not_running(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<GasFeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let mut run_state = estimator_ctx.run_state.lock().await;
            match *run_state {
                GasFeeEstimatorState::Stopped => {
                    *run_state = GasFeeEstimatorState::Starting;
                    drop(run_state);
                    ctx.spawner()
                        .spawn(Self::gas_fee_estimator_loop(ctx.clone(), coin.clone()));
                    return Ok(());
                },
                GasFeeEstimatorState::Running => {
                    let mut using_coins = estimator_ctx.using_coins.lock().await;
                    using_coins.insert(coin.ticker().to_string());
                    debug!("{GAS_FEE_ESTIMATOR_NAME} coin {} connected", coin.ticker());
                    return Ok(());
                },
                GasFeeEstimatorState::Stopping => {
                    drop(run_state);
                    let _ = Self::wait_for_stopped(ctx.clone()).await;
                },
                GasFeeEstimatorState::Starting => {
                    drop(run_state);
                    let _ = Self::wait_for_running(ctx.clone()).await;
                },
            }
        }
    }

    async fn request_to_stop(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<GasFeeEstimatorError>> {
        Self::check_if_coin_connected(ctx.clone(), coin).await?;
        let estimator_ctx = Self::from_ctx(ctx)?;
        let run_state = estimator_ctx.run_state.lock().await;
        if let GasFeeEstimatorState::Running = *run_state {
            let mut stop_handle = estimator_ctx.stop_handle.clone();
            stop_handle
                .try_send(coin.ticker().to_owned())
                .map_err(|_| MmError::new(GasFeeEstimatorError::InternalError("could not stop".to_string())))?;
            debug!("{GAS_FEE_ESTIMATOR_NAME} sent stop request for {}", coin.ticker());
        } else {
            debug!(
                "{GAS_FEE_ESTIMATOR_NAME} could not stop for {}: coin not connected",
                coin.ticker()
            );
            return MmError::err(GasFeeEstimatorError::NotRunning);
        }
        Ok(())
    }

    /// run listen cycle: wait for loop stop or shutdown
    /// returns true if shutdown started to exist quickly
    async fn listen_for_stop(&self) -> Result<bool, MmError<GasFeeEstimatorError>> {
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
            debug!("{GAS_FEE_ESTIMATOR_NAME} received shutdown request");
            return Ok(true);
        } else if let Some(disconnected_coin) = disconnected_coin {
            let mut using_coins = self.using_coins.lock().await;
            if using_coins.remove(&disconnected_coin) {
                debug!("{GAS_FEE_ESTIMATOR_NAME} coin {} disconnected", disconnected_coin);
            }
            // stop loop if all coins disconnected
            if using_coins.is_empty() {
                let mut run_state = self.run_state.lock().await;
                *run_state = GasFeeEstimatorState::Stopping;
            }
        }
        Ok(false)
    }

    async fn wait_for_running(ctx: MmArc) -> Result<(), MmError<GasFeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let run_state = estimator_ctx.run_state.lock().await;
            if let GasFeeEstimatorState::Running = *run_state {
                break;
            }
            drop(run_state);
            Timer::sleep(0.1).await;
        }
        Ok(())
    }

    async fn wait_for_stopped(ctx: MmArc) -> Result<(), MmError<GasFeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        loop {
            let run_state = estimator_ctx.run_state.lock().await;
            if let GasFeeEstimatorState::Stopped = *run_state {
                break;
            }
            drop(run_state);
            Timer::sleep(0.1).await;
        }
        Ok(())
    }

    async fn gas_fee_estimator_loop(ctx: MmArc, coin: EthCoin) {
        let estimator_ctx = Self::from_ctx(ctx.clone());
        if let Ok(estimator_ctx) = estimator_ctx {
            let _ = estimator_ctx.gas_fee_estimator_loop_inner(coin).await;
        }
    }

    /// Gas fee estimator loop
    async fn gas_fee_estimator_loop_inner(&self, coin: EthCoin) -> Result<(), MmError<GasFeeEstimatorError>> {
        let mut run_state = self.run_state.lock().await;
        if let GasFeeEstimatorState::Starting = *run_state {
            *run_state = GasFeeEstimatorState::Running;
            let mut using_coins = self.using_coins.lock().await;
            using_coins.insert(coin.ticker().to_string());
            debug!("{GAS_FEE_ESTIMATOR_NAME} started and coin {} connected", coin.ticker());
        } else {
            debug!("{GAS_FEE_ESTIMATOR_NAME} could not start from this state, probably already running");
            return MmError::err(GasFeeEstimatorError::InternalError("could not start".to_string()));
        }
        // release lock:
        drop(run_state);

        loop {
            let mut run_state = self.run_state.lock().await;
            if let GasFeeEstimatorState::Stopping = *run_state {
                *run_state = GasFeeEstimatorState::Stopped;
                break;
            }
            drop(run_state);

            let started = common::now_float();
            let estimate_fut = coin.get_eip1559_gas_price().fuse();
            let stop_fut = self.listen_for_stop().fuse();
            let (estimated_res, shutdown_started) = select! {
                estimated = Box::pin(estimate_fut) => (estimated, Ok(false)),
                shutdown_started = Box::pin(stop_fut) => (Ok(GasFeeEstimatedInternal::default()), shutdown_started)
            };
            // use returned bool (instead of run_state) to check if shutdown started to exit quickly
            if shutdown_started.is_ok() && shutdown_started.unwrap() {
                break;
            }

            let mut run_state = self.run_state.lock().await;
            if let GasFeeEstimatorState::Stopping = *run_state {
                *run_state = GasFeeEstimatorState::Stopped;
                break;
            }
            drop(run_state);

            let estimated = match estimated_res {
                Ok(estimated) => estimated,
                Err(_) => GasFeeEstimatedInternal::default(), // TODO: if fee estimates could not be obtained should we clear values or use previous?
            };
            let mut estimated_fees = self.estimated_fees.lock().await;
            *estimated_fees = estimated;
            drop(estimated_fees);

            let elapsed = common::now_float() - started;
            debug!(
                "{GAS_FEE_ESTIMATOR_NAME} getting estimated values processed in {} seconds",
                elapsed
            );

            let wait_secs = GasFeeEstimatorContext::get_refresh_rate() - elapsed;
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
        debug!("{GAS_FEE_ESTIMATOR_NAME} stopped");
        Ok(())
    }

    async fn get_estimated_fees(
        ctx: MmArc,
        coin: &EthCoin,
    ) -> Result<GasFeeEstimatedInternal, MmError<GasFeeEstimatorError>> {
        Self::check_if_coin_connected(ctx.clone(), coin).await?;
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let estimated_fees = estimator_ctx.estimated_fees.lock().await;
        Ok(estimated_fees.clone())
    }

    async fn check_if_coin_connected(ctx: MmArc, coin: &EthCoin) -> Result<(), MmError<GasFeeEstimatorError>> {
        let estimator_ctx = Self::from_ctx(ctx.clone())?;
        let using_coins = estimator_ctx.using_coins.lock().await;
        if using_coins.get(&coin.ticker().to_string()).is_none() {
            return MmError::err(GasFeeEstimatorError::CoinNotConnected);
        }
        Ok(())
    }
}

// Rpc request/response/result

#[derive(Deserialize)]
pub struct GasFeeEstimatorStartStopRequest {
    coin: String,
}

#[derive(Serialize)]
pub struct GasFeeEstimatorStartStopResponse {
    result: String,
}

impl GasFeeEstimatorStartStopResponse {
    #[allow(dead_code)]
    pub fn get_result(&self) -> String { self.result.clone() }
}

pub type GasFeeEstimatorStartStopResult = Result<GasFeeEstimatorStartStopResponse, MmError<GasFeeEstimatorError>>;

#[derive(Deserialize)]
pub struct GasFeeEstimatedRequest {
    coin: String,
}

/// Estimated priority gas fee
#[derive(Serialize)]
pub struct GasPriorityFee {
    /// estimated max priority tip fee per gas
    pub max_priority_fee_per_gas: BigDecimal,
    /// estimated max fee per gas
    pub max_fee_per_gas: BigDecimal,
    /// estimated transaction min wait time in mempool in ms for this priority level
    pub min_wait_time: Option<u32>,
    /// estimated transaction max wait time in mempool in ms for this priority level
    pub max_wait_time: Option<u32>,
}

#[derive(Serialize)]
pub struct GasFeeEstimatedResponse {
    pub base_fee: BigDecimal,
    pub low_fee: GasPriorityFee,
    pub medium_fee: GasPriorityFee,
    pub high_fee: GasPriorityFee,
}

pub type GasFeeEstimatedResult = Result<GasFeeEstimatedResponse, MmError<GasFeeEstimatorError>>;

/// Start gas priority fee estimator loop
pub async fn start_gas_fee_estimator(
    ctx: MmArc,
    req: GasFeeEstimatorStartStopRequest,
) -> GasFeeEstimatorStartStopResult {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(coin)) => coin,
        Ok(None) | Err(_) => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };
    let coin = match coin {
        MmCoinEnum::EthCoin(eth) => eth,
        _ => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };

    GasFeeEstimatorContext::start_if_not_running(ctx, &coin).await?;
    Ok(GasFeeEstimatorStartStopResponse {
        result: "Success".to_string(),
    })
}

/// Stop gas priority fee estimator loop
pub async fn stop_gas_fee_estimator(
    ctx: MmArc,
    req: GasFeeEstimatorStartStopRequest,
) -> GasFeeEstimatorStartStopResult {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(coin)) => coin,
        Ok(None) | Err(_) => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };
    let coin = match coin {
        MmCoinEnum::EthCoin(eth) => eth,
        _ => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };

    GasFeeEstimatorContext::request_to_stop(ctx, &coin).await?;
    Ok(GasFeeEstimatorStartStopResponse {
        result: "Success".to_string(),
    })
}

/// Stop gas priority fee estimator loop
pub async fn get_gas_priority_fees(ctx: MmArc, req: GasFeeEstimatedRequest) -> GasFeeEstimatedResult {
    let coin = match lp_coinfind(&ctx, &req.coin).await {
        Ok(Some(coin)) => coin,
        Ok(None) | Err(_) => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };

    // just check this is a eth-like coin.
    // we will return data if estimator is running, for any eth-like coin
    let coin = match coin {
        MmCoinEnum::EthCoin(eth) => eth,
        _ => return MmError::err(GasFeeEstimatorError::CoinNotFoundOrSupported),
    };

    let estimated_fees = GasFeeEstimatorContext::get_estimated_fees(ctx, &coin).await?;
    let eth_decimals = ETH_DECIMALS;
    Ok(GasFeeEstimatedResponse {
        base_fee: u256_to_big_decimal(estimated_fees.base_fee, eth_decimals)?,

        low_fee: GasPriorityFee {
            max_priority_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[0].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            max_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[0].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            min_wait_time: estimated_fees.priority_fees[0].min_wait_time,
            max_wait_time: estimated_fees.priority_fees[0].min_wait_time,
        },

        medium_fee: GasPriorityFee {
            max_priority_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[1].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            max_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[1].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            min_wait_time: estimated_fees.priority_fees[1].min_wait_time,
            max_wait_time: estimated_fees.priority_fees[1].min_wait_time,
        },

        high_fee: GasPriorityFee {
            max_priority_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[2].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            max_fee_per_gas: u256_to_big_decimal(
                estimated_fees.priority_fees[2].max_priority_fee_per_gas,
                eth_decimals,
            )?,
            min_wait_time: estimated_fees.priority_fees[2].min_wait_time,
            max_wait_time: estimated_fees.priority_fees[2].min_wait_time,
        },
    })
}
