#![allow(unused)]

use bitcrypto::dhash160;
use coins::utxo::UtxoCommonOps;
use coins::RawTransactionRes;
use coins::{ConfirmPaymentInput, DexFee, FundingTxSpend, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs,
            MakerCoinSwapOpsV2, MarketCoinOps, ParseCoinAssocTypes, RefundFundingSecretArgs,
            RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs, RefundTakerPaymentArgs,
            SendMakerPaymentArgs, SendTakerFundingArgs, SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, Transaction,
            ValidateMakerPaymentArgs, ValidateTakerFundingArgs};
use common::executor::Timer;
use common::{block_on, block_on_f01, log, now_sec, DEX_FEE_ADDR_RAW_PUBKEY};
use crypto::privkey::key_pair_from_seed;
use crypto::{Bip44Chain, CryptoCtx, CryptoCtxError, GlobalHDAccountArc, KeyPairPolicy};
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::RpcOrderbookEntry;
use mm2_test_helpers::for_tests::{active_swaps, check_recent_swaps, coins_needed_for_kickstart, disable_coin,
                                  disable_coin_err, doc_conf, enable_electrum_json, enable_eth_coin, enable_native,
                                  get_locked_amount, mm_dump, my_swap_status, mycoin1_conf, mycoin_conf, polygon_conf,
                                  start_swaps, wait_for_swap_finished, MarketMakerIt, Mm2TestConf,
                                  SwapV2TestContracts, TestNode, DOC, POLYGON_MAINNET_NODES,
                                  POLYGON_MAINNET_SWAP_CONTRACT, POLYGON_MAINNET_SWAP_V2_MAKER_CONTRACT,
                                  POLYGON_MAINNET_SWAP_V2_NFT_CONTRACT, POLYGON_MAINNET_SWAP_V2_TAKER_CONTRACT};
use mm2_test_helpers::structs::{MmNumberMultiRepr, SetPriceResult};
use script::{Builder, Opcode};
use serde_json::{json, Value};
use serialization::serialize;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;
use web3::transports::Http;
use web3::Web3;
//use crate::generate_utxo_coin_with_privkey;
use coins::eth::EthCoin;
use ethereum_types::{Address, H160, H256, U256};
use lazy_static::lazy_static;
use mm2_test_helpers::electrums::{doc_electrums, marty_electrums};
use mm2_test_helpers::for_tests::{best_orders_v2, best_orders_v2_by_number, enable_eth_coin_v2, orderbook_v2};
use mm2_test_helpers::structs::{ClassicSwapCreateRequest, ClassicSwapDetails, ClassicSwapResponse, GetPublicKeyResult,
                                LrBestQuoteResponse, LrFillMakerOrderResponse, OrderbookV2Response,
                                RpcOrderbookEntryV2, RpcV2Response, SetPriceResponse};
use std::collections::HashSet;

lazy_static! {
    pub static ref POLYGON_WEB3: Web3<Http> = Web3::new(Http::new(POLYGON_MAINNET_NODES[0]).unwrap());
}

#[test]
fn test_aggregated_swap_mainnet_polygon_utxo() {
    let bob_passphrase = std::env::var("BOB_DIMXY").expect("BOB_DIMXY env must be set");
    let alice_passphrase = std::env::var("ALICE_DIMXY").expect("ALICE_DIMXY env must be set");

    let bob_priv_key = key_pair_from_seed(&bob_passphrase).unwrap().private().secret;
    let alice_priv_key = key_pair_from_seed(&alice_passphrase).unwrap().private().secret;

    let token_1_conf = json!({
        "coin": "DAI-PLG20",
        "name": "dai_plg20",
        "derivation_path": "m/44'/966'",
        "chain_id": 137,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "MATIC",
                "contract_address": "0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063"
            }
        }
    });
    let token_2_conf = json!({
        "coin": "1INCH-PLG20", // Note: crossprices API returns unsupported token
        "name": "1inch_plg20",
        "derivation_path": "m/44'/966'",
        "chain_id": 137,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "MATIC",
                "contract_address": "0x9c2C5fd7b07E95EE044DDeba0E97a665F142394f"
            }
        }
    });
    let token_3_conf = json!({
        "coin": "AGIX-PLG20", // Note: crossprices API returns unsupported token
        "name": "agix_plg20",
        "derivation_path": "m/44'/966'",
        "chain_id": 137,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "MATIC",
                "contract_address": "0x190Eb8a183D22a4bdf278c6791b152228857c033"
            }
        }
    });

    let token_1_ticker = token_1_conf["coin"].as_str().unwrap().to_owned();
    let token_2_ticker = token_2_conf["coin"].as_str().unwrap().to_owned();
    let token_3_ticker = token_3_conf["coin"].as_str().unwrap().to_owned();

    let bob_coins = json!([doc_conf(), polygon_conf(), token_1_conf, token_2_conf, token_3_conf]);
    let bob_conf = Mm2TestConf::seednode(&bob_passphrase, &bob_coins); // Using legacy swaps until TPU contracts deployed on POLYGON
    let mut mm_bob = block_on(MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, None)).unwrap();

    let alice_coins = json!([doc_conf(), polygon_conf(), token_1_conf, token_2_conf, token_3_conf]);
    let mut alice_conf = Mm2TestConf::light_node(&alice_passphrase, &alice_coins, &[&mm_bob.ip.to_string()]);
    alice_conf.conf["1inch_api"] = "https://api.1inch.dev".into();
    let mut mm_alice = block_on(MarketMakerIt::start_async(
        alice_conf.conf,
        alice_conf.rpc_password,
        None,
    ))
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_dump(&mm_alice.log_path);
    log!("Alice log path: {}", mm_alice.log_path.display());
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!("Bob log path: {}", mm_bob.log_path.display());

    let contracts_v2 = SwapV2TestContracts {
        maker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_MAKER_CONTRACT.to_owned(),
        taker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_TAKER_CONTRACT.to_owned(),
        nft_maker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_NFT_CONTRACT.to_owned(),
    };

    let polygon_nodes = POLYGON_MAINNET_NODES
        .iter()
        .map(|ip| TestNode { url: (*ip).to_owned() })
        .collect::<Vec<_>>();

    let bob_enable_tokens = block_on(enable_eth_coin_v2(
        &mm_bob,
        "MATIC",
        POLYGON_MAINNET_SWAP_CONTRACT,
        contracts_v2.clone(),
        None,
        &polygon_nodes,
        json!([
            { "ticker": &token_1_ticker },
            { "ticker": &token_2_ticker },
            { "ticker": &token_3_ticker }
        ]),
    ));
    let bob_enable_doc = block_on(enable_electrum_json(&mm_bob, DOC, false, doc_electrums()));
    println!(
        "Bob address={:?}, DOC balance={:?} unspendable_balance={:?}",
        bob_enable_doc["address"].as_str().unwrap(),
        bob_enable_doc["balance"],
        bob_enable_doc["unspendable_balance"]
    );

    let alice_enable_tokens = block_on(enable_eth_coin_v2(
        &mm_alice,
        "MATIC",
        POLYGON_MAINNET_SWAP_CONTRACT,
        contracts_v2,
        None,
        &polygon_nodes,
        json!([
            { "ticker": &token_1_ticker },
            { "ticker": &token_2_ticker },
            { "ticker": &token_3_ticker }
        ]),
    ));
    let alice_enable_doc = block_on(enable_electrum_json(&mm_alice, DOC, false, doc_electrums()));

    let alice_eth_info = alice_enable_tokens["result"]["eth_addresses_infos"]
        .as_object()
        .unwrap();
    let alice_erc20_info = alice_enable_tokens["result"]["erc20_addresses_infos"]
        .as_object()
        .unwrap();
    println!(
        "Alice address={}, MATIC balance={:?}\ntoken balances:\n{:?}={:?}\n{:?}={:?}\n{:?}={:?}",
        alice_eth_info.iter().next().unwrap().0,
        alice_eth_info.iter().next().unwrap().1["balances"].as_object().unwrap(),
        token_1_ticker,
        alice_erc20_info.iter().next().unwrap().1["balances"][token_1_ticker.clone()]
            .as_object()
            .unwrap(),
        token_2_ticker,
        alice_erc20_info.iter().next().unwrap().1["balances"][token_2_ticker.clone()]
            .as_object()
            .unwrap(),
        token_3_ticker,
        alice_erc20_info.iter().next().unwrap().1["balances"][token_3_ticker.clone()]
            .as_object()
            .unwrap(),
    );

    let order_res_1 = block_on(create_maker_order(&mut mm_bob, DOC, &token_1_ticker, 0.002));
    let order_res_2 = block_on(create_maker_order(&mut mm_bob, DOC, &token_2_ticker, 0.002));
    let order_res_3 = block_on(create_maker_order(&mut mm_bob, DOC, &token_3_ticker, 0.002));
    if order_res_1.is_err() && order_res_2.is_err() && order_res_3.is_err() {
        println!(
            "maker orders for test all errors:\n{}\n{}\n{}",
            order_res_1.unwrap_err(),
            order_res_2.unwrap_err(),
            order_res_3.unwrap_err()
        );
        panic!("could not create maker orders for test");
    }

    let token_1_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_1_ticker, 60)).unwrap();
    let token_2_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_2_ticker, 60)).unwrap();
    let token_3_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_3_ticker, 60)).unwrap();
    // println!("token_1_order={}", serde_json::to_string(&token_1_order).unwrap());
    // println!("token_2_order={}", serde_json::to_string(&token_2_order).unwrap());
    // println!("token_3_order={}", serde_json::to_string(&token_3_order).unwrap());

    let best_asks = block_on(best_orders_v2_by_number(&mm_alice, DOC, "buy", 10, false));
    // println!("best_asks={}", serde_json::to_string(&best_asks).unwrap());
    let entries = best_asks.result.orders.values().flatten().cloned().collect::<Vec<_>>();
    // println!("best_asks(entries)={}", serde_json::to_string(&entries).unwrap());

    const SWAP_AMOUNT: &str = "0.001234";
    let best_quote = block_on(find_best_lr_swap(&mut mm_alice, DOC, &entries, SWAP_AMOUNT, "MATIC"))
        .expect("best LR quote should be found");
    println!("Found best LR quote={}", serde_json::to_string(&best_quote).unwrap());

    let agg_uuid = block_on(start_agg_swap(
        &mut mm_alice,
        DOC,
        Some(ClassicSwapCreateRequest::new_from_details(
            &best_quote.lr_swap_details,
            SWAP_AMOUNT,
            1.0,
        )),
        best_quote.best_order,
        None,
        SWAP_AMOUNT.parse().unwrap(),
    ))
    .unwrap();
    log!("Aggregated swap uuid {:?} started", agg_uuid);

    block_on(Timer::sleep(1.0));

    let active_swaps_alice = block_on(active_swaps(&mm_alice));
    assert_eq!(active_swaps_alice.uuids, vec![agg_uuid]);

    block_on(wait_for_swap_finished(&mm_alice, &agg_uuid.to_string(), 120)); // Only taker has the aggregated swap
    println!("Aggregated lr swap {:?} finished", agg_uuid);

    let taker_swap_status = block_on(my_swap_status(&mm_alice, &agg_uuid.to_string())).unwrap();
    log!(
        "final taker_swap_status {}",
        serde_json::to_string(&taker_swap_status).unwrap()
    );
    check_my_agg_swap_final_status(&taker_swap_status);

    //assert!(taker_swap_status[])

    //block_on(check_recent_swaps(&mm_bob, 1));
    //block_on(check_recent_swaps(&mm_alice, 1));
    block_on(cancel_order(&mm_bob, &token_1_order.uuid));
    block_on(cancel_order(&mm_bob, &token_2_order.uuid));
    block_on(cancel_order(&mm_bob, &token_3_order.uuid));
    block_on(Timer::sleep(10.0)); // wait for orderbook update

    // Disabling coins on both nodes should be successful at this point
    block_on(disable_coin(&mm_bob, "MATIC", false));
    block_on(disable_coin(&mm_bob, DOC, false));
    block_on(disable_coin(&mm_alice, "MATIC", false));
    block_on(disable_coin(&mm_alice, DOC, false));
}

async fn top_up_token(mm: &mut MarketMakerIt, base: &str, rel: &str, amount: f64) -> Result<(), String> {
    make_1inch_swap(mm, base, rel, amount).await?;
    Ok(())
}

/// Make 1inch classic swap to top-up tokens for the test
/// Use default tx fee set by the 1inch rpc (should be medium)
async fn make_1inch_swap(mm: &mut MarketMakerIt, base: &str, rel: &str, amount: f64) -> Result<String, String> {
    common::log::info!("Issue {}/{} 1inch swap request", base, rel);
    let rc = mm
        .rpc(&json!({
            "userpass": mm.userpass,
            "method": "1inch_v6_0_classic_swap_create",
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

async fn wait_for_confirmations(tx_hashes: &[&str], timeout: u32) -> Result<(), String> {
    const RETRY_DELAY: u32 = 5;
    let mut waited = 0;
    let mut unconfirmed = tx_hashes
        .iter()
        .map(|hash_str| H256::from_str(hash_str).unwrap())
        .collect::<Vec<_>>();
    loop {
        let mut unconfirmed_new = vec![];
        for hash in unconfirmed {
            if let Ok(receipt) = POLYGON_WEB3.eth().transaction_receipt(hash).await {
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
async fn create_maker_order(
    maker: &mut MarketMakerIt,
    base: &str,
    rel: &str,
    volume: f64,
) -> Result<SetPriceResult, String> {
    common::log::info!("Issue maker {}/{} sell request", base, rel);
    let rc = maker
        .rpc(&json!({
            "userpass": maker.userpass,
            "method": "setprice",
            "base": base,
            "rel": rel,
            "price": volume,
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

async fn wait_for_orderbook(
    taker: &mut MarketMakerIt,
    base: &str,
    rel: &str,
    timeout: u32,
) -> Result<RpcOrderbookEntryV2, String> {
    let mut waited = 0;
    const RETRY_DELAY: u32 = 5;
    loop {
        let orderbook_v2 = orderbook_v2(taker, base, rel).await;
        let orderbook_v2: RpcV2Response<OrderbookV2Response> = serde_json::from_value(orderbook_v2).unwrap();
        if !orderbook_v2.result.asks.is_empty() {
            return Ok(orderbook_v2.result.asks[0].entry.clone());
        }
        if waited > timeout {
            break;
        }
        Timer::sleep(RETRY_DELAY as f64).await;
        waited += RETRY_DELAY;
    }
    Err("no ask in orderbook".to_owned())
}

async fn find_best_lr_swap(
    taker: &mut MarketMakerIt,
    base: &str,
    asks: &[RpcOrderbookEntryV2],
    amount: &str,
    my_token: &str,
) -> Result<LrBestQuoteResponse, String> {
    common::log::info!("Issue lr_best_quote {}/{} request", base, my_token);
    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "preview::lr_best_quote",
            "mmrpc": "2.0",
            "params": {
                "base": base,
                "amount": amount,
                "asks": asks,
                "my_token": my_token
            }
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        let resp: RpcV2Response<LrBestQuoteResponse> = serde_json::from_str(&rc.1).unwrap();
        Ok(resp.result)
    } else {
        Err(rc.1)
    }
}

async fn start_agg_swap(
    taker: &mut MarketMakerIt,
    base: &str,
    lr_swap_0: Option<ClassicSwapCreateRequest>,
    order_entry: RpcOrderbookEntryV2,
    lr_swap_1: Option<ClassicSwapCreateRequest>,
    volume: f64,
) -> Result<Uuid, String> {
    common::log::info!("Issue taker {}/{} lr_fill_order request", base, order_entry.coin);

    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "preview::lr_fill_order",
            "mmrpc": "2.0",
            "params": {
                "sell_buy_req": {
                    "base": base,
                    "rel": order_entry.coin,
                    "price": order_entry.price.rational,
                    "volume": volume,
                    "method": "buy",
                    "match_by": {
                        "type": "Orders",
                        "data": [order_entry.uuid]
                    }
                },
                "lr_swap_0": lr_swap_0,
                "lr_swap_1": lr_swap_1
            }
        }))
        .await
        .unwrap();
    if rc.0.is_success() {
        let resp: RpcV2Response<LrFillMakerOrderResponse> = serde_json::from_str(&rc.1).unwrap();
        Ok(resp.result.uuid)
    } else {
        Err(rc.1)
    }
}

/// Helper function requesting my swap status and checking it's events
fn check_my_agg_swap_final_status(status_response: &Value) {
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

async fn cancel_order(mm: &MarketMakerIt, uuid: &Uuid) {
    let cancel_rc = mm
        .rpc(&json! ({
            "userpass": mm.userpass,
            "method": "cancel_order",
            "uuid": uuid,
        }))
        .await
        .unwrap();
}
