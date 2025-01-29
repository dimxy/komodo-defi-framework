//? RPC implementation for swaps with liquidity routing (LR)

use mm2_err_handle::mm_error::{MmResult, MmError};
use mm2_core::mm_ctx::MmArc;
use types::{LrSwapForMultipleOrdersRequest, FindBestLrSwapResponse, FillOrderWithLrRequest, FillOrderWithLrResponse};
use errors::LrSwapRpcError;

pub mod errors;
pub mod types;


/// Find best swap with liquidity routing over EVM tokens for multiple user tokens.
/// For the provided order RPC searches for the most price-effective swap path with LR for user tokens.
/// More info:
/// User is interested in buying or selling some coin. 
/// There are orders available with that coin but User does not have tokens to fill those orders.
/// User would like to find best swap path to fill one of those orders with his available tokens, with the use of LR.
/// User calls this RPC with the order, desired coin name, amount to buy or sell and list of his tokens he would like to do LR with. 
/// For the token in the provided order the RPC calls several 1inch classic swap queries 
/// to find out the best route from the list of User's tokens into token from the order.
/// The RPC returns best LR into the order, taking into account LR price and fees
pub async fn find_best_lr_swap_for_token_list_rpc(
    _ctx: MmArc,
    _req: LrSwapForMultipleOrdersRequest,
) -> MmResult<FindBestLrSwapResponse, LrSwapRpcError> {
    MmError::err(LrSwapRpcError::SomeError)
}

/// Find best swap with liquidity routing over EVM tokens for multiple orders.
/// For the provided list of orderbook entries this RPC searches for the most price-effective swap path with LR of a User token.
/// More info:
/// User is interested in buying or selling some coin. 
/// There are orders available with that coin but User does not have tokens to fill those orders.
/// User would like to find best swap path to fill several of those orders with one of his available tokens, with the use of LR.
/// User calls this RPC with an order list, coin name and amount to buy or sell and the name of his token he would like to do LR with. 
/// The RPC runs 1inch classic swap query requests to find out the best route from User's token into tokens from the orders from the list
/// The RPC returns the most price-effective swap path, taking into account price in the orders, LR price and tx fees. 
pub async fn find_best_lr_swap_for_order_list_rpc(
    _ctx: MmArc,
    _req: LrSwapForMultipleOrdersRequest,
) -> MmResult<FindBestLrSwapResponse, LrSwapRpcError> {
    MmError::err(LrSwapRpcError::SomeError)
}

/// Run a swap with LR part
pub async fn fill_order_with_lr_rpc(
    _ctx: MmArc,
    _req: FillOrderWithLrRequest,
) -> MmResult<FillOrderWithLrResponse, LrSwapRpcError> {
    MmError::err(LrSwapRpcError::SomeError)
}

// TODO: Do we need to extend trade_preimage_rpc to include LR-part fee?
// Apparently not: in fact, lr_find_best_swap_path_rpc has same behaviour: returns trade fee