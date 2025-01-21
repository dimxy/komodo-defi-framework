//? RPC implementation for swaps with liquidity routing (LR)

use mm2_err_handle::mm_error::{MmResult, MmError};
use mm2_core::mm_ctx::MmArc;
use types::{LrFindBestSwapPathRequest, LrFindBestSwapPathResponse, LrFillOrderRequest, LrFillOrderResponse};
use errors::LrSwapRpcError;

pub mod errors;
pub mod types;

/// Find best swap path with liquidity routing over evm tokens.
/// For the provided list of orderbook entries this RPC searches for the most price effective swap with LR
pub async fn lr_find_best_swap_path_rpc(
    ctx: MmArc,
    req: LrFindBestSwapPathRequest,
) -> MmResult<LrFindBestSwapPathResponse, LrSwapRpcError> {
    MmError::err(LrSwapRpcError::SomeError)
}

/// Run a swap with LR part
pub async fn lr_fill_order_rpc(
    ctx: MmArc,
    req: LrFillOrderRequest,
) -> MmResult<LrFillOrderResponse, LrSwapRpcError> {
    MmError::err(LrSwapRpcError::SomeError)
}

// TODO: Do we need to extend trade_preimage_rpc to include LR-part fee?
// In fact, lr_find_best_swap_path_rpc has same behaviour: returns trade fee