//! RPC implementations for swaps with liquidity routing (LR) of EVM tokens

use super::ext_api::ext_api_errors::ExtApiRpcError;
use super::ext_api::ext_api_types::ClassicSwapDetails;
use crate::lp_swap::check_balance_for_taker_swap;
use crate::lr_swap::lr_helpers::{check_if_one_inch_supports_pair, get_coin_for_one_inch, sell_buy_method};
use crate::lr_swap::lr_quote::find_best_fill_ask_with_lr;
use crate::lr_swap::lr_swap_state_machine::lp_start_agg_taker_swap;
use crate::lr_swap::{AtomicSwapParams, LrSwapParams};
use coins::eth::u256_to_big_decimal;
use coins::{lp_coinfind_or_err, CoinWithDerivationMethod, FeeApproxStage, MmCoin};
use lr_api_types::{LrFindBestQuoteRequest, LrFindBestQuoteResponse, LrGetQuotesForTokensRequest, LrGetQuotesForTokensResponse, LrExecuteRoutedTradeRequest, LrExecuteRoutedTradeResponse};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_mm_error::MapMmError,
                     mm_error::{MmError, MmResult}};
use mm2_number::MmNumber;
use std::ops::Deref;

#[cfg(all(test, not(target_arch = "wasm32"), feature = "test-ext-api"))]
mod lr_api_tests;
pub(crate) mod lr_api_types;

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
/// TODO: currently the RPC supports filling only maker ask orders with rel=token_x, with routing 'user_rel' into maker 'rel' before the 'buy' atomic swap.
/// The RPC should also support other options:
/// filling maker bid orders with routing maker 'rel' into 'user_base' after the 'sell' atomic swap
/// filling maker ask orders with routing maker 'base' into 'user_rel' after the 'buy' atomic-swap
/// filling maker bid orders with routing 'user_base' token into maker 'base' before the 'sell' atomic swap
pub async fn lr_find_best_quote_rpc(
    ctx: MmArc,
    req: LrFindBestQuoteRequest,
) -> MmResult<LrFindBestQuoteResponse, ExtApiRpcError> {
    // TODO: add validation:
    // order.base_min_volume << req.amount <= order.base_max_volume
    // order.coin is supported in 1inch
    // order.price not zero
    // when best order is selected validate against req.rel_max_volume and req.rel_min_volume
    // coins in orders should be unique

    let (user_rel_coin, _) = get_coin_for_one_inch(&ctx, &req.user_rel).await?;
    let user_rel_chain = user_rel_coin
        .chain_id()
        .ok_or(ExtApiRpcError::ChainNotSupported)?;

    let (lr_data, best_order, total_price) =
        find_best_fill_ask_with_lr(&ctx, req.user_base, req.user_rel, req.asks, req.bids, &req.volume).await?;
    let src_amount = MmNumber::from(u256_to_big_decimal(lr_data.src_amount, user_rel_coin.decimals())?);
    let lr_swap_details =
        ClassicSwapDetails::from_api_classic_swap_data(&ctx, user_rel_chain, src_amount, lr_data.api_details)
            .await
            .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))?;
    Ok(LrFindBestQuoteResponse {
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
pub async fn lr_get_quotes_for_tokens_rpc(
    _ctx: MmArc,
    _req: LrGetQuotesForTokensRequest,
) -> MmResult<LrGetQuotesForTokensResponse, ExtApiRpcError> {
    // TODO: impl later
    todo!()
}

/// Run a swap with LR to fill a maker order
pub async fn lr_execute_routed_trade_rpc(
    ctx: MmArc,
    req: LrExecuteRoutedTradeRequest,
) -> MmResult<LrExecuteRoutedTradeResponse, ExtApiRpcError> {
    let atomic_swap_params = AtomicSwapParams {
        action: sell_buy_method(&req.atomic_swap.method)?,
        base: req.atomic_swap.base.clone(),
        rel: req.atomic_swap.rel.clone(),
        price: req.atomic_swap.price,
        match_by: req.atomic_swap.match_by,
        order_type: req.atomic_swap.order_type,
        base_volume: if req.lr_swap_0.is_some() {
            None
        } else {
            let volume = req
                .atomic_swap
                .volume
                .as_ref()
                .ok_or(ExtApiRpcError::InvalidParam("Atomic swap volume not set".to_string()))?; // if lr_swap_0 set the atomic swap will be determined as the result of lr_swap_0
            Some(volume.clone())
        },
    };

    let lr_swap_params_0 = if let Some(ref lr_swap_0) = req.lr_swap_0 {
        let (base_coin, base_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_source_token()?).await?;
        let (rel_coin, rel_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_destination_token()?).await?;
        let base_chain_id = base_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let rel_chain_id = rel_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(base_chain_id, rel_chain_id)?;

        // Ensure correct token routing
        if lr_swap_0.get_destination_token()? != atomic_swap_params.taker_coin() {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: LR token {}, atomic swap token {}",
                lr_swap_0.get_destination_token()?,
                req.atomic_swap.base
            )));
        }
        //let sell_amount = wei_from_big_decimal(&lr_swap_0.swap_details.src_amount.to_decimal(), base_coin.decimals())
        //    .mm_err(|err| ExtApiRpcError::InvalidParam(err.to_string()))?;
        //let expected_dst_amount = wei_from_big_decimal(&lr_swap_0.swap_details.dst_amount.amount, base_coin.decimals())
        //    .mm_err(|err| ExtApiRpcError::InvalidParam(err.to_string()))?;
        let single_address = base_coin.derivation_method().single_addr_or_err().await?;
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
        /*let call_params = make_classic_swap_create_params(
            base_contract,
            rel_contract,
            sell_amount,
            single_address,
            lr_swap_0.slippage,
            lr_swap_0.opt_params.clone(),
        );*/
        Some(LrSwapParams {
            src_amount: lr_swap_0.swap_details.src_amount.clone(),
            src_decimals: base_coin.decimals(),
            dst_decimals: rel_coin.decimals(),
            src: lr_swap_0.get_source_token()?,
            src_contract: base_contract,
            dst: lr_swap_0.get_source_token()?,
            dst_contract: rel_contract,
            from: single_address,
            slippage: lr_swap_0.slippage,
        })
    } else {
        None
    };

    // Check token balance for atomic swap, if this is the first step:
    if req.lr_swap_0.is_none() {
        let atomic_swap_base = lp_coinfind_or_err(&ctx, &req.atomic_swap.base).await?;
        let atomic_swap_rel = lp_coinfind_or_err(&ctx, &req.atomic_swap.rel).await?;
        check_balance_for_taker_swap(
            &ctx,
            atomic_swap_base.deref(),
            atomic_swap_rel.deref(),
            req.atomic_swap
                .volume
                .ok_or(ExtApiRpcError::InvalidParam("Atomic swap volume not set".to_string()))?,
            None,
            None,
            FeeApproxStage::OrderIssue,
        )
        .await?
    }

    let lr_swap_params_1 = if let Some(lr_swap_1) = req.lr_swap_1 {
        let (base_coin, base_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_source_token()?).await?;
        let (rel_coin, rel_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_destination_token()?).await?;
        let base_chain_id = base_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let rel_chain_id = rel_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(base_chain_id, rel_chain_id)?;

        // Ensure correct token routing
        if atomic_swap_params.maker_coin() != lr_swap_1.get_source_token()? {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: atomic swap token {}, LR token {}",
                req.atomic_swap.rel,
                lr_swap_1.get_source_token()?
            )));
        }
        //let sell_amount = wei_from_big_decimal(&lr_swap_1.swap_details.src_amount.to_decimal(), base_coin.decimals())
        //    .mm_err(|err| ExtApiRpcError::InvalidParam(err.to_string()))?;
        //let expected_dst_amount = wei_from_big_decimal(&lr_swap_1.swap_details.dst_amount.amount, base_coin.decimals())
        //    .mm_err(|err| ExtApiRpcError::InvalidParam(err.to_string()))?;
        let single_address = base_coin.derivation_method().single_addr_or_err().await?;
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
        /*let call_params = make_classic_swap_create_params(
            base_contract,
            rel_contract,
            sell_amount,
            single_address,
            lr_swap_1.slippage,
            lr_swap_1.opt_params
        );*/
        Some(LrSwapParams {
            src_amount: lr_swap_1.swap_details.src_amount.clone(),
            src_decimals: base_coin.decimals(),
            dst_decimals: rel_coin.decimals(),
            src: lr_swap_1.get_source_token()?,
            src_contract: base_contract,
            dst: lr_swap_1.get_source_token()?,
            dst_contract: rel_contract,
            from: single_address,
            slippage: lr_swap_1.slippage,
        })
    } else {
        None
    };
    let swap_uuid = lp_start_agg_taker_swap(ctx, lr_swap_params_0, lr_swap_params_1, atomic_swap_params).await?;
    Ok(LrExecuteRoutedTradeResponse { uuid: swap_uuid })
}
