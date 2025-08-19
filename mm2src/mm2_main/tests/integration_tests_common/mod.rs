use coins::eth::SwapV2Contracts;
use coins::hd_wallet::AddrToString;
use common::executor::Timer;
#[cfg(target_arch = "wasm32")] use common::log::LogLevel;
use common::{block_on, log, now_ms, wait_until_ms};
use crypto::privkey::key_pair_from_seed;
use ethereum_types::Address as EthAddress;
use http::StatusCode;
use mm2_main::{lp_main, lp_run, LpMainParams};
use mm2_rpc::data::legacy::CoinInitResponse;
use mm2_test_helpers::electrums::{doc_electrums, marty_electrums};
use mm2_test_helpers::for_tests::{create_new_account_status, enable_native as enable_native_impl,
                                  init_create_new_account, MarketMakerIt};
use mm2_test_helpers::structs::{CreateNewAccountStatus, HDAccountAddressId, HDAccountBalanceMap, InitTaskResult,
                                RpcV2Response};
use serde_json::{self, json, Value as Json};
use std::collections::HashMap;
use std::env::var;
#[cfg(target_arch = "wasm32")] use std::str::FromStr;

#[cfg(any(feature = "run-docker-tests", feature = "test-ext-api"))]
pub(crate) mod lr_tests_common;

/// This is not a separate test but a helper used by `MarketMakerIt` to run the MarketMaker from the test binary.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_mm_start() { test_mm_start_impl(); }

#[cfg(not(target_arch = "wasm32"))]
pub fn test_mm_start_impl() {
    if let Ok(conf) = var("_MM2_TEST_CONF") {
        log!("test_mm_start] Starting the MarketMaker...");
        let conf: Json = serde_json::from_str(&conf).unwrap();
        let params = LpMainParams::with_conf(conf);
        let ctx = block_on(lp_main(params, &|_ctx| (), "TEST".into(), "TEST".into())).unwrap();
        block_on(lp_run(ctx));
    }
}

/// Ideally, this function should be replaced everywhere with `enable_electrum_json`.
pub async fn enable_electrum(mm: &MarketMakerIt, coin: &str, tx_history: bool, urls: &[&str]) -> CoinInitResponse {
    use mm2_test_helpers::for_tests::enable_electrum as enable_electrum_impl;

    let value = enable_electrum_impl(mm, coin, tx_history, urls).await;
    serde_json::from_value(value).unwrap()
}

pub async fn enable_electrum_json(
    mm: &MarketMakerIt,
    coin: &str,
    tx_history: bool,
    servers: Vec<Json>,
) -> CoinInitResponse {
    use mm2_test_helpers::for_tests::enable_electrum_json as enable_electrum_impl;

    let value = enable_electrum_impl(mm, coin, tx_history, servers).await;
    serde_json::from_value(value).unwrap()
}

pub async fn enable_native(
    mm: &MarketMakerIt,
    coin: &str,
    urls: &[&str],
    path_to_address: Option<HDAccountAddressId>,
) -> CoinInitResponse {
    let value = enable_native_impl(mm, coin, urls, path_to_address).await;
    serde_json::from_value(value).unwrap()
}

pub async fn enable_coins_rick_morty_electrum(mm: &MarketMakerIt) -> HashMap<&'static str, CoinInitResponse> {
    let mut replies = HashMap::new();
    replies.insert("RICK", enable_electrum_json(mm, "RICK", false, doc_electrums()).await);
    replies.insert(
        "MORTY",
        enable_electrum_json(mm, "MORTY", false, marty_electrums()).await,
    );
    replies
}

pub async fn enable_coins_eth_electrum(
    mm: &MarketMakerIt,
    eth_urls: &[&str],
) -> HashMap<&'static str, CoinInitResponse> {
    let mut replies = HashMap::new();
    replies.insert("RICK", enable_electrum_json(mm, "RICK", false, doc_electrums()).await);
    replies.insert(
        "MORTY",
        enable_electrum_json(mm, "MORTY", false, marty_electrums()).await,
    );
    replies.insert("ETH", enable_native(mm, "ETH", eth_urls, None).await);
    replies
}

pub fn addr_from_enable<'a>(enable_response: &'a HashMap<&str, CoinInitResponse>, coin: &str) -> &'a str {
    &enable_response.get(coin).unwrap().address
}

pub fn rmd160_from_passphrase(passphrase: &str) -> [u8; 20] {
    key_pair_from_seed(passphrase).unwrap().public().address_hash().take()
}

pub async fn create_new_account(
    mm: &MarketMakerIt,
    coin: &str,
    account_id: Option<u32>,
    timeout: u64,
) -> HDAccountBalanceMap {
    let init = init_create_new_account(mm, coin, account_id).await;
    let init: RpcV2Response<InitTaskResult> = serde_json::from_value(init).unwrap();
    let timeout = wait_until_ms(timeout * 1000);

    loop {
        if now_ms() > timeout {
            panic!("{} initialization timed out", coin);
        }

        let status = create_new_account_status(mm, init.result.task_id).await;
        let status: RpcV2Response<CreateNewAccountStatus> = serde_json::from_value(status).unwrap();
        log!("create_new_account_status: {:?}", status);
        match status.result {
            CreateNewAccountStatus::Ok(result) => break result,
            CreateNewAccountStatus::Error(e) => panic!("{} initialization error {:?}", coin, e),
            _ => Timer::sleep(1.).await,
        }
    }
}

#[allow(unused)]
pub async fn enable_eth_coin_v2(
    mm: &MarketMakerIt,
    ticker: &str,
    swap_contract_address: EthAddress,
    swap_v2_contracts: SwapV2Contracts,
    fallback_swap_contract: Option<EthAddress>,
    nodes: &[&str],
    tokens: &[&str],
) -> Json {
    let enable = mm
        .rpc(&json!({
            "userpass": mm.userpass,
            "method": "enable_eth_with_tokens",
            "mmrpc": "2.0",
            "params": {
                "ticker": ticker,
                "mm2": 1,
                "swap_contract_address": swap_contract_address.addr_to_string(),
                "swap_v2_contracts": {
                    "maker_swap_v2_contract": swap_v2_contracts.maker_swap_v2_contract.addr_to_string(),
                    "taker_swap_v2_contract": swap_v2_contracts.taker_swap_v2_contract.addr_to_string(),
                    "nft_maker_swap_v2_contract": swap_v2_contracts.nft_maker_swap_v2_contract.addr_to_string()
                },
                "fallback_swap_contract": fallback_swap_contract.map(|s| s.addr_to_string()),
                "nodes": nodes.iter().map(|url| json!({ "url": url })).collect::<Vec<_>>(),
                "erc20_tokens_requests": tokens.iter().map(|t| json!({"ticker": t})).collect::<Vec<Json>>(),
            }
        }))
        .await
        .unwrap();
    assert_eq!(
        enable.0,
        StatusCode::OK,
        "'enable_eth_with_tokens' failed: {}",
        enable.1
    );
    serde_json::from_str(&enable.1).unwrap()
}
