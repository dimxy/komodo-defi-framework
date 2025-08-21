//! RPC implementations for swaps with liquidity routing (LR) of EVM tokens

use super::ext_api::ext_api_errors::ExtApiRpcError;
use super::ext_api::ext_api_types::ClassicSwapDetails;
use crate::lp_swap::{
    check_balance_for_taker_swap, check_my_coin_balance_for_swap, check_other_coin_balance_for_swap, CheckBalanceError,
};
use crate::lr_swap::lr_helpers::{check_if_one_inch_supports_pair, get_coin_for_one_inch, sell_buy_method};
use crate::lr_swap::lr_quote::find_best_swap_path_with_lr;
use crate::lr_swap::lr_swap_state_machine::lp_start_agg_taker_swap;
use crate::lr_swap::{AtomicSwapParams, LrSwapParams};
use coins::{
    lp_coinfind_or_err, CoinWithDerivationMethod, FeeApproxStage, MarketCoinOps, MmCoin, MmCoinEnum, TradeFee,
};
use common::log::debug;
use futures::compat::Future01CompatExt;
use lr_api_types::{
    AtomicSwapRpcParams, LrExecuteRoutedTradeRequest, LrExecuteRoutedTradeResponse, LrFindBestQuoteRequest,
    LrFindBestQuoteResponse, LrGetQuotesForTokensRequest, LrGetQuotesForTokensResponse,
};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::MmResultExt;
use mm2_err_handle::{
    map_mm_error::MapMmError,
    mm_error::{MmError, MmResult},
};
use mm2_rpc::data::legacy::TakerAction;
use std::ops::Deref;

#[cfg(all(test, not(target_arch = "wasm32"), feature = "test-ext-api"))]
mod lr_api_tests;
pub(crate) mod lr_api_types;

/// The "find_best_quote" RPC implementation to find the best swap with liquidity routing (LR), also known as 'aggregated taker swap with LR'.
/// For the provided list of orderbook entries this RPC will find out the most price-effective swap which may include LR steps.
/// There are LR_0 and LR_1 steps supported meaning liquidity routing before or after the atomic swap.
/// Liquidity routing is supported in the EVM chains. This is actually a token swap performed by an LR provider (currently 1inch).
/// Both tokens must be in the same EVM network.
///
/// Use cases:
/// User is interested in buying some coin. There are orders available with the desired coin but User does not have tokens to fill those orders.
/// User has some amount of a user_rel token and would like to use it for buying the desired coin.
/// User calls this RPC to find the best swap with LR: with user_rel token converted into the atomic swap taker coin with LR_0 step.
/// User may also request to convert received maker coin into user_base token via LR_1 swap.
///
/// Similar logic is for the case when User would like to sell a token and use LR step to convert it into an atomic swap taker coin
/// and/or convert a maker coin into the User desired token.
///
/// User calls this RPC with the following params:
/// * user_base and user_rel tickers,
/// * amount to buy or sell,
/// * list of ask and bid orders list.
///
/// If user_base and/or user_rel differs from the coins in the orders the RPC creates LR_0 and/or LR_1 steps and gets LR provider quotes
/// to obtain prices for LR_0 and LR_1 steps. The RPC alsouses order prices to calculate the most price-effective swap path.
/// As the result the RPC returns params for LR_0 LR_1 and atomic swap corresponding the most price effective aggregated swap.
/// If best swap cannot be found a error is returned.
/// TODO: we should also return total fees.
///
/// More info:
/// The GUI should provide this RPC with a list of ask or bid orders to select best swap path from.
/// Currently for that the "best_orders" RPC can be used:
/// The GUI should pick tokens which resides in the same chains with user_base and/or user_rel and query "best_orders" RPC (maybe multiple times).
/// The "best_orders" results are asks or bids which can be passed into this RPC.
///
/// TODO: develop a more convenient RPC to find ask and bid orders for finding the best swap with LR.
/// It should support getting info about most liquid white-listed tokens from the LR provider, to do search for best swap more efficiently.
///
pub async fn lr_find_best_quote_rpc(
    ctx: MmArc,
    req: LrFindBestQuoteRequest,
) -> MmResult<LrFindBestQuoteResponse, ExtApiRpcError> {
    // TODO: add validation:
    // order.base_min_volume << req.amount <= order.base_max_volume - DONE
    // when best order is selected validate against req.rel_max_volume and req.rel_min_volume (?)
    // order coins are supported in 1inch
    // order.price not zero
    // coins in orders should be unique (?)
    // Check user available balance for user_base/user_rel when selecting best swap (?)

    let action = sell_buy_method(&req.method).map_mm_err()?;

    let (lr_data_0, best_order, atomic_swap_volume, lr_data_1, total_price) = find_best_swap_path_with_lr(
        &ctx,
        req.user_base,
        req.user_rel,
        &action,
        req.asks,
        req.bids,
        &req.volume,
    )
    .await
    .map_mm_err()?;

    let lr_data_0 = lr_data_0
        .map(|lr_data| {
            ClassicSwapDetails::from_api_classic_swap_data(
                &ctx,
                lr_data.chain_id,
                lr_data.src_amount,
                lr_data.api_details,
            )
            .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))
        })
        .transpose()?;
    let lr_data_1 = lr_data_1
        .map(|lr_data| {
            ClassicSwapDetails::from_api_classic_swap_data(
                &ctx,
                lr_data.chain_id,
                lr_data.src_amount,
                lr_data.api_details,
            )
            .mm_err(|err| ExtApiRpcError::OneInchDataError(err.to_string()))
        })
        .transpose()?;
    Ok(LrFindBestQuoteResponse {
        lr_data_0,
        lr_data_1,
        atomic_swap: AtomicSwapRpcParams {
            volume: atomic_swap_volume,
            base: best_order.taker_ticker(),
            rel: best_order.maker_ticker(),
            price: best_order.buy_price(),
            method: "sell".to_owned(), // Always convert to the 'sell' action to simplify LR_0 estimations
            order_uuid: best_order.order().uuid,
            match_by: None,
            order_type: None,
        },
        total_price,
        // TODO: implement later
        // trade_fee: ...
    })
}

/// TODO: Find possible swaps with liquidity routing of several user tokens to fill one order.
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
pub async fn lr_get_quotes_for_tokens_rpc(
    _ctx: MmArc,
    _req: LrGetQuotesForTokensRequest,
) -> MmResult<LrGetQuotesForTokensResponse, ExtApiRpcError> {
    // TODO: impl later
    todo!()
}

/// Run an aggregated swap with LR to fill a maker order.
/// The `req` parameter is constructed from the result of a "find_best_quote" RPC call
/// It contains parameters for the atomic swap and LR_0 and LR_1 steps
/// LR_0 and LR_1 are liquidity routing steps before and after the atomic swap (see the description for lr_find_best_quote_rpc fn)
pub async fn lr_execute_routed_trade_rpc(
    ctx: MmArc,
    req: LrExecuteRoutedTradeRequest,
) -> MmResult<LrExecuteRoutedTradeResponse, ExtApiRpcError> {
    debug_print_routed_trade_req(&req);

    if req.lr_swap_0.is_none() && req.lr_swap_1.is_none() {
        return MmError::err(ExtApiRpcError::InvalidParam("no LR params".to_owned()));
    }
    let action = sell_buy_method(&req.atomic_swap.method).map_mm_err()?;
    if action != TakerAction::Sell {
        return MmError::err(ExtApiRpcError::InvalidParam("action must be sell".to_owned()));
    }

    let atomic_swap_params = AtomicSwapParams {
        action,
        base: req.atomic_swap.base.clone(),
        rel: req.atomic_swap.rel.clone(),
        price: req.atomic_swap.price,
        match_by: req.atomic_swap.match_by.unwrap_or_default(),
        order_type: req.atomic_swap.order_type.unwrap_or_default(),
        base_volume: req.atomic_swap.volume,
    };

    // Validate LR step 0 (before the atomic swap):
    let lr_swap_params_0 = if let Some(ref lr_swap_0) = req.lr_swap_0 {
        let (src_coin, src_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_source_token()?)
            .await
            .map_mm_err()?;
        let (dst_coin, dst_contract) = get_coin_for_one_inch(&ctx, &lr_swap_0.get_destination_token()?)
            .await
            .map_mm_err()?;
        let src_chain_id = src_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let dst_chain_id = dst_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(src_chain_id, dst_chain_id).map_mm_err()?;

        // Ensure correct token routing from LR-0 to atomic swap
        if lr_swap_0.get_destination_token()? != atomic_swap_params.taker_coin() {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: LR_0 dst_token: {}, taker_coin: {}",
                lr_swap_0.get_destination_token()?,
                atomic_swap_params.taker_coin()
            )));
        }
        let single_address = src_coin.derivation_method().single_addr_or_err().await.map_mm_err()?;
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
        .await
        .map_mm_err()?;
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
            // If no gas limit provided, we may fail estimations in LR_0 rollback (and if it's the platform coin to rollback).
            // I guess this is not a reason to fail
            gas: lr_swap_0.swap_details.gas.unwrap_or_default(),
        })
    } else {
        None
    };

    let atomic_swap_base_coin = lp_coinfind_or_err(&ctx, &req.atomic_swap.base).await.map_mm_err()?;
    let atomic_swap_rel_coin = lp_coinfind_or_err(&ctx, &req.atomic_swap.rel).await.map_mm_err()?;
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
            atomic_swap_params.taker_volume()?,
            None,
            None,
            FeeApproxStage::OrderIssue,
        )
        .await
        .map_mm_err()?
    }

    // Validate needed trade fee in maker platform coin, total for atomic swap and LR_1
    // For the atomic swap Taker may need some amount to spend the Maker payment:
    let mut maker_coin_trade_fee = maker_coin
        .get_receiver_trade_fee(FeeApproxStage::OrderIssue)
        .compat()
        .await
        .mm_err(|e| CheckBalanceError::from_trade_preimage_error(e, maker_coin.ticker()))
        .map_mm_err()?;

    // Add tx fee for LR_1:
    if let (Some(lr_swap_1), MmCoinEnum::EthCoin(eth_coin)) = (&req.lr_swap_1, &maker_coin) {
        let lr_trade_fee = eth_coin
            .estimate_trade_fee(
                lr_swap_1.swap_details.gas.unwrap_or_default().into(),
                FeeApproxStage::OrderIssue,
            )
            .await
            .map_mm_err()?;
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
        check_other_coin_balance_for_swap(&ctx, maker_coin.deref(), None, maker_coin_trade_fee)
            .await
            .map_mm_err()?;
    }

    // Validate LR step 1 (after the atomic swap):
    let lr_swap_params_1 = if let Some(lr_swap_1) = req.lr_swap_1 {
        let (src_coin, src_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_source_token()?)
            .await
            .map_mm_err()?;
        let (dst_coin, dst_contract) = get_coin_for_one_inch(&ctx, &lr_swap_1.get_destination_token()?)
            .await
            .map_mm_err()?;
        let src_chain_id = src_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        let dst_chain_id = dst_coin.chain_id().ok_or(ExtApiRpcError::ChainNotSupported)?;
        check_if_one_inch_supports_pair(src_chain_id, dst_chain_id).map_mm_err()?;

        // Ensure correct token routing from atomic swap to LR_1
        if atomic_swap_params.maker_coin() != lr_swap_1.get_source_token()? {
            return MmError::err(ExtApiRpcError::InvalidParam(format!(
                "Connecting tokens must be same: maker coin: {}, LR_1 src_token: {}",
                atomic_swap_params.maker_coin(),
                lr_swap_1.get_source_token()?
            )));
        }
        let single_address = src_coin.derivation_method().single_addr_or_err().await.map_mm_err()?;
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
            gas: lr_swap_1.swap_details.gas.unwrap_or_default(), // this field not used yet in LR_1 step
        })
    } else {
        None
    };
    let swap_uuid = lp_start_agg_taker_swap(ctx, lr_swap_params_0, lr_swap_params_1, atomic_swap_params)
        .await
        .map_mm_err()?;
    Ok(LrExecuteRoutedTradeResponse { uuid: swap_uuid })
    // TODO: add different reasons for AbortReason instead SomeReason
    // TODO: ExtApiError - reduce included big OneInchApi err
    // TODO: replace LrSwapError::InternalError with concrete errors
    // TODO: add LR_1 txfee check - added
    // TODO: return 'allowance' rpc result in MmNumber coin units
    // TODO: return 'allowance not enough' error in MmNumber coin units
    // TODO: add set swap tx policy to enable_coin rpc - done
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
        base, rel, method, volume.as_ref().map(|v|v.to_decimal()),
        lr_1_src, lr_1_dst, lr_1_src_amount.map(|v| v.to_decimal()),
    );
}
