//? Types for LR swaps rpc

use std::collections::HashMap;
use mm2_number::MmNumber;
use mm2_number::BigDecimal;
use crate::lp_ordermatch::AggregatedOrderbookEntryV2;
use crate::rpc::lp_commands::one_inch::types::ClassicSwapDetails;
use crate::lp_swap::TradePreimageResponse;
use mm2_rpc::data::legacy::{SellBuyRequest, SellBuyResponse};
//use trading_api::one_inch_api::types::ClassicSwapData;
//use coins::TradeFee;

/// Request to find best swap path with LR for multiple orders.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindBestLrSwapForMultipleOrdersRequest {
    /// Order base coin ticker.
    pub base: String,
    /// Swap amount in base coins to sell (with fraction)
    pub amount: MmNumber,
    /// List of maker orders with rel tokens and order data, to find best swap path with LR
    pub orderbook_entries: HashMap<String, AggregatedOrderbookEntryV2>,
    /// User token to trade with LR 
    pub my_token: String,
}

/// Request to find best swap path with LR rpc for multiple tokens.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindBestLrSwapForMultipleTokensRequest {
    /// Order base coin ticker.
    pub base: String,
    /// Swap amount in base coins to sell (with fraction)
    pub amount: MmNumber,
    /// Maker order's rel token and order data, to find best swap path with LR
    pub orderbook_entry: (String, AggregatedOrderbookEntryV2),
    /// List of user tokens to trade with LR 
    pub my_tokens: Vec<String>,
}


/// Response for find best swap path with LR rpc
#[derive(Serialize)]
pub struct FindBestLrSwapResponse {
    /// Swap tx data (from 1inch quote)
    pub lr_swap_details: ClassicSwapDetails,
    /// found best order which can be filled with LR swap
    pub best_order: AggregatedOrderbookEntryV2,
    /// base/rel price including the price of the LR swap part
    pub total_price: BigDecimal, 
    /// Same retuned 
    pub trade_fee: TradePreimageResponse,
}

/// Request to sell or buy with LR
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FillOrderWithLrRequest {
    /// Original sell or buy request (but only MatchBy::Orders could be used to fill the maker swap found in )
    #[serde(flatten)]
    pub fill_req: SellBuyRequest,

    /// Tx data to create one inch swap (from 1inch quote)
    /// TODO: make this a enum to allow other LR providers
    pub lr_swap_details: ClassicSwapDetails,
}

/// Request to sell or buy with LR
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FillOrderWithLrResponse {
    /// Original sell or buy response
    #[serde(flatten)]
    pub fill_response: SellBuyResponse,
}