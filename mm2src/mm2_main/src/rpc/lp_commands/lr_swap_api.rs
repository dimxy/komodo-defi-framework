//! RPC implementations for swaps with liquidity routing (LR) of EVM tokens

use super::ext_api::ext_api_errors::ExtApiRpcError;
use super::ext_api::ext_api_types::ClassicSwapDetails;
use crate::lp_swap::{check_balance_for_taker_swap, check_my_coin_balance_for_swap, check_other_coin_balance_for_swap,
                     CheckBalanceError};
use crate::lr_swap::lr_helpers::{check_if_one_inch_supports_pair, get_coin_for_one_inch, sell_buy_method};
use crate::lr_swap::lr_quote::find_best_swap_path_with_lr;
use crate::lr_swap::lr_swap_state_machine::lp_start_agg_taker_swap;
use crate::lr_swap::{AtomicSwapParams, LrSwapParams};
use coins::eth::u256_to_big_decimal;
use coins::{lp_coinfind_or_err, CoinWithDerivationMethod, FeeApproxStage, MarketCoinOps, MmCoin, MmCoinEnum, TradeFee};
use common::log::debug;
use futures::compat::Future01CompatExt;
use lr_api_types::{LrExecuteRoutedTradeRequest, LrExecuteRoutedTradeResponse, LrFindBestQuoteRequest,
                   LrFindBestQuoteResponse, LrGetQuotesForTokensRequest, LrGetQuotesForTokensResponse};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_mm_error::MapMmError,
                     mm_error::{MmError, MmResult}};
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::TakerAction;
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
    let user_rel_chain_id = user_rel_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
    let action = sell_buy_method(&req.method)?;

    let (lr_data_0, best_order, lr_data_1, total_price) = find_best_swap_path_with_lr(
        &ctx,
        req.user_base,
        req.user_rel,
        action,
        req.asks,
        req.bids,
        &req.volume,
    )
    .await?;

    let lr_data_0 = lr_data_0
        .map(|lr_data| {
            let src_amount = MmNumber::from(u256_to_big_decimal(lr_data.src_amount, user_rel_coin.decimals())?);
            ClassicSwapDetails::from_api_classic_swap_data(&ctx, user_rel_chain_id, src_amount, lr_data.api_details)
                .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))
        })
        .transpose()?;
    let lr_data_1 = lr_data_1
        .map(|lr_data| {
            let src_amount = MmNumber::from(u256_to_big_decimal(lr_data.src_amount, user_rel_coin.decimals())?);
            ClassicSwapDetails::from_api_classic_swap_data(&ctx, user_rel_chain_id, src_amount, lr_data.api_details) // TODO: user_rel_chain_id incorrect
                .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))
        })
        .transpose()?;
    Ok(LrFindBestQuoteResponse {
        lr_data_0,
        lr_data_1,
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
    debug_print_routed_trade_req(&req);
    let atomic_swap_params = AtomicSwapParams {
        action: sell_buy_method(&req.atomic_swap.method)?,
        base: req.atomic_swap.base.clone(),
        rel: req.atomic_swap.rel.clone(),
        price: req.atomic_swap.price,
        match_by: req.atomic_swap.match_by,
        order_type: req.atomic_swap.order_type,
        base_volume: req.atomic_swap.volume, // TODO: validation vs src_amount
    };

    // Validate LR step 0 (before the atomic swap):
    let lr_swap_params_0 = if let Some(ref lr_swap_0) = req.lr_swap_0 {
        let (src_coin, src_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_source_token()?).await?;
        let (dst_coin, dst_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_destination_token()?).await?;
        let src_chain_id = src_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let dst_chain_id = dst_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(src_chain_id, dst_chain_id)?;

        // Ensure correct token routing from LR-0 to atomic swap
        if lr_swap_0.get_destination_token()? != atomic_swap_params.taker_coin() {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: LR token {}, atomic swap token {}",
                lr_swap_0.get_destination_token()?,
                req.atomic_swap.base
            )));
        }
        let single_address = src_coin.derivation_method().single_addr_or_err().await?;
        debug!(
            "Calling check source coin '{}' balance for LR step 0...",
            src_coin.ticker()
        );
        // Check LR swap step source token balance:
        check_my_coin_balance_for_swap(
            &ctx,
            &src_coin,
            None,
            lr_swap_0.swap_details.src_amount.clone(),
            TradeFee {
                coin: src_coin.platform_ticker().to_owned(),
                amount: Default::default(),   // TODO: fix (add to the LR quote)
                paid_from_trading_vol: false, // false as LR is done for EVM tokens
            },
            None,
        )
        .await?;
        Some(LrSwapParams {
            src_amount: lr_swap_0.swap_details.src_amount.clone(),
            src_decimals: src_coin.decimals(),
            dst_decimals: dst_coin.decimals(),
            src: lr_swap_0.get_source_token()?,
            src_contract,
            dst: lr_swap_0.get_destination_token()?,
            dst_contract,
            from: single_address,
            slippage: lr_swap_0.slippage,
        })
    } else {
        None
    };

    let atomic_swap_base_coin = lp_coinfind_or_err(&ctx, &req.atomic_swap.base).await?;
    let atomic_swap_rel_coin = lp_coinfind_or_err(&ctx, &req.atomic_swap.rel).await?;
    let (taker_coin, maker_coin) = match atomic_swap_params.action {
        TakerAction::Sell => (atomic_swap_base_coin, atomic_swap_rel_coin),
        TakerAction::Buy => (atomic_swap_rel_coin, atomic_swap_base_coin),
    };
    if req.lr_swap_0.is_none() {
        // Check token balance for the atomic swap, if it is the first step:
        debug!(
            "Calling check taker coin '{}' balance for atomic swap...",
            taker_coin.deref().ticker()
        );
        // TODO: no need as start_lp_auto_buy in the state machine does this
        check_balance_for_taker_swap(
            &ctx,
            taker_coin.deref(),
            maker_coin.deref(),
            atomic_swap_params.taker_volume(),
            None,
            None,
            FeeApproxStage::OrderIssue,
        )
        .await?
    }

    // Validate needed trade fee in maker platform coin, total for atomic swap and LR_1
    // For the atomic swap Taker may need some amount to spend the Maker payment:
    let mut maker_coin_trade_fee = maker_coin
        .get_receiver_trade_fee(FeeApproxStage::OrderIssue)
        .compat()
        .await
        .mm_err(|e| CheckBalanceError::from_trade_preimage_error(e, maker_coin.ticker()))?;

    // Add tx fee for LR_1:
    if let (Some(lr_swap_1), MmCoinEnum::EthCoin(eth_coin)) = (&req.lr_swap_1, &maker_coin) {
        let lr_trade_fee = eth_coin
            .estimate_trade_fee(
                lr_swap_1.swap_details.gas.unwrap_or_default().into(),
                FeeApproxStage::OrderIssue,
            )
            .await?;
        maker_coin_trade_fee.amount += lr_trade_fee.amount;
        maker_coin_trade_fee.paid_from_trading_vol = false;
    }
    debug!(
        "Check other_coin '{}' balance for atomic swap, paid_from_trading_vol={}",
        maker_coin.ticker(),
        maker_coin_trade_fee.paid_from_trading_vol
    );
    if !maker_coin_trade_fee.paid_from_trading_vol {
        debug!(
            "Calling check other_coin '{}' balance for atomic swap...",
            maker_coin.ticker()
        );
        check_other_coin_balance_for_swap(&ctx, maker_coin.deref(), None, maker_coin_trade_fee).await?;
    }

    // Validate LR step 1 (after the atomic swap):
    let lr_swap_params_1 = if let Some(lr_swap_1) = req.lr_swap_1 {
        let (src_coin, src_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_source_token()?).await?;
        let (dst_coin, dst_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_destination_token()?).await?;
        let src_chain_id = src_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let dst_chain_id = dst_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(src_chain_id, dst_chain_id)?;

        // Ensure correct token routing from atomic swap to LR_1
        if atomic_swap_params.maker_coin() != lr_swap_1.get_source_token()? {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: atomic swap token {}, LR token {}",
                req.atomic_swap.rel,
                lr_swap_1.get_source_token()?
            )));
        }
        let single_address = src_coin.derivation_method().single_addr_or_err().await?;
        Some(LrSwapParams {
            src_amount: lr_swap_1.swap_details.src_amount.clone(),
            src_decimals: src_coin.decimals(),
            dst_decimals: dst_coin.decimals(),
            src: lr_swap_1.get_source_token()?,
            src_contract,
            dst: lr_swap_1.get_destination_token()?,
            dst_contract,
            from: single_address,
            slippage: lr_swap_1.slippage,
        })
    } else {
        None
    };
    let swap_uuid = lp_start_agg_taker_swap(ctx, lr_swap_params_0, lr_swap_params_1, atomic_swap_params).await?;
    Ok(LrExecuteRoutedTradeResponse { uuid: swap_uuid })
    // TODO: add reasons for AbortReason
    // TODO: ExtApiError - fix included big OneInchApi err
    // TODO: replace LrSwapError::InternalError with concrete errors
    // TODO: add LR_1 txfee check - added
    // TODO: return 'allowance' rpc result in MmNumber coin units
    // TODO: return 'allowance not enough' error in MmNumber coin units
    // TODO: add set swap tx policy to enable_coin rpc
}

fn debug_print_routed_trade_req(req: &LrExecuteRoutedTradeRequest) {
    let (lr_0_src, lr_0_dst, lr_0_src_amount) = req
        .lr_swap_0
        .as_ref()
        .map(|lr| {
            (
                lr.swap_details.src_token.as_ref().and_then(|t| t.symbol_kdf.as_ref()),
                lr.swap_details.dst_token.as_ref().and_then(|t| t.symbol_kdf.as_ref()),
                Some(&lr.swap_details.src_amount),
            )
        })
        .unwrap_or_default();
    let (base, rel, method, volume) = (
        &req.atomic_swap.base,
        &req.atomic_swap.rel,
        &req.atomic_swap.method,
        &req.atomic_swap.volume,
    );
    let (lr_1_src, lr_1_dst, lr_1_src_amount) = req
        .lr_swap_1
        .as_ref()
        .map(|lr| {
            (
                lr.swap_details.src_token.as_ref().and_then(|t| t.symbol_kdf.as_ref()),
                lr.swap_details.dst_token.as_ref().and_then(|t| t.symbol_kdf.as_ref()),
                Some(&lr.swap_details.src_amount),
            )
        })
        .unwrap_or_default();
    debug!("RPC execute_routed_trade entered with lr_0: [src: {:?} dst: {:?} src_amount: {:?}] atomic swap: [base: {:?} rel: {:?} method: {:?} volume: {:?}] lr_1 [src: {:?} dst: {:?} src_amount: {:?}] ", 
        lr_0_src, lr_0_dst, lr_0_src_amount.map(|v| v.to_decimal()),
        base, rel, method, volume.to_decimal(),
        lr_1_src, lr_1_dst, lr_1_src_amount.map(|v| v.to_decimal()),
    );
}
