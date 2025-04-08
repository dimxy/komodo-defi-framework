//! State machine for taker aggregated swap: liquidity routing swap (opt) + atomic swap + liquidity routing swap (opt)

#![allow(unused)] // TODO: remove

use async_trait::async_trait;
use crate::lp_swap::CheckBalanceResult;
use coins::{lp_coinfind_or_err, FeeApproxStage, MarketCoinOps, SignEthTransactionParams, SignRawTransactionEnum, SignRawTransactionRequest, Ticker};
use coins::eth::eth_addr_to_hex;
use coins::eth::display_eth_address;
use coins::eth::{u256_to_big_decimal, wei_from_big_decimal};
use ethereum_types::{Address as EthAddress, H256, U256};
use coins::MmCoin;
use coins::CoinWithDerivationMethod;
//use coins::{AddrToString, CanRefundHtlc, ConfirmPaymentInput, DexFee, FeeApproxStage, FundingTxSpend, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs, MakerCoinSwapOpsV2, MmCoin, ParseCoinAssocTypes, RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs, SearchForFundingSpendErr, SendMakerPaymentArgs, SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, Ticker, ToBytes, TradePreimageValue, Transaction, TxPreimageWithSig, ValidateTakerFundingArgs};
use common::executor::abortable_queue::AbortableQueue;
use common::executor::AbortableSystem;
use common::log::{info, warn};
use common::executor::AbortSettings;
use futures::compat::Future01CompatExt;
use enum_derives::EnumFromStringify;
//use common::{now_sec, Future01CompatExt, DEX_FEE_ADDR_RAW_PUBKEY};
use crate::lp_swap::check_balance_for_taker_swap;
use crate::lp_swap::swap_lock::SwapLock;
use crate::lp_swap::swap_v2_common::*;
use crate::lp_ordermatch::lp_auto_buy;
use crate::rpc::lp_commands::one_inch::errors::ApiIntegrationRpcError;
use crate::rpc::lp_commands::one_inch::types::{ClassicSwapCreateRequest, ClassicSwapDetails};
use crate::rpc::lp_commands::one_inch::rpcs::get_coin_for_one_inch;
use crate::rpc::lp_commands::lr_swap::lr_types::LrFillMakerOrderResponse;
//use ethcore_transaction::Action;
//use crate::lp_swap::SwapsContext;
//use crypto::privkey::SerializableSecp256k1Keypair;
//use crypto::secret_hash_algo::SecretHashAlgo;
#[allow(unused_imports)]
use mm2_rpc::data::legacy::{MatchBy, Mm2RpcResult, OrderConfirmationsSettings, OrderType, RpcOrderbookEntry,
    SellBuyRequest, SellBuyResponse, TakerAction, TakerRequestForRpc};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
//use mm2_number::MmNumber;
use mm2_state_machine::prelude::*;
use mm2_state_machine::storable_state_machine::*;
use trading_api::one_inch_api::classic_swap_types::{TxFields, ClassicSwapData, ClassicSwapQuoteParams, ClassicSwapCreateParams};
use trading_api::one_inch_api::client::ApiClient;
use serde_json::{self as json, Value as Json};
use std::convert::TryInto;
use std::ops::Deref;
use std::str::FromStr;
use common::{new_uuid, now_sec};
use crate::common::log::LogOnError;
use crate::common::executor::SpawnAbortable;

use crate::database::my_lr_swaps::SELECT_LR_SWAP_BY_UUID;

//use primitives::hash::H256;
//use rpc::v1::types::{Bytes as BytesJson, H256 as H256Json};
use uuid::Uuid;

cfg_native!(
    use common::async_blocking;
    use db_common::sqlite::rusqlite::{named_params, Error as SqlError, Result as SqlResult, Row};
    use db_common::sqlite::rusqlite::types::Type as SqlType;
);

cfg_wasm32!(
    use crate::lp_swap::swap_wasm_db::{MySwapsFiltersTable, SavedSwapTable};
    use crate::swap_versioning::legacy_swap_version;
);

/* 
#[derive(Debug, Display, EnumFromStringify)]
pub enum ApiIntegrationRpcError {
    #[from_stringify("coins::CoinFindError")]
    NoSuchCoin(String),
    #[from_stringify("serde_json::Error")]
    ParseError(String),
    InternalError(String),
}*/



/// Represents events produced by aggregated taker swap states.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event_type", content = "event_data")]
pub enum AggTakerSwapEvent {
    /// Atomic swap has been successfully started
    StartAtomicSwap {
       
    },
    /// Run LR-swap before atomic swap and get its result
    RunLrSwap0 {
        
    },
    /// Waiting for running atomic swap
    WaitingForAtomicSwap {
        
    },
    /// Run LR-swap after atomic swap and get its result
    RunLrSwap1 {
        
    },
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
        use crate::{database::my_lr_swaps::insert_new_lr_swap, lp_swap::TAKER_SWAP_AGGREGATED_TYPE};

        let ctx = self.ctx.clone();

        async_blocking(move || {
            let sql_params = named_params! {
                ":my_coin": repr.taker_coin,
                ":other_coin": repr.maker_coin,
                ":uuid": repr.uuid.to_string(),
                ":started_at": repr.started_at,
                ":swap_type": TAKER_SWAP_AGGREGATED_TYPE,
                ":swap_version": repr.swap_version,
            };
            insert_new_lr_swap(&ctx, sql_params)?;
            Ok(())
        })
        .await
    }

    #[cfg(target_arch = "wasm32")]
    async fn store_repr(&mut self, uuid: Self::MachineId, _repr: Self::DbRepr) -> Result<(), Self::Error> {
        let swaps_ctx = SwapsContext::from_ctx(&self.ctx).expect("SwapsContext::from_ctx should not fail");
        let db = swaps_ctx.swap_db().await?;
        let transaction = db.transaction().await?;
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

    async fn has_record_for(&mut self, _id: &Self::MachineId) -> Result<bool, Self::Error> {
        //has_db_record_for(self.ctx.clone(), id).await
        Ok(false)
    }

    async fn store_event(&mut self, _id: Self::MachineId, event: AggTakerSwapEvent) -> Result<(), Self::Error> {
        //store_swap_event::<AggTakerSwapDbRepr>(self.ctx.clone(), id, event).await
        Ok(())
    }

    async fn get_unfinished(&self) -> Result<Vec<Self::MachineId>, Self::Error> {
        //get_unfinished_swaps_uuids(self.ctx.clone(), MAKER_SWAP_V2_TYPE).await
        Ok(vec![])
    }

    async fn mark_finished(&mut self, _id: Self::MachineId) -> Result<(), Self::Error> {
        //mark_swap_as_finished(self.ctx.clone(), id).await
        Ok(())
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

/*impl GetSwapCoins for AggTakerSwapDbRepr {
    fn maker_coin(&self) -> &str { &self.maker_coin }

    fn taker_coin(&self) -> &str { &self.taker_coin }
}*/

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
            events: json::from_str(&row.get::<_, String>(7)?)
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
    /// unique ID of the agg swap for stopping or querying status.
    pub uuid: Uuid,
    /// The timestamp when the agg swap was started.
    pub started_at: u64,
    /// version
    pub swap_version: u8,
}

#[async_trait]
impl StorableStateMachine for AggTakerSwapStateMachine
{
    type Storage = AggTakerSwapStorage;
    type Result = ();
    type Error = MmError<SwapStateMachineError>;
    type ReentrancyLock = SwapLock;
    type RecreateCtx = ();
    type RecreateError = MmError<SwapRecreateError>;

    fn to_db_repr(&self) -> AggTakerSwapDbRepr {
        AggTakerSwapDbRepr {
            maker_coin: self.maker_coin().unwrap_or_default().to_owned(),
            started_at: self.started_at,
            taker_coin: self.taker_coin().unwrap_or_default().to_owned(),
            uuid: self.uuid,
            events: Vec::new(),
            swap_version: self.swap_version,        
        }
    }

    fn storage(&mut self) -> &mut Self::Storage { &mut self.storage }

    fn id(&self) -> <Self::Storage as StateMachineStorage>::MachineId { Uuid::new_v4() }

    async fn recreate_machine(
        uuid: Uuid,
        storage: AggTakerSwapStorage,
        mut repr: AggTakerSwapDbRepr,
        _recreate_ctx: Self::RecreateCtx,
    ) -> Result<(RestoredMachine<Self>, Box<dyn RestoredState<StateMachine = Self>>), Self::RecreateError> {
        let current_state: Box<dyn RestoredState<StateMachine = Self>> = match repr.events
            .pop()
            .ok_or(MmError::new(SwapRecreateError::ReprEventsEmpty))?
        {
            AggTakerSwapEvent::StartAtomicSwap {} => Box::new(states::StartAtomicSwap { }),
            AggTakerSwapEvent::RunLrSwap0 {} => Box::new(states::RunLrSwap0 { }),
            AggTakerSwapEvent::RunLrSwap1 {} => Box::new(states::RunLrSwap1 { }),
            
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
        /*let swap_info = ActiveSwapV2Info {
            uuid: self.uuid,
            maker_coin: self.maker_coin.ticker().into(),
            taker_coin: self.taker_coin.ticker().into(),
            swap_type: MAKER_SWAP_V2_TYPE,
        };
        init_additional_context_impl(&self.ctx, swap_info, self.taker_p2p_pubkey);*/
    }

    fn clean_up_context(&mut self) {
    }

    fn on_event(&mut self, event: &AggTakerSwapEvent) {
        match event {
            AggTakerSwapEvent::StartAtomicSwap { } => {
                /*let swaps_ctx = SwapsContext::from_ctx(&self.ctx).expect("from_ctx should not fail at this point");                
                swaps_ctx
                    .locked_amounts
                    .lock()
                    .unwrap()
                    .entry(maker_coin_ticker)
                    .or_insert_with(Vec::new)
                    .push(new_locked);*/
            },
            AggTakerSwapEvent::Aborted { .. }
            | AggTakerSwapEvent::Completed => (),
            _ => (),
        }
        // Send a notification to the swap status streamer about a new event.
        /*self.ctx
            .event_stream_manager
            .send_fn(SwapStatusStreamer::derive_streamer_id(), || SwapStatusEvent::MakerV2 {
                uuid: self.uuid,
                event: event.clone(),
            })
            .ok();*/
    }

    fn on_kickstart_event(&mut self, _event: AggTakerSwapEvent) {
        
    }
}

mod states {
    use super::*;

    /// Represents a state used to start a new aggregated taker swap.
    pub struct Initialize {}

    impl Default for Initialize {
        fn default() -> Self {
            Initialize {}
        }
    }

    impl InitialState for Initialize
    {
        type StateMachine = AggTakerSwapStateMachine;
    }

    #[async_trait]
    impl State for Initialize
    {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(self: Box<Self>, state_machine: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
            info!("Aggregated taker swap {} starting...", state_machine.uuid);
            if state_machine.lr_swap_0.is_some() {
                let run_lr_swap_0 = RunLrSwap0 {};
                Self::change_state(run_lr_swap_0, state_machine).await
            } else {
                let run_atomic_swap = StartAtomicSwap {};
                Self::change_state(run_atomic_swap, state_machine).await
            }
        }
    }

    pub(super) struct StartAtomicSwap {}

    #[async_trait]
    impl State for StartAtomicSwap
    {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(self: Box<Self>, state_machine: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {

            if let Err(err) = state_machine.start_lp_auto_buy().await {
                let aborted = Aborted { reason: AbortReason::SomeReason(err.to_string()) };
                return Self::change_state(aborted, state_machine).await
            }
            let wait_for_atomic_swap = WaitForAtomicSwap {};
            Self::change_state(wait_for_atomic_swap, state_machine).await
        }
    }

    impl StorableState for StartAtomicSwap
    {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::StartAtomicSwap { }
        }
    }

    impl TransitionFrom<Initialize> for StartAtomicSwap {}
    impl TransitionFrom<RunLrSwap0> for StartAtomicSwap {}

    pub(super) struct WaitForAtomicSwap {}

    #[async_trait]
    impl State for WaitForAtomicSwap
    {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(self: Box<Self>, state_machine: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {

            if state_machine.lr_swap_1.is_some() {
                let run_lr_swap_1 = RunLrSwap1 {};
                Self::change_state(run_lr_swap_1, state_machine).await
            } else {
                let completed = Completed {};
                Self::change_state(completed, state_machine).await
            }
        }
    }

    impl StorableState for WaitForAtomicSwap
    {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::WaitingForAtomicSwap { }
        }
    }

    impl TransitionFrom<StartAtomicSwap> for WaitForAtomicSwap {}

    pub(super) struct RunLrSwap0 {}

    #[async_trait]
    impl State for RunLrSwap0
    {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(self: Box<Self>, state_machine: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
            let start_atomic_swap = StartAtomicSwap {};
            Self::change_state(start_atomic_swap, state_machine).await
        }
    }

    impl StorableState for RunLrSwap0
    {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::RunLrSwap0 { }
        }
    }

    impl TransitionFrom<Initialize> for RunLrSwap0 {}

    pub(super) struct RunLrSwap1 {}

    #[async_trait]
    impl State for RunLrSwap1
    {
        type StateMachine = AggTakerSwapStateMachine;

        async fn on_changed(self: Box<Self>, state_machine: &mut Self::StateMachine) -> StateResult<Self::StateMachine> {
            let completed = Completed {};
            Self::change_state(completed, state_machine).await
        }
    }

    impl StorableState for RunLrSwap1
    {
        type StateMachine = AggTakerSwapStateMachine;

        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::RunLrSwap1 { }
        }
    }

    impl TransitionFrom<WaitForAtomicSwap> for RunLrSwap1 {}

    pub(super) struct Completed {}
    
    impl Completed {
        fn new() -> Completed {
            Completed {}
        }
    }
    
    impl StorableState for Completed
    {
        type StateMachine = AggTakerSwapStateMachine;
    
        fn get_event(&self) -> super::AggTakerSwapEvent { AggTakerSwapEvent::Completed }
    }
    
    #[async_trait]
    impl LastState for Completed
    {
        type StateMachine = AggTakerSwapStateMachine;
    
        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> <Self::StateMachine as StateMachineTrait>::Result {
            info!("Aggregated Taker Swap {} has been completed", state_machine.uuid);
        }
    }
    
    impl TransitionFrom<RunLrSwap1> for Completed {}
    impl TransitionFrom<WaitForAtomicSwap> for Completed {}

    /// Represents possible reasons of taker swap being aborted
    #[derive(Clone, Debug, Deserialize, Display, Serialize)]
    pub enum AbortReason {
        SomeReason(String),
    }

    struct Aborted {
        reason: AbortReason,
    }
    
    impl Aborted {
        fn new(reason: AbortReason) -> Aborted {
            Aborted {
                reason,
            }
        }
    }
    
    #[async_trait]
    impl LastState for Aborted
    {
        type StateMachine = AggTakerSwapStateMachine;
    
        async fn on_changed(
            self: Box<Self>,
            state_machine: &mut Self::StateMachine,
        ) -> <Self::StateMachine as StateMachineTrait>::Result {
            warn!("Swap {} was aborted with reason {}", state_machine.uuid, self.reason);
        }
    }
    
    impl StorableState for Aborted
    {
        type StateMachine = AggTakerSwapStateMachine;
    
        fn get_event(&self) -> AggTakerSwapEvent {
            AggTakerSwapEvent::Aborted {
                reason: self.reason.clone(),
            }
        }
    }
    
    impl TransitionFrom<RunLrSwap0> for Aborted {}
    impl TransitionFrom<RunLrSwap1> for Aborted {}
    impl TransitionFrom<WaitForAtomicSwap> for Aborted {}
    impl TransitionFrom<StartAtomicSwap> for Aborted {}
}

impl AggTakerSwapStateMachine {

    /// Current agg swap version
    const AGG_SWAP_VERSION: u8 = 0;

    fn sell_buy_method(&self) -> MmResult<TakerAction, ApiIntegrationRpcError> {
        json::from_str(self.sell_buy_request.method.as_str())
            .map_to_mm(|_| ApiIntegrationRpcError::InvalidParam("invalid method in sell/buy request".to_owned()))
    }

    fn maker_coin(&self) -> MmResult<&str, ApiIntegrationRpcError> {
        match self.sell_buy_method()? {
            TakerAction::Buy => Ok(&self.sell_buy_request.base),
            TakerAction::Sell => Ok(&self.sell_buy_request.rel),
        }
    }

    fn taker_coin(&self) -> MmResult<&str, ApiIntegrationRpcError> {
        match self.sell_buy_method()? {
            TakerAction::Buy => Ok(&self.sell_buy_request.rel),
            TakerAction::Sell => Ok(&self.sell_buy_request.base),
        }
    }

    async fn run_lr_swap(&self, req: &ClassicSwapCreateRequest) -> MmResult<(), ApiIntegrationRpcError> {
        let (base, base_contract) = get_coin_for_one_inch(&self.ctx, &req.base).await?;
        let (rel, rel_contract) = get_coin_for_one_inch(&self.ctx, &req.rel).await?;
        let sell_amount = wei_from_big_decimal(&req.amount.to_decimal(), base.decimals())
            .mm_err(|err| ApiIntegrationRpcError::InvalidParam(err.to_string()))?;
        let single_address = base.derivation_method().single_addr_or_err().await?;

        let query_params = ClassicSwapCreateParams::new(
                base_contract,
                rel_contract,
                sell_amount.to_string(),
                display_eth_address(&single_address),
                req.slippage,
            )
            // TODO: add more quuery params from req
            .build_query_params()
            .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(base.decimals())))?;
        let swap_with_tx: ClassicSwapData = ApiClient::new(&self.ctx)
            .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(base.decimals())))?
            .call_one_inch_api(
                Some(base.chain_id()),
                ApiClient::classic_swap_endpoint(),
                ApiClient::swap_method().to_owned(),
                Some(query_params),
            )
            .await
            .mm_err(|api_err| ApiIntegrationRpcError::InternalError(api_err.to_string()))?; // use 'base' as amount in errors is in the src coin
    
        let tx_fields = swap_with_tx.tx.ok_or(ApiIntegrationRpcError::InternalError("TxFields empty".to_string()))?;
        /*let tx = base.sign_and_send_transaction(
            U256::from_str(&tx_fields.value)?, 
            Action::Call(tx_fields.to), 
            hex::decode(tx_fields.data)?, 
            U256::from(tx_fields.gas)
        ).compat().await?;*/

        let sign_params = SignRawTransactionEnum::ETH(SignEthTransactionParams {
            value: Some(u256_to_big_decimal(U256::from_dec_str(&tx_fields.value)?, base.decimals())?),
            to: Some(eth_addr_to_hex(&tx_fields.to)),
            data: Some(tx_fields.data),
            gas_limit: U256::from(tx_fields.gas),
            pay_for_gas: None, // TODO: use gas price from tx_fields? Maybe we can use our gas_price
        });

        // TODO: maybe add another sign and send tx impl in trading_api? 
        // actually I use sign_raw_tx instead of eth.rs's sign_and_send_transaction to avoid bringing eth types here
        // TODO: refactor: use SignRawTransactionEnum as the param instead of SignRawTransactionRequest (coin unneeded)
        let tx_bytes = base.sign_raw_tx(&SignRawTransactionRequest {
            coin: base.ticker().to_owned(),
            tx: sign_params
        }).await?;
        Ok(())
    }

    async fn start_lp_auto_buy(&self) -> MmResult<SellBuyResponse, ApiIntegrationRpcError> {
        let rel_coin = lp_coinfind_or_err(&self.ctx, &self.sell_buy_request.rel).await?;
        let base_coin = lp_coinfind_or_err(&self.ctx, &self.sell_buy_request.base).await?;
        if base_coin.wallet_only(&self.ctx) {
            return MmError::err(ApiIntegrationRpcError::InvalidParam(format!("Base coin {} is wallet only", self.sell_buy_request.base)));
        }
        if rel_coin.wallet_only(&self.ctx) {
            return MmError::err(ApiIntegrationRpcError::InvalidParam(format!("Base coin {} is wallet only", self.sell_buy_request.rel)));
        }
        let my_amount = match self.sell_buy_method()? {
            TakerAction::Buy => &self.sell_buy_request.volume * &self.sell_buy_request.price,
            TakerAction::Sell => self.sell_buy_request.volume.clone(),
        };
        check_balance_for_taker_swap(
                &self.ctx,
                rel_coin.deref(),
                base_coin.deref(),
                my_amount,
                None,
                None,
                FeeApproxStage::OrderIssue
            )
            .await
            .mm_err(|_| ApiIntegrationRpcError::InternalError("insufficient balance".to_string()))?;
        let res_bytes = lp_auto_buy(&self.ctx, &base_coin, &rel_coin, self.sell_buy_request.clone())
            .await
            .map_err(|err| MmError::new(ApiIntegrationRpcError::InternalError(err)))?;
        let res: SellBuyResponse = json::from_slice(res_bytes.as_slice())?;
        Ok(res)
    }
}

async fn check_balance_for_agg_taker_swap(
    ctx: &MmArc,
    lr_swap_0: &Option<ClassicSwapCreateRequest>,
    sell_buy_request: &SellBuyRequest,
) -> MmResult<(), ApiIntegrationRpcError> {
    //let base_coin = lp_coinfind_or_err(ctx, &sell_buy_request.base).await?;
    //let rel_coin = lp_coinfind_or_err(ctx, &sell_buy_request.rel).await?;
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
) -> MmResult<Uuid, ApiIntegrationRpcError> {
    let spawner = ctx.spawner();
    let uuid = new_uuid(); // For a aggregated swap we need a new uuid, different from the atomic swap uuid, to distinguish the aggregated swap as dedicated in rpcs, statuses etc)

    check_balance_for_agg_taker_swap(&ctx, &lr_swap_0, &sell_buy_request)
        .await
        .mm_err(|_| ApiIntegrationRpcError::InternalError("insufficient balance".to_string()))?; // TODO: add specific err

    let fut = async move {
        log_tag!(
            ctx,
            "";
            fmt = "Entering the aggregated_taker_swap_loop {}/{} with uuid: {}",
            sell_buy_request.base,
            sell_buy_request.rel,
            uuid
        );
        start_agg_taker_swap_state_machine(&ctx, lr_swap_0, lr_swap_1, sell_buy_request, uuid).await;
    };

    let settings = AbortSettings::info_on_abort(format!("swap {uuid} stopped!"));
    spawner.spawn_with_settings(fut, settings);
    Ok(uuid)
}