//! Types for LR swaps rpc

use crate::lp_ordermatch::RpcOrderbookEntryV2;
use crate::rpc::lp_commands::ext_api::ext_api_errors::ExtApiRpcError;
use crate::rpc::lp_commands::ext_api::ext_api_types::{ClassicSwapCreateOptParams, ClassicSwapDetails};
use coins::Ticker;
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::{MatchBy, OrderType};
use uuid::Uuid;

/// Request to find best swap path with LR to fill an order from list.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LrBestQuoteRequest {
    /// Order base coin ticker (from the orderbook).
    pub base: Ticker,
    /// Source amount in base coins to sell (with fraction)
    pub amount: MmNumber,
    /// List of maker ask orders, to find best swap path with LR
    pub asks: Vec<RpcOrderbookEntryV2>,
    // TODO: impl later
    // /// List of maker bid orders, to find best swap path with LR
    // pub bids: Vec<RpcOrderbookEntryV2>,
    /// User token to fill order with LR
    pub my_token: Ticker,
}

/// Response for find best swap path with LR
#[derive(Debug, Serialize)]
pub struct LrBestQuoteResponse {
    /// Swap tx data (from 1inch quote)
    pub lr_swap_details: ClassicSwapDetails,
    /// found best order which can be filled with LR swap
    pub best_order: RpcOrderbookEntryV2,
    /// base/rel price including the price of the LR swap part
    pub total_price: MmNumber, // TODO: add as BigDecimal and Rational like other prices
                               // /// Fees to pay, including LR swap fee
                               // pub trade_fee: TradePreimageResponse, // TODO: implement when trade_preimage implemented for TPU
}

/// Request to get quotes with possible swap paths to fill order with multiple tokens with LR
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LrQuotesForTokensRequest {
    /// Order base coin ticker (from the orderbook).
    pub base: Ticker,
    /// Swap amount in base coins to sell (with fraction)
    pub amount: MmNumber,
    /// Maker order to find possible swap path with LR
    pub orderbook_entry: RpcOrderbookEntryV2,
    /// List of user tokens to trade with LR
    pub my_tokens: Vec<Ticker>,
}

/// Details with swap with LR
#[derive(Debug, Serialize)]
pub struct QuotesDetails {
    /// interim token to route to/from
    pub dest_token: Ticker,
    /// Swap tx data (from 1inch quote)
    pub lr_swap_details: ClassicSwapDetails,
    /// total swap price with LR
    pub total_price: MmNumber,
    // /// Fees to pay, including LR swap fee
    // pub trade_fee: TradePreimageResponse, // TODO: implement when trade_preimage implemented for TPU
}

/// Response for quotes to fill order with LR
#[derive(Debug, Serialize)]
pub struct LrQuotesForTokensResponse {
    pub quotes: Vec<QuotesDetails>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LrSwapRpcParams {
    pub slippage: f32,
    pub swap_details: ClassicSwapDetails,
    pub opt_params: Option<ClassicSwapCreateOptParams>,
}

impl LrSwapRpcParams {
    pub fn get_source_token(&self) -> Result<Ticker, ExtApiRpcError> {
        Ok(self
            .swap_details
            .src_token
            .as_ref()
            .ok_or(ExtApiRpcError::NoLrTokenInfo)?
            .symbol_kdf
            .as_ref()
            .ok_or(ExtApiRpcError::NoLrTokenInfo)?
            .clone())
    }
    pub fn get_destination_token(&self) -> Result<Ticker, ExtApiRpcError> {
        Ok(self
            .swap_details
            .dst_token
            .as_ref()
            .ok_or(ExtApiRpcError::NoLrTokenInfo)?
            .symbol_kdf
            .as_ref()
            .ok_or(ExtApiRpcError::NoLrTokenInfo)?
            .clone())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AtomicSwapRpcParams {
    pub volume: Option<MmNumber>,
    pub base: Ticker,
    pub rel: Ticker,
    pub price: MmNumber,
    pub method: String,
    #[serde(default)]
    pub match_by: MatchBy,
    #[serde(default)]
    pub order_type: OrderType,
    // TODO: add opt params
}

/// Request to fill a maker order with atomic swap with LR steps
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LrFillMakerOrderRequest {
    /// Sell or buy params for the atomic swap step
    pub atomic_swap: AtomicSwapRpcParams,

    /// Params to create 1inch LR swap (from 1inch quote)
    /// TODO: make this an enum to allow other LR providers
    //#[serde(skip_serializing_if = "Option::is_none")]
    pub lr_swap_0: Option<LrSwapRpcParams>,

    /// Params to create 1inch LR swap (from 1inch quote)
    //#[serde(skip_serializing_if = "Option::is_none")]
    pub lr_swap_1: Option<LrSwapRpcParams>,
}

/// Response to sell or buy order with LR
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LrFillMakerOrderResponse {
    /// Created aggregated swap uuid for tracking the swap
    pub uuid: Uuid,
}
