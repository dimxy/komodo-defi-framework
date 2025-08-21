//! Helpers for 'aggregated taker swaps with liquidity routing' (LR).

use coins::RawTransactionRes;
use common::executor::Timer;
use common::{block_on, log};
use ethereum_types::H256;
use mm2_number::BigDecimal;
use mm2_rpc::data::legacy::MatchBy;
use mm2_test_helpers::for_tests::orderbook_v2;
use mm2_test_helpers::for_tests::{my_balance, MarketMakerIt};
use mm2_test_helpers::structs::lr_test_structs::{
    ClassicSwapResponse, LrExecuteRoutedTradeResponse, LrFindBestQuoteResponse,
};
use mm2_test_helpers::structs::SetPriceResult;
use mm2_test_helpers::structs::{OrderbookV2Response, RpcOrderbookEntryV2, RpcV2Response, SetPriceResponse};
use serde_json::{json, Value};
use std::str::FromStr;
use uuid::Uuid;
use web3::transports::Http;
use web3::Web3;

/// Not used yet
/// We may use this to top-up a token before an agg swap test,
/// for e.g. if we want to do liquidity routing from this token
#[allow(unused)]
pub async fn top_up_token(mm: &mut MarketMakerIt, base: &str, rel: &str, amount: f64) -> Result<(), String> {
    make_1inch_swap(mm, base, rel, amount).await?;
    Ok(())
}

/// Not used yet
/// Make 1inch classic swap to top-up tokens for the test
/// Use default tx fee set by the 1inch rpc (should be medium)
#[allow(unused)]
pub async fn make_1inch_swap(mm: &mut MarketMakerIt, base: &str, rel: &str, amount: f64) -> Result<String, String> {
    common::log::info!("Issue {}/{} 1inch swap request", base, rel);
    let rc = mm
        .rpc(&json!({
            "userpass": mm.userpass,
            "method": "experimental::1inch_v6_0::classic_swap_create",
            "mmrpc": "2.0",
            "params": {
                "base": base,
                "rel": rel,
                "amount": amount,
                "slippage": 1
            }
        }))
        .await
        .unwrap();
    if !rc.0.is_success() {
        return Err(format!("cannot create 1inch swap: {}", rc.1));
    }
    let swap_resp: RpcV2Response<ClassicSwapResponse> = serde_json::from_str(&rc.1).unwrap();
    let tx_fields = swap_resp.result.tx.unwrap();

    let rc = mm
        .rpc(&json!({
            "userpass": mm.userpass,
            "method": "sign_raw_transaction",
            "mmrpc": "2.0",
            "params": {
                "coin": base,
                "type": "ETH",
                "tx": {
                    "to": tx_fields.to,
                    "value": tx_fields.value,
                    "gas_limit": tx_fields.gas,
                    "pay_for_gas": {
                        "tx_type": "Legacy",
                        "gas_price": tx_fields.gas_price
                    },
                    "data": tx_fields.data
                }
            }
        }))
        .await
        .unwrap();
    if !rc.0.is_success() {
        return Err(format!("cannot sign 1inch swap tx: {}", rc.1));
    }
    let sign_resp: RpcV2Response<RawTransactionRes> = serde_json::from_str(&rc.1).unwrap();

    let rc = block_on(mm.rpc(&json!({
        "userpass": mm.userpass,
        "method": "send_raw_transaction",
        "coin": base,
        "tx_hex": sign_resp.result.tx_hex
    })))
    .unwrap();
    if !rc.0.is_success() {
        return Err(format!("could not do 1inch send_raw_transaction: {}", rc.1));
    }
    let send_resp: Value = serde_json::from_str(&rc.1).unwrap();
    Ok(send_resp["tx_hash"].to_string())
}

#[allow(unused)]
pub async fn wait_for_confirmation_node(node: &str, tx_hashes: &[&str], timeout: u32) -> Result<(), String> {
    const RETRY_DELAY: u32 = 5;
    let mut waited = 0;
    let mut unconfirmed = tx_hashes
        .iter()
        .map(|hash_str| H256::from_str(hash_str).unwrap())
        .collect::<Vec<_>>();
    loop {
        let mut unconfirmed_new = vec![];
        for hash in unconfirmed {
            if let Ok(receipt) = Web3::new(Http::new(node).unwrap())
                .eth()
                .transaction_receipt(hash)
                .await
            {
                if receipt.is_some() {
                    continue;
                }
            }
            unconfirmed_new.push(hash);
        }
        if unconfirmed_new.is_empty() {
            return Ok(());
        }
        if waited > timeout {
            break;
        }
        Timer::sleep(RETRY_DELAY as f64).await;
        waited += RETRY_DELAY;
        unconfirmed = unconfirmed_new;
    }
    Err("timeout waiting for confirmations".to_owned())
}

/// issue setprice request on Bob side by setting base/rel price
pub async fn create_maker_order(
    maker: &mut MarketMakerIt,
    base: &str,
    rel: &str,
    price: f64,
    volume: f64,
) -> Result<SetPriceResult, String> {
    common::log::info!("Issue maker {}/{} setprice request", base, rel);
    let rc = maker
        .rpc(&json!({
            "userpass": maker.userpass,
            "method": "setprice",
            "base": base,
            "rel": rel,
            "price": price,
            "volume": volume
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        let resp: SetPriceResponse = serde_json::from_str(&rc.1).unwrap();
        Ok(resp.result)
    } else {
        Err(rc.1)
    }
}

pub async fn wait_for_orderbook(
    mm: &mut MarketMakerIt,
    base: &str,
    rel: &str,
    uuid: &Uuid,
    timeout: u32,
) -> Result<RpcOrderbookEntryV2, String> {
    let mut waited = 0;
    const RETRY_DELAY: u32 = 5;
    loop {
        let orderbook_v2 = orderbook_v2(mm, base, rel).await;
        let orderbook_v2: RpcV2Response<OrderbookV2Response> = serde_json::from_value(orderbook_v2).unwrap();
        if let Some(e) = orderbook_v2.result.asks.iter().find(|ask| &ask.entry.uuid == uuid) {
            return Ok(e.entry.clone());
        }
        if let Some(e) = orderbook_v2.result.bids.iter().find(|bid| &bid.entry.uuid == uuid) {
            return Ok(e.entry.clone());
        }
        if waited > timeout {
            break;
        }
        Timer::sleep(RETRY_DELAY as f64).await;
        waited += RETRY_DELAY;
    }
    Err(format!("no uuid {uuid} in orderbook"))
}

pub async fn find_best_lr_swap(
    taker: &mut MarketMakerIt,
    user_base: &str,
    asks: &Value,
    bids: &Value,
    volume: &BigDecimal,
    method: &str,
    user_rel: &str,
) -> Result<LrFindBestQuoteResponse, String> {
    log!("Issue find_best_quote {}/{} request", user_base, user_rel);
    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "experimental::liquidity_routing::find_best_quote",
            "mmrpc": "2.0",
            "params": {
                "user_base": user_base,
                "volume": volume,
                "asks": asks,
                "bids": bids,
                "method": method,
                "user_rel": user_rel
            }
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        let resp: RpcV2Response<LrFindBestQuoteResponse> = serde_json::from_str(&rc.1).unwrap();
        Ok(resp.result)
    } else {
        Err(rc.1)
    }
}

pub async fn create_and_start_agg_taker_swap(
    taker: &mut MarketMakerIt,
    slippage: f32,
    best_quote: LrFindBestQuoteResponse,
) -> Result<Uuid, String> {
    let lr_swap_0 = best_quote.lr_data_0.map(|swap_details| {
        json!({
            "slippage": slippage,
            "swap_details": swap_details
        })
    });
    let lr_swap_1 = best_quote.lr_data_1.map(|swap_details| {
        json!({
            "slippage": slippage,
            "swap_details": swap_details
        })
    });
    let mut atomic_swap = best_quote.atomic_swap;
    // We discussed that we should not match a specific order.
    // I set this for now but we need to think about this again:
    // if a different order is taken the LR calculations may be wrong for it.
    atomic_swap.match_by = Some(MatchBy::Orders([atomic_swap.order_uuid].into()));
    log!(
        "Issue execute_routed_trade {} {}/{} request",
        atomic_swap.method,
        atomic_swap.base,
        atomic_swap.rel
    );
    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "experimental::liquidity_routing::execute_routed_trade",
            "mmrpc": "2.0",
            "params": {
                "lr_swap_0": lr_swap_0,
                "atomic_swap": atomic_swap,
                "lr_swap_1": lr_swap_1,
            }
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        let resp: RpcV2Response<LrExecuteRoutedTradeResponse> = serde_json::from_str(&rc.1).unwrap();
        Ok(resp.result.uuid)
    } else {
        Err(rc.1)
    }
}

/// Helper function requesting my swap status and checking it's events
pub fn check_my_agg_swap_final_status(status_response: &Value) {
    assert!(status_response["result"]["is_finished"].as_bool().unwrap());

    let events = status_response["result"]["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .any(|item| item["event_type"].as_str() == Some("Completed")),
        "swap not completed successfully"
    );
    assert!(
        !events.iter().any(|item| item["event_type"].as_str() == Some("Aborted")),
        "swap was aborted"
    );
}

pub async fn cancel_order(mm: &MarketMakerIt, uuid: &Uuid) {
    mm.rpc(&json! ({
        "userpass": mm.userpass,
        "method": "cancel_order",
        "uuid": uuid,
    }))
    .await
    .unwrap();
}

pub async fn set_swap_gas_fee_policy(mm: &MarketMakerIt, coin: &str, fee_policy: &str) -> Result<(), String> {
    let rc = mm
        .rpc(&json! ({
            "userpass": mm.userpass,
            "mmrpc": "2.0",
            "method": "set_swap_gas_fee_policy",
            "params": {
                "coin": coin,
                "swap_gas_fee_policy": fee_policy
            }
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        Ok(())
    } else {
        Err(rc.1)
    }
}

#[allow(unused)]
pub fn check_results<T: std::fmt::Debug>(results: &[Result<T, String>], msg: &str) {
    let mut err_msg = "".to_owned();
    for (i, r) in results.iter().enumerate() {
        if r.is_err() {
            err_msg += format!(" result[{i}]={}", r.as_ref().unwrap_err()).as_str();
        }
    }
    if !err_msg.is_empty() {
        log!("{},{}", msg, err_msg);
        panic!("{}", msg);
    }
}

pub fn print_balances(mm: &MarketMakerIt, msg: &str, tickers: &[&str]) {
    for ticker in tickers {
        let balance = block_on(my_balance(mm, ticker));
        log!(
            "{} {}: address={:?} balance={:?} unspendable_balance={:?}",
            msg,
            ticker,
            balance.address,
            balance.balance,
            balance.unspendable_balance
        );
    }
}

pub fn print_quote_resp(quote: &LrFindBestQuoteResponse) {
    log!(
        "Found best quote for swap with LR:
        LR_0: src_token={:?} src_amount={:?} dst_token={:?} dst_amount={:?}
        atomic swap params: base={} rel={} method={} price={} volume={:?}
        LR_1: src_token={:?} src_amount={:?} dst_token={:?} dst_amount={:?}",
        quote
            .lr_data_0
            .as_ref()
            .and_then(|data| data.src_token.as_ref().unwrap().symbol_kdf.as_ref()),
        quote.lr_data_0.as_ref().map(|data| data.src_amount.to_decimal()),
        quote
            .lr_data_0
            .as_ref()
            .and_then(|data| data.dst_token.as_ref().unwrap().symbol_kdf.as_ref()),
        quote.lr_data_0.as_ref().map(|data| &data.dst_amount.amount),
        quote.atomic_swap.base,
        quote.atomic_swap.rel,
        quote.atomic_swap.method,
        quote.atomic_swap.price.to_decimal(),
        quote.atomic_swap.volume.as_ref().map(|v| v.to_decimal()),
        quote
            .lr_data_1
            .as_ref()
            .and_then(|data| data.src_token.as_ref().unwrap().symbol_kdf.as_ref()),
        quote.lr_data_1.as_ref().map(|data| data.src_amount.to_decimal()),
        quote
            .lr_data_1
            .as_ref()
            .and_then(|data| data.dst_token.as_ref().unwrap().symbol_kdf.as_ref()),
        quote.lr_data_1.as_ref().map(|data| &data.dst_amount.amount),
    );
}
