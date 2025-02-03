//? RPC implementation for swaps with liquidity routing (LR)

use coins::lp_coinfind_or_err;
use mm2_err_handle::{map_mm_error::MapMmError, mm_error::{MmError, MmResult}};
use mm2_core::mm_ctx::MmArc;
use types::{FindBestLrSwapForMultipleOrdersRequest, FindBestLrSwapForMultipleTokensRequest, FindBestLrSwapResponse, FillOrderWithLrRequest, FillOrderWithLrResponse};
use lr_impl::find_best_lr_buy_for_multiple_orders;
use crate::rpc::lp_commands::one_inch::errors::ApiIntegrationRpcError;

use super::one_inch::types::ClassicSwapDetails;
//use errors::LrSwapRpcError;

mod errors;
mod types;
mod lr_impl;


/// Find best swap with liquidity routing over EVM tokens for multiple user tokens.
/// For the provided order the RPC searches for the most price-effective swap path with LR for user tokens.
/// More info:
/// User is interested in buying or selling some coin. 
/// There are orders available with that coin but User does not have tokens to fill those orders.
/// User would like to find best swap path to fill one of those orders with his available tokens, with the use of LR.
/// User calls this RPC with the order, desired coin name, amount to buy or sell and list of his tokens he would like to do LR with. 
/// For the token in the User provided order the RPC calls several 1inch classic swap queries 
/// to find out the best route from User's tokens into the order token (or backwards).
/// The RPC returns best LR for the order, taking into account the LR price and fees
pub async fn find_best_lr_swap_for_token_list_rpc(
    _ctx: MmArc,
    _req: FindBestLrSwapForMultipleTokensRequest,
) -> MmResult<FindBestLrSwapResponse, ApiIntegrationRpcError> {
    MmError::err(ApiIntegrationRpcError::SomeError("unimplemented".to_owned()))
}

/// Find best swap with liquidity routing over EVM tokens for multiple orders.
/// For the provided list of orderbook entries this RPC searches for the most price-effective swap path with LR of a User token.
/// More info:
/// User is interested in buying or selling some coin. 
/// There are orders available with that coin but User does not have tokens to fill those orders.
/// User would like to find best swap path to fill several of those orders with one of his available tokens, with the use of LR.
/// User calls this RPC with an order list, coin name and amount to buy or sell and the name of his token he would like to do LR with. 
/// The RPC runs 1inch classic swap query requests to find out the best route from User's token into orders' tokens (or backwards).
/// The RPC returns the most price-effective swap with LR, taking into account order prices, the LR price and tx fees. 
pub async fn find_best_lr_swap_for_order_list_rpc(
    ctx: MmArc,
    req: FindBestLrSwapForMultipleOrdersRequest,
) -> MmResult<FindBestLrSwapResponse, ApiIntegrationRpcError> {
    let coin = lp_coinfind_or_err(&ctx, &req.my_token).await?;
    let (swap_data, best_order, total_price) = find_best_lr_buy_for_multiple_orders(&ctx, req.my_token, &req.orderbook_entries, &req.amount).await?;
    let lr_swap_details = ClassicSwapDetails::from_api_classic_swap_data(swap_data, coin.decimals()).mm_err(|err| ApiIntegrationRpcError::ApiDataError(err.to_string()))?;
    Ok(FindBestLrSwapResponse {
        lr_swap_details,
        best_order,
        total_price: todo!(),
        trade_fee: todo!(),
    })
}

/// Run a swap with LR to fill a maker order
pub async fn fill_order_with_lr_rpc(
    _ctx: MmArc,
    _req: FillOrderWithLrRequest,
) -> MmResult<FillOrderWithLrResponse, ApiIntegrationRpcError> {
    MmError::err(ApiIntegrationRpcError::SomeError("unimplemented".to_owned()))
}

// TODO: Do we need to extend trade_preimage_rpc to include LR-part fee?
// Apparently not: in fact, lr_find_best_swap_path_rpc has same behaviour: returns trade fee