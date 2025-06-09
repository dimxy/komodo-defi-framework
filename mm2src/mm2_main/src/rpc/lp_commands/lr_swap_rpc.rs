//! RPC implementations for swaps with liquidity routing (LR) of EVM tokens

use std::ops::Deref;
use crate::lp_swap::{check_balance_for_taker_swap, check_my_coin_balance_for_swap};
use crate::rpc::lp_commands::ext_api::ext_api_errors::ExtApiRpcError;
use crate::rpc::lp_commands::ext_api::ext_api_types::ClassicSwapDetails;
use coins::{lp_coinfind_or_err, MarketCoinOps, TradeFee, FeeApproxStage};
use crate::lr_swap::lr_quote::find_best_fill_ask_with_lr;
use crate::lr_swap::lr_swap_state_machine::lp_start_agg_taker_swap;
use lr_rpc_types::{LrBestQuoteRequest, LrBestQuoteResponse, LrFillMakerOrderRequest, LrFillMakerOrderResponse,
               LrQuotesForTokensRequest};
use crate::lr_swap::lr_helpers::{check_if_one_inch_supports_pair, maker_coin_from_req, taker_coin_from_req, get_coin_for_one_inch, taker_volume_from_req};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_mm_error::MapMmError, mm_error::{MmError, MmResult}};

mod lr_rpc_types;
#[cfg(all(test, not(target_arch = "wasm32"), feature = "test-ext-api"))] mod lr_tests;

/// Find the best swap with liquidity routing of EVM tokens, to select from multiple orders.
/// For the provided list of orderbook entries this RPC will find out the most price-effective swap with LR.
///
/// More info:
/// User is interested in buying some coin. There are orders available with the desired coin but User does not have tokens to fill those orders.
/// User may fill the order with User my_token running an preliminary LR swap combined with the ordinary dex-swap (using 1inch as the LR provider).
/// Or, User may convert the tokens from the order into my_token with a subsequent LR swap.
///
/// User calls this RPC with an ask/bid order list, base or rel coin, amount to buy or sell and User token name to fill the order with.
/// The RPC runs 1inch swap quotes to convert User's my_token into orders' tokens (or backwards)
/// and returns the most price-effective swap path, taking into account order and LR prices.
/// TODO: should also returns total fees.
///
/// TODO: currently supported only ask orders with rel=token_x, with routing User's my_token into token_x before the dex-swap.
/// The RPC should also support:
/// bid orders with rel=token_x, with routing token_x into my_token after the dex-swap
/// ask orders with base=token_x, with routing token_x into my_token after the dex-swap
/// bid orders with base=token_x, with routing User's my_token into token_x before the dex-swap
pub async fn lr_best_quote_rpc(
    ctx: MmArc,
    req: LrBestQuoteRequest,
) -> MmResult<LrBestQuoteResponse, ExtApiRpcError> {
    // TODO: add validation:
    // order.base_min_volume << req.amount <= order.base_max_volume
    // order.coin is supported in 1inch
    // order.price not zero
    // when best order is selected validate against req.rel_max_volume and req.rel_min_volume
    // coins in orders should be unique

    let (my_eth_coin, _) = get_coin_for_one_inch(&ctx, &req.my_token).await?;
    let my_chain_id = my_eth_coin
        .chain_id()
        .ok_or(ExtApiRpcError::ChainNotSupported)?;
    let (swap_data, best_order, total_price) =
        find_best_fill_ask_with_lr(&ctx, req.my_token, req.asks, &req.amount).await?;
    let lr_swap_details = ClassicSwapDetails::from_api_classic_swap_data(&ctx, my_chain_id, swap_data)
        .await
        .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))?;
    Ok(LrBestQuoteResponse {
        lr_swap_details,
        best_order,
        total_price,
        // TODO: implement later
        // trade_fee: ...
    })
}

/// Find possible swaps with liquidity routing of several user tokens to fill one order.
/// For the provided single order the RPC searches for the most price-effective swap path with LR for user tokens.
///
/// More info:
/// User is interested in buying some coin. There is an order available the User would like to fill but the User does not have tokens from the order.
/// User calls this RPC with the order, desired coin name, amount to buy or sell and list of User tokens to convert to/from with LR.
/// The RPC calls several 1inch classic swap quotes (to find most efficient token conversions)
/// and return possible LR paths to fill the order, with total swap prices.
/// TODO: should also returns total fees.
///
/// NOTE: this RPC does not select the best quote between User tokens because it finds routes for different tokens (with own value),
/// so returns all of them.
/// That is, it's up to the User to select the most cost effective swap, for e.g. comparing token fiat value.
/// In fact, this could be done even in this RPC as 1inch also can get value in fiat but maybe User evaludation is more prefferable.
/// Again, it's a TODO.
pub async fn lr_quotes_for_tokens_rpc(
    _ctx: MmArc,
    _req: LrQuotesForTokensRequest,
) -> MmResult<LrBestQuoteResponse, ExtApiRpcError> {
    // TODO: impl later
    todo!()
}

/// Run a swap with LR to fill a maker order
pub async fn lr_fill_order_rpc(
    ctx: MmArc,
    req: LrFillMakerOrderRequest,
) -> MmResult<LrFillMakerOrderResponse, ExtApiRpcError> {
    println!("sell_buy_req={:?}", req.sell_buy_req);

    check_agg_taker_swap_request(&ctx, &req).await?;

    // State machine errors will be automatically converted to ApiIntegrationRpcError
    let swap_uuid = lp_start_agg_taker_swap(ctx, req.volume, req.lr_swap_0, req.lr_swap_1, req.sell_buy_req).await?;

    Ok(LrFillMakerOrderResponse { uuid: swap_uuid })
}

async fn check_agg_taker_swap_request(
    ctx: &MmArc,
    req: &LrFillMakerOrderRequest,
) -> MmResult<(), ExtApiRpcError> {
    
    if let Some(ref lr_swap_0) = req.lr_swap_0 {
        let (base_coin, _) = get_coin_for_one_inch(&ctx, &lr_swap_0.base).await?;
        let (rel_coin, _) = get_coin_for_one_inch(&ctx, &lr_swap_0.rel).await?;
        let base_chain_id = base_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let rel_chain_id = rel_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(base_chain_id, rel_chain_id)?;

        // Ensure correct token routing
        if lr_swap_0.rel != taker_coin_from_req(&req.sell_buy_req)? {
            return MmError::err(ExtApiRpcError::InvalidParam(format!("Connecting tokens must be same: LR token {}, atomic token {}", lr_swap_0.rel, req.sell_buy_req.base)));
        }
        // Check source token balance for LR swap step:
        // TODO: enable when swap details are passed
        /*check_my_coin_balance_for_swap(
            ctx,
            &base_coin,
            None,
            lr_swap_0.amount.clone(),
            TradeFee {
                coin: base_coin.platform_ticker().to_owned(),
                amount: todo!(),
                paid_from_trading_vol: true,
            },
            None,
        )
        .await?;*/
    }

    // Check token balance for atomic swap, if this is the first step:
    if req.lr_swap_0.is_none() {
        let volume = taker_volume_from_req(&req.sell_buy_req)?;
        let atomic_swap_base = lp_coinfind_or_err(ctx, &req.sell_buy_req.base).await?;
        let atomic_swap_rel = lp_coinfind_or_err(ctx, &req.sell_buy_req.rel).await?;
        check_balance_for_taker_swap(
            &ctx,
            atomic_swap_base.deref(),
            atomic_swap_rel.deref(),
            volume,
            None,
            None,
            FeeApproxStage::OrderIssue
        )
        .await?
    }

    if let Some(ref lr_swap_1) = req.lr_swap_1 {
        let (base_coin, _) = get_coin_for_one_inch(&ctx, &lr_swap_1.base).await?;
        let (rel_coin, _) = get_coin_for_one_inch(&ctx, &lr_swap_1.rel).await?;
        let base_chain_id = base_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let rel_chain_id = rel_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(base_chain_id, rel_chain_id)?;

        // Ensure correct token routing
        if maker_coin_from_req(&req.sell_buy_req)? != lr_swap_1.base {
            return MmError::err(ExtApiRpcError::InvalidParam(format!("Connecting tokens must be same: atomic token {}, LR token {}", req.sell_buy_req.rel, lr_swap_1.base)));
        }

        // Check source token balance for LR swap step:
        // TODO: enable when swap details are passed
        /*check_my_coin_balance_for_swap(
            ctx,
            &base_coin,
            None,
            lr_swap_1.amount.clone(),
            TradeFee {
                coin: base_coin.platform_ticker().to_owned(),
                amount: todo!(),
                paid_from_trading_vol: true,
            },
            None,
        )
        .await?;*/
    }
    Ok(())
}
