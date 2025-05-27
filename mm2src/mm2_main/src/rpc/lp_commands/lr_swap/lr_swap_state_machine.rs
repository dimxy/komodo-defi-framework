//! State machine for taker aggregated swap: liquidity routing swap (opt) + atomic swap + liquidity routing swap (opt)

use crate::common::executor::SpawnAbortable;
use crate::common::log::LogOnError;
use crate::lp_ordermatch::lp_auto_buy;
use crate::lp_swap::check_balance_for_taker_swap;
use crate::lp_swap::swap_lock::SwapLock;
use crate::lp_swap::swap_v2_common::*;
use crate::lp_swap::swap_v2_rpcs::{my_swap_status_rpc, MySwapStatusError, MySwapStatusRequest, SwapRpcData};
use crate::lp_swap::AGG_TAKER_SWAP_TYPE;
use crate::rpc::lp_commands::lr_swap::lr_errors::LrSwapError;
use crate::rpc::lp_commands::lr_swap::lr_helpers::get_coin_for_one_inch;
use crate::rpc::lp_commands::one_inch::types::ClassicSwapCreateRequest;
use async_trait::async_trait;
use coins::eth::{u256_to_big_decimal, wei_from_big_decimal};
use coins::hd_wallet::DisplayAddress;
use coins::MmCoin;
use coins::{lp_coinfind_or_err, ConfirmPaymentInput, FeeApproxStage, MarketCoinOps, SignEthTransactionParams,
            SignRawTransactionEnum, SignRawTransactionRequest};
use coins::{CoinWithDerivationMethod, MmCoinEnum, Ticker};
use common::executor::abortable_queue::AbortableQueue;
use common::executor::AbortSettings;
use common::executor::AbortableSystem;
use common::executor::Timer;
use common::log::{info, warn};
use common::Future01CompatExt;
use common::{new_uuid, now_sec};
use ethereum_types::U256;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::map_mm_error::MapMmError;
use mm2_err_handle::prelude::*;
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::{Mm2RpcResult, SellBuyRequest, SellBuyResponse, TakerAction};
use mm2_state_machine::prelude::*;
use mm2_state_machine::storable_state_machine::*;
use rpc::v1::types::Bytes as BytesJson;
use std::ops::Deref;
use std::sync::OnceLock;
use trading_api::one_inch_api::classic_swap_types::{ClassicSwapCreateParams, ClassicSwapData};
use trading_api::one_inch_api::client::{ApiClient, SwapApiMethods, SwapUrlBuilder};
use uuid::Uuid;

const LR_SWAP_CONFIRMATIONS: u64 = 1;
const LR_SWAP_WAIT_CONFIRM_TIMEOUT_SEC: u64 = 60;
const LR_SWAP_WAIT_CONFIRM_INTERVAL_SEC: u64 = 15;

cfg_native!(
    use common::async_blocking;
    use db_common::sqlite::rusqlite::{named_params, Error as SqlError, Result as SqlResult, Row};
    use db_common::sqlite::rusqlite::types::Type as SqlType;
    use crate::database::my_lr_swaps::insert_new_lr_swap;
    use crate::database::my_lr_swaps::SELECT_LR_SWAP_BY_UUID;
);

cfg_wasm32!(
    use crate::lp_swap::SwapsContext;
    use crate::lp_swap::swap_wasm_db::{MySwapsFiltersTable, SavedSwapTable};
    use crate::swap_versioning::legacy_swap_version;
);

/// Represents events produced by aggregated taker swap states.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event_type", content = "event_data")]
pub enum AggTakerSwapEvent {
    /// Run LR-swap before atomic swap and get its result
    RunLrSwap0 { req: ClassicSwapCreateRequest },
    /// Waiting for LR tx 0 to confirm
    WaitingForLrTxConfirmation0 { coin: Ticker, tx_bytes: BytesJson },
    /// Atomic swap has been successfully started
    StartAtomicSwap {},
    /// Waiting for running atomic swap
    WaitingForAtomicSwap {},
    /// Run LR-swap after atomic swap and get its result
    RunLrSwap1 { req: ClassicSwapCreateRequest },
    /// Waiting for LR tx 1 to confirm
    WaitingForLrTxConfirmation1 { coin: Ticker, tx_bytes: BytesJson },
    /// Aggregated swap has been aborted before any payment was sent.
    Aborted { reason: states::AbortReason },
    /// Aggregated swap completed successfully.
    Completed,
}

/// Storage for aggregated swaps
#[derive(Clone)]
pub struct AggTakerSwapStorage {
    ctx: MmArc,
}

impl AggTakerSwapStorage {
    pub fn new(ctx: MmArc) -> Self { AggTakerSwapStorage { ctx } }
}

#[async_trait]
impl StateMachineStorage for AggTakerSwapStorage {
    type MachineId = Uuid;
    type DbRepr = AggTakerSwapDbRepr;
    type Error = MmError<SwapStateMachineError>;

    #[cfg(not(target_arch = "wasm32"))]
    async fn store_repr(&mut self, _id: Self::MachineId, repr: Self::DbRepr) -> Result<(), Self::Error> {
        let ctx = self.ctx.clone();

        async_blocking(move || {
            let sql_params = named_params! {
                ":my_coin": repr.taker_coin,
                ":other_coin": repr.maker_coin,
                ":uuid": repr.uuid.to_string(),
                ":started_at": repr.started_at,
                ":swap_type": AGG_TAKER_SWAP_TYPE,
                ":swap_version": repr.swap_version,
            };
            insert_new_lr_swap(&ctx, sql_params)?;
            Ok(())
        })
        .await
    }

    #[cfg(target_arch = "wasm32")]
    async fn store_repr(&mut self, _uuid: Self::MachineId, _repr: Self::DbRepr) -> Result<(), Self::Error> {
        let swaps_ctx = SwapsContext::from_ctx(&self.ctx).expect("SwapsContext::from_ctx should not fail");
        let db = swaps_ctx.swap_db().await?;
        let _transaction = db.transaction().await?;
        // TODO: add for wasm
        // ...
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn get_repr(&self, id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
        let ctx = self.ctx.clone();
        let id_str = id.to_string();

        async_blocking(move || {
            Ok(ctx.sqlite_connection().query_row(
                SELECT_LR_SWAP_BY_UUID,
                &[(":uuid", &id_str)],
                Self::DbRepr::from_sql_row,
            )?)
        })
        .await
    }

    #[cfg(target_arch = "wasm32")]
    async fn get_repr(&self, id: Self::MachineId) -> Result<Self::DbRepr, Self::Error> {
        get_swap_repr(&self.ctx, id).await
    }

    async fn has_record_for(&mut self, id: &Self::MachineId) -> Result<bool, Self::Error> {
        has_db_record_for(self.ctx.clone(), id).await
    }

    async fn store_event(&mut self, id: Self::MachineId, event: AggTakerSwapEvent) -> Result<(), Self::Error> {
        store_swap_event::<AggTakerSwapDbRepr>(self.ctx.clone(), id, event).await
    }

    async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
        get_unfinished_swaps_uuids(self.ctx.clone(), AGG_TAKER_SWAP_TYPE).await
    }

    async fn mark_finished(&mut self, id: Self::MachineId) -> Result<(), Self::Error> {
        mark_swap_as_finished(self.ctx.clone(), id).await
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AggTakerSwapDbRepr {
    /// Maker coin
    pub maker_coin: String,
    /// The timestamp when the swap was started.
    pub started_at: u64,
    /// Taker coin
    pub taker_coin: String,
    /// UUID of the agg swap
    pub uuid: Uuid,
    /// Swap events
    pub events: Vec<AggTakerSwapEvent>,
    /// Swap protocol version
    #[cfg_attr(target_arch = "wasm32", serde(default = "legacy_swap_version"))]
    pub swap_version: u8,
}

impl StateMachineDbRepr for AggTakerSwapDbRepr {
    type Event = AggTakerSwapEvent;

    fn add_event(&mut self, event: Self::Event) { self.events.push(event) }
}

#[cfg(not(target_arch = "wasm32"))]
impl AggTakerSwapDbRepr {
    fn from_sql_row(row: &Row) -> SqlResult<Self> {
        Ok(AggTakerSwapDbRepr {
            taker_coin: row.get(0)?,
            maker_coin: row.get(1)?,
            uuid: row
                .get::<_, String>(2)?
                .parse()
                .map_err(|e| SqlError::FromSqlConversionFailure(2, SqlType::Text, Box::new(e)))?,
            started_at: row.get(3)?,
            events: serde_json::from_str(&row.get::<_, String>(7)?)
                .map_err(|e| SqlError::FromSqlConversionFailure(7, SqlType::Text, Box::new(e)))?,
            swap_version: row.get(20)?,
        })
    }
}

/// Represents the state machine for maker's side of the Trading Protocol Upgrade swap (v2).
pub struct AggTakerSwapStateMachine {
    /// MM2 context
    pub ctx: MmArc,
    /// Storage
    pub storage: AggTakerSwapStorage,

    /// Abortable queue used to spawn related activities
    pub abortable_system: AbortableQueue,

    /// Params for the LR swap running before the atomic swap (optional)
    pub lr_swap_0: Option<ClassicSwapCreateRequest>,
    /// Params for the LR swap running after the atomic swap (optional)
    pub lr_swap_1: Option<ClassicSwapCreateRequest>,

    //pub classic_swap_details: ClassicSwapDetails,
    /// Sell or but params for the atomic swap
    pub sell_buy_request: SellBuyRequest,
    /// The UUID of the atomic swap
    pub atomic_swap_uuid: OnceLock<Uuid>,
    /// unique ID of the agg swap for stopping or querying status.
    pub uuid: Uuid,
    /// The timestamp when the agg swap was started.
    pub started_at: u64,
    /// version
    pub swap_version: u8,
}

#[async_trait]
impl StorableStateMachine for AggTakerSwapStateMachine {
    type Storage = AggTakerSwapStorage;
    type Result = ();
    type Error = MmError<SwapStateMachineError>;
    type ReentrancyLock = SwapLock;
    type RecreateCtx = ();
    type RecreateError = MmError<SwapRecreateError>;

    fn to_db_repr(&self) -> AggTakerSwapDbRepr {
        AggTakerSwapDbRepr {
            maker_coin: self.maker_coin().expect("maker coin should be set").to_owned(),
            started_at: self.started_at,
            taker_coin: self.taker_coin().expect("taker coin should be set").to_owned(),
            uuid: self.uuid,
            events: Vec::new(),
            swap_version: self.swap_version,
        }
    }

    fn storage(&mut self) -> &mut Self::Storage { &mut self.storage }

    fn id(&self) -> <Self::Storage as StateMachineStorage>::MachineId { self.uuid }

    async fn recreate_machine(
        uuid: Uuid,
        storage: AggTakerSwapStorage,
        mut repr: AggTakerSwapDbRepr,
        _recreate_ctx: Self::RecreateCtx,
    ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Self::RecreateError> {
        let current_state: Box<dyn RestoredState<StateMachine = Self>> = match repr
            .events
            .pop()
            .ok_or(MmError::new(SwapRecreateError::ReprEventsEmpty))?
        {
            AggTakerSwapEvent::StartAtomicSwap {} => Box::new(states::StartAtomicSwap {}),
            AggTakerSwapEvent::RunLrSwap0 { req } => Box::new(states::RunLrSwap0 { req }),
            AggTakerSwapEvent::RunLrSwap1 { req } => Box::new(states::RunLrSwap1 { req }),

            AggTakerSwapEvent::Aborted { .. } => return MmError::err(SwapRecreateError::SwapAborted),
            AggTakerSwapEvent::Completed => return MmError::err(SwapRecreateError::SwapCompleted),
            _ => return MmError::err(SwapRecreateError::FailedToParseData("".to_string())),
        };

        let machine = AggTakerSwapStateMachine {
            ctx: storage.ctx.clone(),
            abortable_system: storage
                .ctx
                .abortable_system
                .create_subsystem()
                .expect("create_subsystem should not fail"),
            storage,
            lr_swap_0: Default::default(),
            lr_swap_1: Default::default(),
            sell_buy_request: Default::default(),
            atomic_swap_uuid: OnceLock::default(), // TODO: set from db
            uuid,
            swap_version: AggTakerSwapStateMachine::AGG_SWAP_VERSION,
            started_at: 0,
        };

        Ok((RestoredMachine::new(machine), current_state))
    }

    async fn acquire_reentrancy_lock(&self) -> Result<Self::ReentrancyLock, Self::Error> {
        acquire_reentrancy_lock_impl(&self.ctx, self.uuid).await
    }

    fn spawn_reentrancy_lock_renew(&mut self, guard: Self::ReentrancyLock) {
        spawn_reentrancy_lock_renew_impl(&self.abortable_system, self.uuid, guard)
    }

    fn init_additional_context(&mut self) {
        let swap_info = ActiveSwapV2Info {
            uuid: self.uuid,
            maker_coin: self.maker_coin().expect("maker coin should be set").to_owned(),
            taker_coin: self.taker_coin().expect("taker coin should be set").to_owned(),
            swap_type: AGG_TAKER_SWAP_TYPE,
        };
        init_agg_swap_context_impl(&self.ctx, swap_info);
        println!("init_additional_context uuid={}", self.uuid);
    }

    fn clean_up_context(&mut self) {
        let maker_coin = self.maker_coin().expect("maker coin should be set");
        let taker_coin = self.taker_coin().expect("taker coin should be set");
        clean_up_agg_swap_context_impl(&self.ctx, &self.uuid, maker_coin, taker_coin);
    }

    fn on_event(&mut self, event: &AggTakerSwapEvent) {
        match event {
            AggTakerSwapEvent::StartAtomicSwap {} => {
                /*let swaps_ctx = SwapsContext::from_ctx(&self.ctx).expect("from_ctx should not fail at this point");
                swaps_ctx
                    .locked_amounts
                    .lock()
                    .unwrap()
                    .entry(maker_coin_ticker)
                    .or_insert_with(Vec::new)
                    .push(new_locked);*/
            },
            AggTakerSwapEvent::Aborted { .. } | AggTakerSwapEvent::Completed => (),
            _ => (),
        }

        // TODO: add streamer call
        // Send a notification to the swap status streamer about a new event.
        /*self.ctx
        .event_stream_manager
        .send_fn(SwapStatusStreamer::derive_streamer_id(), || SwapStatusEvent::AggTaker {
            uuid: self.uuid,
            event: event.clone(),
        })
        .ok();*/
    }

    fn on_kickstart_event(&mut self, _event: AggTakerSwapEvent) {}
}

mod states {
    use super::*;

    /// Represents a state used to start a new aggregated taker swap.
    #[derive(Default)]
    pub struct Initialize {}

    impl InitialState for Initialize {
        type StateMachine = AggTakerSwapStateMachine;
    }

    #[async_trait]
    impl State for Initialize {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            let my_amount = state_machine.swap_amount().unwrap_or_default();
            let base_ticker = state_machine.maker_coin().unwrap_or_default();
            let rel_ticker = state_machine.taker_coin().unwrap_or_default();
            info!(
                "Aggregated taker swap with LR {} for {}/{} for amount {} starting...",
                state_machine.uuid,
                base_ticker,
                rel_ticker,
                my_amount.to_decimal()
            );
            if let Some(ref lr_swap_params) = state_machine.lr_swap_0 {
                let run_lr_swap = RunLrSwap0 {
                    req: lr_swap_params.clone(),
                };
                return Self::change_state(run_lr_swap, state_machine).await;
            }
            let run_atomic_swap = StartAtomicSwap {};
            Self::change_state(run_atomic_swap, state_machine).await
        }
    }

    /// State to start atomic swap step
    pub(super) struct StartAtomicSwap {}

    #[async_trait]
    impl State for StartAtomicSwap {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            match state_machine.start_lp_auto_buy().await {
                Ok(resp) => {
                    let _ = state_machine
                        .atomic_swap_uuid
                        .set(resp.request.uuid)
                        .expect("Atomic swap UUID should be empty");
                    let next_state = WaitForAtomicSwap {};
                    Self::change_state(next_state, state_machine).await
                },
                Err(err) => {
                    let next_state = Aborted {
                        reason: AbortReason::SomeReason(err.get_inner().to_string()),
                    };
                    Self::change_state(next_state, state_machine).await
                },
            }
        }
    }

    impl StorableState for StartAtomicSwap {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent { AggTakerSwapEvent::StartAtomicSwap {} }
    }

    impl TransitionFrom<Initialize> for StartAtomicSwap {}
    impl TransitionFrom<WaitForLrTxConfirmation0> for StartAtomicSwap {}

    /// State to wait for the atomic swap step to finish
    pub(super) struct WaitForAtomicSwap {}

    #[async_trait]
    impl State for WaitForAtomicSwap {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            if let Err(err) = state_machine.wait_for_atomic_swap_finished().await {
                let next_state = Aborted {
                    reason: AbortReason::SomeReason(err.get_inner().to_string()),
                };
                return Self::change_state(next_state, state_machine).await;
            }
            if let Some(ref lr_swap_req) = state_machine.lr_swap_1 {
                let run_lr_swap = RunLrSwap1 {
                    req: lr_swap_req.clone(),
                };
                return Self::change_state(run_lr_swap, state_machine).await;
            }
            let completed = Completed {};
            Self::change_state(completed, state_machine).await
        }
    }

    impl StorableState for WaitForAtomicSwap {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent { AggTakerSwapEvent::WaitingForAtomicSwap {} }
    }

    impl TransitionFrom<StartAtomicSwap> for WaitForAtomicSwap {}

    /// State to create and send a LR swap tx (before atomic swap)
    pub(super) struct RunLrSwap0 {
        pub req: ClassicSwapCreateRequest,
    }

    #[async_trait]
    impl State for RunLrSwap0 {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            let (base_ticker, tx_bytes) = match state_machine.run_lr_swap(&self.req).await {
                Ok((base_ticker, tx_bytes)) => (base_ticker, tx_bytes),
                Err(err) => {
                    let aborted = Aborted {
                        reason: AbortReason::SomeReason(err.get_inner().to_string()),
                    };
                    return Self::change_state(aborted, state_machine).await;
                },
            };
            let next_state = WaitForLrTxConfirmation0 {
                coin: base_ticker,
                tx_bytes,
            };
            Self::change_state(next_state, state_machine).await
        }
    }

    impl StorableState for RunLrSwap0 {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent { AggTakerSwapEvent::RunLrSwap0 { req: self.req.clone() } }
    }

    impl TransitionFrom<Initialize> for RunLrSwap0 {}

    /// State to wait for confirmation of LR swap tx (before atomic swap)
    pub(super) struct WaitForLrTxConfirmation0 {
        coin: Ticker,
        tx_bytes: BytesJson,
    }

    #[async_trait]
    impl State for WaitForLrTxConfirmation0 {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            if let Err(err) = state_machine
                .wait_for_lr_tx_confirmation(&self.coin, self.tx_bytes.clone())
                .await
            {
                let aborted = Aborted {
                    reason: AbortReason::SomeReason(err.get_inner().to_string()),
                };
                return Self::change_state(aborted, state_machine).await;
            }
            let run_atomic_swap = StartAtomicSwap {};
            Self::change_state(run_atomic_swap, state_machine).await
        }
    }

    impl StorableState for WaitForLrTxConfirmation0 {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::WaitingForLrTxConfirmation0 {
                coin: self.coin.clone(),
                tx_bytes: self.tx_bytes.clone(),
            }
        }
    }

    impl TransitionFrom<RunLrSwap0> for WaitForLrTxConfirmation0 {}

    /// State to create and send a LR swap tx (after atomic swap)
    pub(super) struct RunLrSwap1 {
        pub req: ClassicSwapCreateRequest,
    }

    #[async_trait]
    impl State for RunLrSwap1 {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            let (base_ticker, tx_bytes) = match state_machine.run_lr_swap(&self.req).await {
                Ok((base_ticker, tx_bytes)) => (base_ticker, tx_bytes),
                Err(err) => {
                    let aborted = Aborted {
                        reason: AbortReason::SomeReason(err.get_inner().to_string()),
                    };
                    return Self::change_state(aborted, state_machine).await;
                },
            };
            let wait_for_conf = WaitForLrTxConfirmation1 {
                coin: base_ticker,
                tx_bytes,
            };
            Self::change_state(wait_for_conf, state_machine).await
        }
    }

    impl StorableState for RunLrSwap1 {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent { AggTakerSwapEvent::RunLrSwap1 { req: self.req.clone() } }
    }

    impl TransitionFrom<WaitForAtomicSwap> for RunLrSwap1 {}

    /// State to wait for confirmation of LR swap tx (after atomic swap)
    pub(super) struct WaitForLrTxConfirmation1 {
        coin: Ticker,
        tx_bytes: BytesJson,
    }

    #[async_trait]
    impl State for WaitForLrTxConfirmation1 {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> StateResult<Self::StateMachine> {
            if let Err(err) = state_machine
                .wait_for_lr_tx_confirmation(&self.coin, self.tx_bytes.clone())
                .await
            {
                let aborted = Aborted {
                    reason: AbortReason::SomeReason(err.get_inner().to_string()),
                };
                return Self::change_state(aborted, state_machine).await;
            }
            let completed = Completed {};
            Self::change_state(completed, state_machine).await
        }
    }

    impl StorableState for WaitForLrTxConfirmation1 {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::WaitingForLrTxConfirmation1 {
                coin: self.coin.clone(),
                tx_bytes: self.tx_bytes.clone(),
            }
        }
    }

    impl TransitionFrom<RunLrSwap1> for WaitForLrTxConfirmation1 {}

    /// Aggregated swap completed state
    pub(super) struct Completed {}

    impl StorableState for Completed {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> super::AggTakerSwapEvent { AggTakerSwapEvent::Completed }
    }

    #[async_trait]
    impl LastState for Completed {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> <Self::StateMachine as StateMachineTrait>::Result {
            info!(
                "Aggregated taker swap with LR {} has been completed",
                state_machine.uuid
            );
        }
    }

    impl TransitionFrom<WaitForLrTxConfirmation1> for Completed {}
    impl TransitionFrom<WaitForAtomicSwap> for Completed {}

    /// Represents possible reasons of taker swap being aborted
    /// TODO: add reasons
    #[derive(Clone, Debug, Deserialize, Display, Serialize)]
    pub enum AbortReason {
        SomeReason(String),
    }

    /// Aggregated swap aborted state
    struct Aborted {
        reason: AbortReason,
    }

    #[async_trait]
    impl LastState for Aborted {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> <Self::StateMachine as StateMachineTrait>::Result {
            warn!("Swap {} was aborted with reason {}", state_machine.uuid, self.reason);
        }
    }

    impl StorableState for Aborted {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::Aborted {
                reason: self.reason.clone(),
            }
        }
    }

    impl TransitionFrom<RunLrSwap0> for Aborted {}
    impl TransitionFrom<WaitForLrTxConfirmation0> for Aborted {}
    impl TransitionFrom<RunLrSwap1> for Aborted {}
    impl TransitionFrom<WaitForLrTxConfirmation1> for Aborted {}
    impl TransitionFrom<WaitForAtomicSwap> for Aborted {}
    impl TransitionFrom<StartAtomicSwap> for Aborted {}
}

impl AggTakerSwapStateMachine {
    /// Current agg swap version
    const AGG_SWAP_VERSION: u8 = 0;

    #[allow(clippy::result_large_err)]
    fn sell_buy_method(&self) -> MmResult<TakerAction, LrSwapError> {
        match self.sell_buy_request.method.as_str() {
            "buy" => Ok(TakerAction::Buy),
            "sell" => Ok(TakerAction::Sell),
            _ => MmError::err(LrSwapError::InvalidParam(
                "invalid method in sell/buy request".to_owned(),
            )),
        }
    }

    #[allow(clippy::result_large_err)]
    fn maker_coin(&self) -> MmResult<&str, LrSwapError> {
        match self.sell_buy_method()? {
            TakerAction::Buy => Ok(&self.sell_buy_request.base),
            TakerAction::Sell => Ok(&self.sell_buy_request.rel),
        }
    }

    #[allow(clippy::result_large_err)]
    fn taker_coin(&self) -> MmResult<&str, LrSwapError> {
        match self.sell_buy_method()? {
            TakerAction::Buy => Ok(&self.sell_buy_request.rel),
            TakerAction::Sell => Ok(&self.sell_buy_request.base),
        }
    }

    #[allow(clippy::result_large_err)]
    fn swap_amount(&self) -> MmResult<MmNumber, LrSwapError> {
        match self.sell_buy_method()? {
            TakerAction::Buy => Ok(&self.sell_buy_request.volume * &self.sell_buy_request.price),
            TakerAction::Sell => Ok(self.sell_buy_request.volume.clone()),
        }
    }

    async fn run_lr_swap(&self, req: &ClassicSwapCreateRequest) -> MmResult<(Ticker, BytesJson), LrSwapError> {
        let (base, base_contract) = get_coin_for_one_inch(&self.ctx, &req.base).await?;
        let (_rel, rel_contract) = get_coin_for_one_inch(&self.ctx, &req.rel).await?;
        info!(
            "Taker swap with LR: starting liquidity routing step for {}/{} for amount {}",
            req.base,
            req.rel,
            req.amount.to_decimal()
        );
        let sell_amount = wei_from_big_decimal(&req.amount.to_decimal(), base.decimals())
            .mm_err(|err| LrSwapError::InvalidParam(err.to_string()))?;
        let single_address = base.derivation_method().single_addr_or_err().await?;

        let query_params = ClassicSwapCreateParams::new(
            base_contract.display_address(),
            rel_contract.display_address(),
            sell_amount.to_string(),
            single_address.display_address(),
            req.slippage,
        )
        .build_query_params()?; // TODO: add more query params from req

        let url =
            SwapUrlBuilder::create_api_url_builder(&self.ctx, base.chain_id(), SwapApiMethods::ClassicSwapCreate)?
                .with_query_params(query_params)
                .build()?;
        let swap_with_tx: ClassicSwapData = ApiClient::call_api(url).await?;
        let tx_fields = swap_with_tx
            .tx
            .ok_or(LrSwapError::InternalError("TxFields empty".to_string()))?;

        let sign_params = SignRawTransactionEnum::ETH(SignEthTransactionParams {
            value: Some(u256_to_big_decimal(
                U256::from_dec_str(&tx_fields.value)?,
                base.decimals(),
            )?),
            to: Some(tx_fields.to.display_address()),
            data: Some(tx_fields.data),
            gas_limit: U256::from(tx_fields.gas),
            pay_for_gas: None, // TODO: use gas price from tx_fields? Maybe we can use our gas_price
        });

        // TODO: maybe add another sign and send tx impl in trading_api?
        // actually I use sign_raw_tx instead of eth.rs's sign_and_send_transaction to avoid bringing eth types here
        // TODO: refactor: use SignRawTransactionEnum as the param instead of SignRawTransactionRequest (coin unneeded)
        let tx_bytes = base
            .sign_raw_tx(&SignRawTransactionRequest {
                coin: base.ticker().to_owned(),
                tx: sign_params,
            })
            .await?;
        let txid = base
            .send_raw_tx_bytes(&tx_bytes.tx_hex)
            .compat()
            .await
            .map_to_mm(LrSwapError::TransactionError)?;
        info!("Taker swap with LR: liquidity routing tx {} sent okay", txid);
        Ok((req.base.clone(), tx_bytes.tx_hex))
    }

    async fn start_lp_auto_buy(&self) -> MmResult<SellBuyResponse, LrSwapError> {
        let base_coin = lp_coinfind_or_err(&self.ctx, &self.sell_buy_request.base).await?;
        let rel_coin = lp_coinfind_or_err(&self.ctx, &self.sell_buy_request.rel).await?;
        if base_coin.wallet_only(&self.ctx) {
            return MmError::err(LrSwapError::InvalidParam(format!(
                "Base coin {} is wallet only",
                self.sell_buy_request.base
            )));
        }
        if rel_coin.wallet_only(&self.ctx) {
            return MmError::err(LrSwapError::InvalidParam(format!(
                "Rel coin {} is wallet only",
                self.sell_buy_request.rel
            )));
        }
        let my_amount = self.swap_amount()?;
        let base_ticker = self.maker_coin()?;
        let rel_ticker = self.taker_coin()?;

        println!(
            "Checking balance {} for atomic swap {}/{} for agg swap uuid: {}",
            my_amount.to_decimal(),
            base_ticker,
            rel_ticker,
            self.uuid
        );
        check_balance_for_taker_swap(
            &self.ctx,
            rel_coin.deref(),
            base_coin.deref(),
            my_amount.clone(),
            None,
            None,
            FeeApproxStage::OrderIssue,
        )
        .await
        .mm_err(|_| LrSwapError::InternalError("insufficient balance".to_string()))?;
        info!(
            "Taker swap with LR: starting atomic swap {}/{} for amount {}",
            base_ticker,
            rel_ticker,
            my_amount.to_decimal()
        );
        let res_bytes = lp_auto_buy(&self.ctx, &base_coin, &rel_coin, self.sell_buy_request.clone())
            .await
            .map_to_mm(LrSwapError::InternalError)?;
        let rpc_res: Mm2RpcResult<SellBuyResponse> = serde_json::from_slice(res_bytes.as_slice())?;
        Ok(rpc_res.result)
    }

    #[allow(clippy::result_large_err)]
    fn check_if_status_finished(swap_result: &MmResult<SwapRpcData, MySwapStatusError>) -> MmResult<bool, LrSwapError> {
        let swap_status = match swap_result {
            Ok(swap_status) => swap_status,
            Err(mm_err) => {
                match mm_err.get_inner() {
                    // TODO: now considering that swap has not been started yet and we don't have non-existing uuids,
                    // but maybe we could throw an error after some time
                    MySwapStatusError::NoSwapWithUuid(_) => return Ok(false),
                    other_err => {
                        return MmError::err(LrSwapError::InternalError(format!(
                            "Failed to get swap status: {}",
                            other_err
                        )))
                    },
                }
            },
        };
        match swap_status {
            SwapRpcData::TakerV1(swap_status) => Ok(swap_status.is_finished()),
            SwapRpcData::TakerV2(swap_status) => Ok(swap_status.is_finished),
            SwapRpcData::AggTaker(_) | SwapRpcData::MakerV1(_) | SwapRpcData::MakerV2(_) => {
                MmError::err(LrSwapError::InternalError("incorrect atomic swap type".to_string()))
            },
        }
    }

    async fn wait_for_atomic_swap_finished(&self) -> MmResult<(), LrSwapError> {
        let atomic_swap_uuid = self
            .atomic_swap_uuid
            .get()
            .ok_or(LrSwapError::InternalError("atomic swap uuid not set".to_string()))?;
        info!(
            "Taker swap with LR: waiting for atomic swap uuid {} to finish",
            atomic_swap_uuid
        );
        loop {
            let swap_result = my_swap_status_rpc(self.ctx.clone(), MySwapStatusRequest {
                uuid: *atomic_swap_uuid,
            })
            .await;
            if Self::check_if_status_finished(&swap_result)? {
                break;
            }
            Timer::sleep(5.).await;
        }
        info!("Taker swap with LR: atomic swap finished");
        Ok(())
    }

    async fn wait_for_lr_tx_confirmation(&self, coin: &Ticker, tx_bytes: BytesJson) -> MmResult<(), LrSwapError> {
        match lp_coinfind_or_err(&self.ctx, coin).await? {
            MmCoinEnum::EthCoin(eth_coin) => {
                info!("Taker swap with LR: waiting for liquidity routing tx to confirm");
                let confirm_lr_swap_input = ConfirmPaymentInput {
                    payment_tx: tx_bytes.0,
                    confirmations: LR_SWAP_CONFIRMATIONS,
                    requires_nota: false,
                    wait_until: now_sec() + LR_SWAP_WAIT_CONFIRM_TIMEOUT_SEC,
                    check_every: LR_SWAP_WAIT_CONFIRM_INTERVAL_SEC,
                };
                let _ = eth_coin
                    .wait_for_confirmations(confirm_lr_swap_input)
                    .compat()
                    .await
                    .map_to_mm(LrSwapError::TransactionError)?;
                info!("Taker swap with LR: liquidity routing tx confirmed");
                Ok(())
            },
            _ => MmError::err(LrSwapError::CoinTypeError),
        }
    }
}

#[allow(unused)]
async fn check_balance_for_agg_taker_swap(
    ctx: &MmArc,
    lr_swap_0: &Option<ClassicSwapCreateRequest>,
    sell_buy_request: &SellBuyRequest,
) -> MmResult<(), LrSwapError> {
    // TODO: add checking balance for aggregated swap
    // let base_coin = lp_coinfind_or_err(ctx, &sell_buy_request.base).await?;
    // let rel_coin = lp_coinfind_or_err(ctx, &sell_buy_request.rel).await?;
    Ok(())
}

async fn start_agg_taker_swap_state_machine(
    ctx: &MmArc,
    lr_swap_0: Option<ClassicSwapCreateRequest>,
    lr_swap_1: Option<ClassicSwapCreateRequest>,
    sell_buy_request: SellBuyRequest,
    uuid: Uuid,
) {
    let mut state_machine = AggTakerSwapStateMachine {
        storage: AggTakerSwapStorage::new(ctx.clone()),
        abortable_system: ctx
            .abortable_system
            .create_subsystem()
            .expect("create_subsystem should not fail"),
        ctx: ctx.clone(),
        lr_swap_0,
        lr_swap_1,
        sell_buy_request,
        uuid,
        started_at: now_sec(),
        swap_version: AggTakerSwapStateMachine::AGG_SWAP_VERSION,
        atomic_swap_uuid: OnceLock::default(),
    };
    #[allow(clippy::box_default)]
    state_machine
        .run(Box::new(states::Initialize::default()))
        .await
        .error_log();
}

pub(crate) async fn lp_start_agg_taker_swap(
    ctx: MmArc,
    lr_swap_0: Option<ClassicSwapCreateRequest>,
    lr_swap_1: Option<ClassicSwapCreateRequest>,
    sell_buy_request: SellBuyRequest,
) -> MmResult<Uuid, LrSwapError> {
    let spawner = ctx.spawner();
    let uuid = new_uuid(); // For a aggregated swap we need a new uuid, different from the atomic swap uuid, to distinguish the aggregated swap as dedicated in rpcs, statuses etc)

    check_balance_for_agg_taker_swap(&ctx, &lr_swap_0, &sell_buy_request)
        .await
        .mm_err(|_| LrSwapError::InternalError("insufficient balance".to_string()))?; // TODO: add specific err

    let fut = async move {
        println!("Entering the aggregated taker swap with LR uuid: {}", uuid);
        start_agg_taker_swap_state_machine(&ctx, lr_swap_0, lr_swap_1, sell_buy_request, uuid).await;
    };

    let settings = AbortSettings::info_on_abort(format!("swap {uuid} stopped!"));
    spawner.spawn_with_settings(fut, settings);
    Ok(uuid)
}
