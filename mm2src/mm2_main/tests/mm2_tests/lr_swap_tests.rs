#![allow(unused)]

//use crate::{generate_utxo_coin_with_random_privkey, GETH_MAKER_SWAP_V2, MYCOIN, MYCOIN1};
use bitcrypto::dhash160;
use coins::utxo::UtxoCommonOps;
use coins::{ConfirmPaymentInput, DexFee, FundingTxSpend, GenTakerFundingSpendArgs, GenTakerPaymentSpendArgs,
            MakerCoinSwapOpsV2, MarketCoinOps, ParseCoinAssocTypes, RefundFundingSecretArgs,
            RefundMakerPaymentSecretArgs, RefundMakerPaymentTimelockArgs, RefundTakerPaymentArgs,
            SendMakerPaymentArgs, SendTakerFundingArgs, SwapTxTypeWithSecretHash, TakerCoinSwapOpsV2, Transaction,
            ValidateMakerPaymentArgs, ValidateTakerFundingArgs};
use coins::RawTransactionRes;
use common::executor::Timer;
use common::{block_on, block_on_f01, log, now_sec, DEX_FEE_ADDR_RAW_PUBKEY};
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::RpcOrderbookEntry;
use mm2_test_helpers::for_tests::{active_swaps, check_recent_swaps, coins_needed_for_kickstart, disable_coin, disable_coin_err, doc_conf, enable_electrum_json, enable_eth_coin, enable_native, get_locked_amount, mm_dump, my_swap_status, mycoin1_conf, mycoin_conf, polygon_conf, start_swaps, wait_for_swap_finished, wait_for_swap_status, MarketMakerIt, Mm2TestConf, SwapV2TestContracts, TestNode, DOC, POLYGON_MAINNET_NODES, POLYGON_MAINNET_SWAP_CONTRACT, POLYGON_MAINNET_SWAP_V2_MAKER_CONTRACT, POLYGON_MAINNET_SWAP_V2_NFT_CONTRACT, POLYGON_MAINNET_SWAP_V2_TAKER_CONTRACT};
use mm2_test_helpers::structs::MmNumberMultiRepr;
use serde_json::{json, Value};
//use crate::tests::eth_tests::enable_eth_rpc;
use script::{Builder, Opcode};
use serialization::serialize;
use web3::transports::Http;
use web3::Web3;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;
use crypto::privkey::key_pair_from_seed;
use crypto::{Bip44Chain, CryptoCtx, CryptoCtxError, GlobalHDAccountArc, KeyPairPolicy};
//use crate::generate_utxo_coin_with_privkey;
use mm2_test_helpers::electrums::{doc_electrums, marty_electrums};
use mm2_test_helpers::for_tests::{enable_eth_coin_v2, orderbook_v2};
use mm2_test_helpers::structs::{GetPublicKeyResult, OrderbookV2Response, RpcV2Response, SetPriceResponse, RpcOrderbookEntryV2, LrBestQuoteResponse, LrFillMakerOrderResponse, ClassicSwapDetails, ClassicSwapResponse};
use coins::eth::EthCoin;
use ethereum_types::{Address, H160, H256, U256};
use lazy_static::lazy_static;
use std::collections::HashSet;

lazy_static! {
    pub static ref POLYGON_WEB3: Web3<Http> = Web3::new(Http::new(POLYGON_MAINNET_NODES[0]).unwrap());
}

#[test]
fn test_aggregated_swap_mainnet_polygon_utxo() {

    let bob_passphrase = std::env::var("BOB_PASSPHRASE").expect("BOB_PASSPHRASE env must be set");
    let alice_passphrase = std::env::var("ALICE_PASSPHRASE").expect("ALICE_PASSPHRASE env must be set");

    let bob_priv_key = key_pair_from_seed(&bob_passphrase).unwrap().private().secret;
    let alice_priv_key = key_pair_from_seed(&alice_passphrase).unwrap().private().secret;

    // WETH = 2696.90 USD
    let weth_conf = json!({
        "coin": "WETH-ERC20",
        "name": "WETH-ERC20",
        "derivation_path": "m/44'/1'",
        "chain_id": 1,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "ETH",
                "contract_address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            }
        }
    });

    // BNB = 612.36 USD
    let bnb_conf = json!({
        "coin": "BNB-ERC20",
        "name": "BNB token",
        "derivation_path": "m/44'/1'",
        "chain_id": 1,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "ETH",
                "contract_address": "0xB8c77482e45F1F44dE1745F52C74426C631bDD52"
            }
        }
    });
    // AAVE 258.75 USD
    let aave_conf = json!({
        "coin": "AAVE-ERC20",
        "name": "AAVE token",
        "derivation_path": "m/44'/1'",
        "chain_id": 1,
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": "ETH",
                "contract_address": "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9"
            }
        }
    });

    let weth_ticker = weth_conf["coin"].as_str().unwrap().to_owned();
    let bnb_ticker = bnb_conf["coin"].as_str().unwrap().to_owned();
    let aave_ticker = aave_conf["coin"].as_str().unwrap().to_owned();

    let bob_coins = json!([doc_conf(), polygon_conf()]);
    let bob_conf = Mm2TestConf::seednode(&bob_passphrase, &bob_coins); // Using legacy swaps until TPU contracts deployed on POLYGON 
    let mut mm_bob = block_on(MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, None)).unwrap();

    let alice_coins = json!([doc_conf(), polygon_conf(), weth_conf.clone(), bnb_conf.clone(), aave_conf.clone()]);
    let alice_conf = Mm2TestConf::light_node(&alice_passphrase, &alice_coins, &[&mm_bob
        .ip
        .to_string()]);
    let mut mm_alice = block_on(MarketMakerIt::start_async(alice_conf.conf, alice_conf.rpc_password, None)).unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_dump(&mm_alice.log_path);
    log!("Alice log path: {}", mm_alice.log_path.display());
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!("Bob log path: {}", mm_bob.log_path.display());

    let contracts_v2 = SwapV2TestContracts {
        maker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_MAKER_CONTRACT.to_owned(),
        taker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_TAKER_CONTRACT.to_owned(),
        nft_maker_swap_v2_contract: POLYGON_MAINNET_SWAP_V2_NFT_CONTRACT.to_owned(),
    };

    let polygon_nodes = POLYGON_MAINNET_NODES.iter().map(|ip| TestNode { url: (*ip).to_owned() }).collect::<Vec<_>>();
    log!("{:?}", block_on(enable_eth_coin_v2(&mm_bob, "MATIC", POLYGON_MAINNET_SWAP_CONTRACT, contracts_v2.clone(), None, &polygon_nodes, &[])));
    log!("{:?}", block_on(enable_electrum_json(&mm_bob, DOC, false, doc_electrums())));

    log!("{:?}", block_on(enable_eth_coin_v2(&mm_alice, "MATIC", POLYGON_MAINNET_SWAP_CONTRACT, contracts_v2, None, &polygon_nodes, 
        &[
            &weth_ticker,
            &bnb_ticker,
            &aave_ticker
        ])));
    log!("{:?}", block_on(enable_electrum_json(&mm_alice, DOC, false, doc_electrums())));

    block_on(create_maker_order(&mut mm_bob, DOC, &weth_ticker, 0.00002));
    block_on(create_maker_order(&mut mm_bob, DOC, &bnb_ticker, 0.00002));
    block_on(create_maker_order(&mut mm_bob, DOC, &aave_ticker, 0.00002));

    let weth_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &weth_ticker, 60)).unwrap();
    let bnb_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &bnb_ticker, 60)).unwrap();
    let aave_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &aave_ticker, 60)).unwrap();

    let best_quote = block_on(find_best_lr_swap(
        &mut mm_alice,
        DOC,
        &[weth_order, bnb_order, aave_order],
        0.00001,
        "MATIC"
    )).unwrap();

    let agg_uuid = block_on(start_agg_swap(
        &mut mm_alice,
        DOC,
        Some(best_quote.lr_swap_details),
        best_quote.best_order,
        None,
        0.0001,
    )).unwrap();
    log!("aggregated swap uuid {:?}", agg_uuid);

    let active_swaps_bob = block_on(active_swaps(&mm_bob));
    assert_eq!(active_swaps_bob.uuids, vec![agg_uuid]);

    let active_swaps_alice = block_on(active_swaps(&mm_alice));
    assert_eq!(active_swaps_alice.uuids, vec![agg_uuid]);

    block_on(wait_for_swap_finished(&mm_bob, &agg_uuid.to_string(), 60));
    block_on(wait_for_swap_finished(&mm_alice, &agg_uuid.to_string(), 30));

    let maker_swap_status = block_on(my_swap_status(&mm_bob, &agg_uuid.to_string()));
    log!("{:?}", maker_swap_status);

    let taker_swap_status = block_on(my_swap_status(&mm_alice, &agg_uuid.to_string()));
    log!("{:?}", taker_swap_status);

    //block_on(check_recent_swaps(&mm_bob, 1));
    //block_on(check_recent_swaps(&mm_alice, 1));

    // Disabling coins on both nodes should be successful at this point
    block_on(disable_coin(&mm_bob, "MATIC", false));
    block_on(disable_coin(&mm_bob, DOC, false));
    block_on(disable_coin(&mm_alice, "MATIC", false));
    block_on(disable_coin(&mm_alice, DOC, false));
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
    if !rc.0.is_success() { return Err(format!("cannot create 1inch swap: {}", rc.1)); }
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
    if !rc.0.is_success() { return Err(format!("cannot sign 1inch swap tx: {}", rc.1)); }
    let sign_resp: RpcV2Response<RawTransactionRes> = serde_json::from_str(&rc.1).unwrap();

    let rc = block_on(mm.rpc(&json!({
        "userpass": mm.userpass,
        "method": "send_raw_transaction",
        "coin": base,
        "tx_hex": sign_resp.result.tx_hex
    })))
    .unwrap();
    if !rc.0.is_success() { return Err(format!("could not send_raw_transaction: {}", rc.1)); }
    let send_resp: Value = serde_json::from_str(&rc.1).unwrap();
    Ok(send_resp["tx_hash"].to_string())
}

async fn wait_for_confirmations(tx_hashes: &[&str], timeout: u32) -> Result<(), String> {
    const RETRY_DELAY: u32 = 5;
    let mut waited = 0;
    let mut unconfirmed = tx_hashes.iter().map(|hash_str| H256::from_str(*hash_str).unwrap()).collect::<Vec<_>>();
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
        if unconfirmed_new.len() == 0 {
            return Ok(())
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
async fn create_maker_order(maker: &mut MarketMakerIt, base: &str, rel: &str, volume: f64) {
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
    assert!(rc.0.is_success(), "!setprice: {}", rc.1);
}

async fn wait_for_orderbook(taker: &mut MarketMakerIt, base: &str, rel: &str, timeout: u32) -> Result<RpcOrderbookEntryV2, String> {
    let mut waited = 0;
    const RETRY_DELAY: u32 = 5;
    loop {
        let orderbook_v2 = block_on(orderbook_v2(taker, base, rel));
        let orderbook_v2: RpcV2Response<OrderbookV2Response> = serde_json::from_value(orderbook_v2).unwrap();
        if orderbook_v2.result.asks.len() > 0 {
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
    amount: f64,
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
    lr_swap_0: Option<ClassicSwapDetails>,
    order_entry: RpcOrderbookEntryV2,
    lr_swap_1: Option<ClassicSwapDetails>,
    volume: f64,
) -> Result<Uuid, String> {
    common::log::info!("Issue taker {}/{} lr_fill_order request", base, order_entry.coin);
    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "lr_fill_order",
            "mmrpc": "2.0",
            "params": {
                "base": base,
                "rel": order_entry.coin,
                "price": order_entry.price,
                "volume": volume,
                "method": "buy",
                "match_by": {
                    "type": "Orders",
                    "data": [order_entry.uuid]
                },
                "lr_swap_0": lr_swap_0,
                "lr_swap_1": lr_swap_1,
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