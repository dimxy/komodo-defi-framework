//! LR implementation code 
use core::cmp::Ordering;
use std::collections::HashMap;
use futures::future::join_all;
use mm2_err_handle::prelude::*;
use mm2_core::mm_ctx::MmArc;
use mm2_number::MmNumber;
//use mm2_number::BigDecimal;
use coins::lp_coinfind_or_err;
use coins::MmCoin;
use coins::Ticker;
use coins::eth::wei_from_big_decimal;
use trading_api::one_inch_api::client::ApiClient;
use trading_api::one_inch_api::errors::ApiClientError;
use trading_api::one_inch_api::types::{ClassicSwapData, ClassicSwapQuoteParams};
use crate::lp_ordermatch::AggregatedOrderbookEntryV2;
use crate::rpc::lp_commands::one_inch::rpcs::get_coin_for_one_inch;
use crate::rpc::lp_commands::one_inch::errors::ApiIntegrationRpcError;
use ethereum_types::U256;

type TickerAmount = (Ticker, U256);
type QuotePrice = (Ticker, Ticker, MmNumber);
type TickerOrderMap = HashMap<Ticker, AggregatedOrderbookEntryV2>;
type TickerOrder = (Ticker, AggregatedOrderbookEntryV2);

/// Calculate amounts of destination tokens required to fill orders and get user requested base_amount
/// Basically multiply base_amount by order price.
async fn calc_destination_token_amounts(ctx: &MmArc, orders: &HashMap<Ticker, AggregatedOrderbookEntryV2>, base_amount: &MmNumber) -> Vec<TickerAmount> {
    let mut token_amounts = vec![];
    for (token, order) in orders {
        let price: MmNumber = order.entry.price.rational.clone().into();
        let dest_amount = (base_amount * &price).to_decimal();
        let coin = lp_coinfind_or_err(ctx, &token).await.unwrap();
        token_amounts.push((token.clone(), wei_from_big_decimal(&dest_amount, coin.decimals()).unwrap()));
    }
    token_amounts
}

/// Estimate destination-token / my-token price(s) by getting quotes of known destination-token amounts for my-tokens from LR provider 
/// and calculating the destination-token prices (by dividing destination-token amount on result)
/// Assuming the RPC-level code ensures that relation src_tokens : dst_tokens will never be M:N (but only 1:M or M:1)
async fn estimate_destination_token_prices(ctx: &MmArc, src_tokens: &Vec<Ticker>, dst_tokens: &Vec<TickerAmount>) -> MmResult<Vec<QuotePrice>, ApiIntegrationRpcError> {
    let mut quote_futs = vec![];
    let mut src_dst = vec![];
    for src in src_tokens {
        let (_src_coin, src_contract) = get_coin_for_one_inch(ctx, &src).await?;
        for dst in dst_tokens {
            let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, &dst.0).await?;
            // Run 'reversed' dst / src query:
            let query_params = ClassicSwapQuoteParams::new(dst_contract, src_contract.clone(), dst.1.to_string())
                .build_query_params()
                .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(dst_coin.decimals())))?;
            let fut = ApiClient::new(ctx.clone())
                .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(dst_coin.decimals())))?
                .call_swap_api::<ClassicSwapData>(
                    dst_coin.chain_id(),
                    ApiClient::get_quote_method().to_owned(),
                    Some(query_params),
                );
            quote_futs.push(fut);
            src_dst.push((src.clone(), dst));
        }
    }
    let quotes = join_all(quote_futs).await;
    
    // Calculate dest-token price and return it as (dest, source, price) quote
    let dest_quotes = quotes.into_iter().zip(src_dst.iter()).map(|(quote, (src, dst))| {
        if let Ok(quote) = quote {
            let src_amount_str = quote.dst_amount.to_string(); // quote.dst_amount is a name for token received
            let dst_amount_str = dst.1.to_string();
            let src_amount = MmNumber::from(src_amount_str.as_str());
            let dst_amount = MmNumber::from(dst_amount_str.as_str());
            let dst_price = dst_amount / src_amount; // TODO: checked_div
            (dst.0.clone(), src.clone(), dst_price)
        } else {
            (dst.0.clone(), src.clone(), MmNumber::from(0))
        }

    }).collect::<Vec<_>>();
    Ok(dest_quotes)
}

fn calc_source_token_amounts(ctx: &MmArc, dst_quotes: &Vec<QuotePrice>, dst_token_amounts: &Vec<TickerAmount>) -> Vec<TickerAmount> {
    dst_quotes.iter().zip(dst_token_amounts).map(|(dst_quote, dst_token_amount)| {
        let src_amount = &dst_quote.2;
        let dst_amount = MmNumber::from(dst_token_amount.1.to_string().as_str());
        let price = &dst_amount / src_amount; // TODO: checked_div
        let price = U256::from_dec_str(price.to_ratio().to_integer().to_string().as_str()).unwrap_or_default();
        (dst_quote.1.clone(), price)
    }).collect::<Vec<_>>()
}

/// run 1inch requests to get several quotes to convert tokens for LR
/// Assuming the src_tokens : dst_tokens relation is either 1:M or M:1 (assured at RPC)
async fn run_lr_quotes(ctx: &MmArc, src_token_amounts: &Vec<TickerAmount>, dst_tokens: &Vec<Ticker>) -> MmResult<Vec<ClassicSwapData>, ApiIntegrationRpcError> {
    let mut quote_futs = vec![];
    for src in src_token_amounts {
        let (_src_coin, src_contract) = get_coin_for_one_inch(ctx, &src.0).await?;
        for dst in dst_tokens {
            let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, &dst).await?;
            // Run 'reversed' dst / src query:
            let query_params = ClassicSwapQuoteParams::new(dst_contract, src_contract.clone(), src.1.to_string())
                .build_query_params()
                .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(dst_coin.decimals())))?;
            let fut = ApiClient::new(ctx.clone())
                .mm_err(|api_err| ApiIntegrationRpcError::from_api_error(api_err, Some(dst_coin.decimals())))?
                .call_swap_api::<ClassicSwapData>(
                    dst_coin.chain_id(),
                    ApiClient::get_quote_method().to_owned(),
                    Some(query_params),
                );
            quote_futs.push(fut);
        }
    }
    let result: MmResult<Vec<ClassicSwapData>, ApiClientError> = join_all(quote_futs).await.into_iter().collect();
    result.mm_err(|cli_err| ApiIntegrationRpcError::from_api_error(cli_err, None))
}

/// Select the best swap path, by total price (order and LR swap)
/// Assuming lr_swaps to orders relation is M:M ensured at the RPC
fn select_best_swap(
    src_token_amount: &TickerAmount,
    lr_swaps: &Vec<ClassicSwapData>,
    orders: &HashMap<Ticker, AggregatedOrderbookEntryV2>
) -> MmResult<(ClassicSwapData, AggregatedOrderbookEntryV2, MmNumber), ApiIntegrationRpcError> {
    // Comparison fn, to select best swap path as minimum of LR swap amount and fee and ordinary swap amount
    let calc_total_price = |lr_swap: &ClassicSwapData, order: &AggregatedOrderbookEntryV2| {
        const CONVERSION_ERR: &'static str = "Conversion Error";
        let src_amount = MmNumber::from(src_token_amount.1.to_string().as_str());
        //let dest_amount = U256::from_dec_str(&lr_swap.dst_amount).map_to_mm(|_| ApiIntegrationRpcError::SomeError(CONVERSION_ERR.to_owned()))?;
        let order_price: MmNumber = order.entry.price.rational.clone().into();
        let order_volume = MmNumber::from(lr_swap.dst_amount.to_string().as_str());
        let order_amount = order_price * order_volume;
        let total_price = src_amount / order_amount; // TODO: checked_div
        total_price
    };

    if lr_swaps.len() != orders.len() {
        return MmError::err(ApiIntegrationRpcError::SomeError("Internal Error (LR swap to orders combination)".to_owned()));
    }

    lr_swaps.iter()
        .zip(orders.iter())
        .map(|(lr_swap, order)| {
            let total_price = calc_total_price(lr_swap, order.1);
            (lr_swap, order.1, total_price) 
        })
        .min_by(|(_,_,price_0), (_,_,price_1)| price_0.cmp(price_1))
        .map(|(lr_swap, order, price)| (lr_swap.clone(), order.clone(), price))
        .ok_or(MmError::new(ApiIntegrationRpcError::SomeError("Best swap not found".to_owned())))
}

/// Find best swap path to buy order 's best "UTXO" coins, including LR quotes to sell my token for the rel tokens from the orders
/// base_amount is amount of UTXO coins user would like to buy
pub async fn find_best_lr_buy_for_multiple_orders(
    ctx: &MmArc, 
    user_token: Ticker, 
    orders: &HashMap<Ticker, AggregatedOrderbookEntryV2>, 
    base_amount: &MmNumber
) -> MmResult<(ClassicSwapData, AggregatedOrderbookEntryV2, MmNumber), ApiIntegrationRpcError> {

    let dst_token_amounts = calc_destination_token_amounts(ctx, orders, base_amount).await;
    let dst_quotes = estimate_destination_token_prices(ctx, &vec![user_token], &dst_token_amounts).await?;
    let src_token_amounts = calc_source_token_amounts(ctx, &dst_quotes, &dst_token_amounts);
    let dst_tokens = dst_token_amounts.iter().map(|t_a| t_a.0.clone()).collect::<Vec<_>>();
    let lr_swaps = run_lr_quotes(ctx, &src_token_amounts, &dst_tokens).await?;

    select_best_swap(&src_token_amounts[0], &lr_swaps, orders)
}

/*
/// Find best swap path to sell order rel "UTXO" coins, selecting best from LR quotes to buy my token for orders' base tokens
/// base_amount is amount of base token
async fn find_best_lr_sell_for_multiple_orders(orders: &HashMap<String, AggregatedOrderbookEntryV2>, user_token: &String, base_amount: &MmNumber) {


    let lr_swaps = run_lr_quotes().await;

    let cmp_fn = |min, e| -> bool {

    };

    let best_swap = lr_swaps.iter().min_by(cmp_fn);
}*/
