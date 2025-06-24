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
use mm2_number::BigDecimal;
use mm2_number::{bigdecimal::ToPrimitive, MmNumber};
use mm2_rpc::data::legacy::{CoinInitResponse, RpcOrderbookEntry};
use mm2_test_helpers::for_tests::{active_swaps, check_recent_swaps, coins_needed_for_kickstart, disable_coin,
                                  disable_coin_err, doc_conf, enable_electrum_json, enable_eth_coin, enable_native,
                                  get_locked_amount, mm_dump, my_balance, my_swap_status, mycoin1_conf, mycoin_conf,
                                  polygon_conf, start_swaps, wait_for_swap_finished, MarketMakerIt, Mm2TestConf,
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
use mm2_test_helpers::structs::lr_test_structs::{AskOrBidOrder, AsksForCoin, BidsForCoin, ClassicSwapDetails,
                                                 ClassicSwapResponse, LrExecuteRoutedTradeResponse,
                                                 LrFindBestQuoteResponse};
use mm2_test_helpers::structs::{GetPublicKeyResult, OrderbookV2Response, RpcOrderbookEntryV2, RpcV2Response,
                                SetPriceResponse};
use std::collections::HashSet;

lazy_static! {
    pub static ref POLYGON_WEB3: Web3<Http> = Web3::new(Http::new(POLYGON_MAINNET_NODES[0]).unwrap());
}

/// This is a non-CI test that runs an 'aggregated taker swap with liquidity routing' (LR).
/// Alice needs some coins on MATIC (POL) mainnet and Bob needs test DOC coins (Bob also needs some MATIC to pay tx fee in atomic swap).
/// It swaps some MATIC coins to buy DOC coins with interim liquidity routing (via the 1inch LR provider) of user MATIC coins over one of some POLYGON tokens.
/// For this the test first calls the LR find best quote rpc selecting the best priced swap path from 1inch quotes for MATIC to POL-tokens and POL-tokens to DOC KDF orders,
/// then calls the LR fill order rpc to run an aggregated swap that does liquidity routing and then atomic swap.
#[test]
fn test_aggregated_swap_mainnet_polygon_utxo() {
    const MATIC: &str = "MATIC";
    let bob_passphrase = std::env::var("BOB_DIMXY").expect("BOB_DIMXY env must be set");
    let alice_passphrase = std::env::var("ALICE_DIMXY").expect("ALICE_DIMXY env must be set");

    let bob_priv_key = key_pair_from_seed(&bob_passphrase).unwrap().private().secret;
    let alice_priv_key = key_pair_from_seed(&alice_passphrase).unwrap().private().secret;

    // 1 USD
    let token_1_conf = json!({
        "coin": "DAI-PLG20",
        "name": "dai_plg20",
        "derivation_path": "m/44'/966'",
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": MATIC,
                "contract_address": "0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063"
            }
        }
    });
    let token_2_conf = json!({
        "coin": "1INCH-PLG20", // Note: crossprices API always returns 'token not in whitelist, unsupported token'
        "name": "1inch_plg20",
        "derivation_path": "m/44'/966'",
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": MATIC,
                "contract_address": "0x9c2C5fd7b07E95EE044DDeba0E97a665F142394f"
            }
        }
    });
    let token_3_conf = json!({
        "coin": "AGIX-PLG20", // Note: crossprices API returns 'token not in whitelist, unsupported token' frequently
        "name": "agix_plg20",
        "derivation_path": "m/44'/966'",
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": MATIC,
                "contract_address": "0x190Eb8a183D22a4bdf278c6791b152228857c033"
            }
        }
    });
    // 289 USD
    let token_4_conf = json!({
        "coin": "AAVE-PLG20",
        "name": "aave_plg20",
        "derivation_path": "m/44'/966'",
        "decimals": 18,
        "protocol": {
            "type": "ERC20",
            "protocol_data": {
                "platform": MATIC,
                "contract_address": "0xD6DF932A45C0f255f85145f286eA0b292B21C90B"
            }
        }
    });

    let token_1_ticker = token_1_conf["coin"].as_str().unwrap().to_owned();
    let token_2_ticker = token_2_conf["coin"].as_str().unwrap().to_owned();
    let token_3_ticker = token_3_conf["coin"].as_str().unwrap().to_owned();
    let token_4_ticker = token_4_conf["coin"].as_str().unwrap().to_owned();

    let bob_coins = json!([
        doc_conf(),
        polygon_conf(),
        token_1_conf,
        token_2_conf,
        token_3_conf,
        token_4_conf
    ]);
    let bob_conf = Mm2TestConf::seednode(&bob_passphrase, &bob_coins); // Using legacy swaps until TPU contracts deployed on POLYGON
    let mut mm_bob = block_on(MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, None)).unwrap();

    let alice_coins = json!([
        doc_conf(),
        polygon_conf(),
        token_1_conf,
        token_2_conf,
        token_3_conf,
        token_4_conf
    ]);
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
        MATIC,
        POLYGON_MAINNET_SWAP_CONTRACT,
        contracts_v2.clone(),
        None,
        &polygon_nodes,
        json!([
            { "ticker": &token_1_ticker },
            { "ticker": &token_2_ticker },
            { "ticker": &token_3_ticker },
            { "ticker": &token_4_ticker }
        ]),
    ));
    let bob_eth_info = bob_enable_tokens["result"]["eth_addresses_infos"].as_object().unwrap();

    let bob_enable_doc = block_on(enable_electrum_json(&mm_bob, DOC, false, doc_electrums()));
    log!(
        "Bob DOC address={:?} balance={:?} unspendable_balance={:?}",
        bob_enable_doc["address"].as_str().unwrap(),
        bob_enable_doc["balance"],
        bob_enable_doc["unspendable_balance"]
    );
    log!(
        "Bob MATIC address={:?} balance={:?}",
        bob_eth_info.iter().next().unwrap().0,
        bob_eth_info.iter().next().unwrap().1["balances"].as_object().unwrap()
    );

    let alice_enable_tokens = block_on(enable_eth_coin_v2(
        &mm_alice,
        MATIC,
        POLYGON_MAINNET_SWAP_CONTRACT,
        contracts_v2,
        None,
        &polygon_nodes,
        json!([
            { "ticker": &token_1_ticker },
            { "ticker": &token_2_ticker },
            { "ticker": &token_3_ticker },
            { "ticker": &token_4_ticker }
        ]),
    ));
    let alice_enable_doc = block_on(enable_electrum_json(&mm_alice, DOC, false, doc_electrums()));
    let alice_enable_doc = serde_json::from_value::<CoinInitResponse>(alice_enable_doc).unwrap();
    log!(
        "Alice DOC address={:?} balance={:?} unspendable_balance={:?}",
        alice_enable_doc.address,
        alice_enable_doc.balance,
        alice_enable_doc.unspendable_balance
    );

    let alice_eth_info = alice_enable_tokens["result"]["eth_addresses_infos"]
        .as_object()
        .unwrap();
    let alice_erc20_info = alice_enable_tokens["result"]["erc20_addresses_infos"]
        .as_object()
        .unwrap();
    log!(
        "Alice {MATIC} address={} balance={:?}",
        alice_eth_info.iter().next().unwrap().0,
        alice_eth_info.iter().next().unwrap().1["balances"].as_object().unwrap()
    );
    log!(
        "Alice token balances: {:?}={:?}, {:?}={:?}, {:?}={:?}, {:?}={:?}",
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
        token_4_ticker,
        alice_erc20_info.iter().next().unwrap().1["balances"][token_4_ticker.clone()]
            .as_object()
            .unwrap()
    );

    let order_res_1 = block_on(create_maker_order(&mut mm_bob, DOC, &token_1_ticker, 0.0011111, 1.0)); // DAI
    let order_res_2 = block_on(create_maker_order(&mut mm_bob, DOC, &token_2_ticker, 0.00105, 1.0)); // 1INCH
    let order_res_3 = block_on(create_maker_order(
        &mut mm_bob,
        DOC,
        &token_3_ticker,
        0.000980009090,
        1.0,
    )); // AGIX
    let order_res_4 = block_on(create_maker_order(
        &mut mm_bob,
        DOC,
        &token_4_ticker,
        0.0000037313793103,
        1.0,
    )); // AAVE
    check_results(
        &[order_res_1, order_res_2, order_res_3, order_res_4],
        "maker orders created with errors",
    );

    let token_1_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_1_ticker, 60)).unwrap();
    let token_2_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_2_ticker, 60)).unwrap();
    let token_3_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_3_ticker, 60)).unwrap();
    let token_4_order = block_on(wait_for_orderbook(&mut mm_alice, DOC, &token_4_ticker, 60)).unwrap();

    let best_asks = block_on(best_orders_v2_by_number(&mm_alice, DOC, "buy", 10, false));
    let entries = best_asks.result.orders.values().flatten().cloned().collect::<Vec<_>>();

    let doc_amount_to_buy: BigDecimal = "1.0".parse().unwrap();
    const LR0_SLIPPAGE: f32 = 0.0;
    let best_quote = block_on(find_best_lr_swap(
        &mut mm_alice,
        DOC,
        &[AsksForCoin {
            base: DOC.to_owned(),
            orders: entries,
        }],
        &[],
        &doc_amount_to_buy,
        MATIC,
    ))
    .expect("best quote should be found");
    log!(
        "Found best quote for swap with LR0: lr_swap_details: src_token={} src_amount={} dst_token={} dst_amount={}",
        best_quote
            .lr_swap_details
            .src_token
            .as_ref()
            .unwrap()
            .symbol_kdf
            .as_ref()
            .unwrap(),
        best_quote.lr_swap_details.src_amount.to_decimal(),
        best_quote
            .lr_swap_details
            .dst_token
            .as_ref()
            .unwrap()
            .symbol_kdf
            .as_ref()
            .unwrap(),
        best_quote.lr_swap_details.dst_amount.as_ratio().to_f64().unwrap(),
    );

    let agg_uuid = block_on(create_and_start_agg_taker_swap(
        &mut mm_alice,
        DOC,
        None,
        LR0_SLIPPAGE,
        Some(best_quote.lr_swap_details),
        best_quote.best_order,
        None,
    ))
    .unwrap();
    log!("Aggregated swap uuid {:?} started", agg_uuid);

    block_on(Timer::sleep(1.0));

    let active_swaps_alice = block_on(active_swaps(&mm_alice));
    assert_eq!(active_swaps_alice.uuids, vec![agg_uuid]);

    block_on(wait_for_swap_finished(&mm_alice, &agg_uuid.to_string(), 180)); // Only taker has the aggregated swap
    log!("Aggregated lr swap {:?} finished", agg_uuid);

    let taker_swap_status = block_on(my_swap_status(&mm_alice, &agg_uuid.to_string())).unwrap();
    log!(
        "final taker_swap_status {}",
        serde_json::to_string(&taker_swap_status).unwrap()
    );
    check_my_agg_swap_final_status(&taker_swap_status);
    let alice_balance = block_on(my_balance(&mm_alice, DOC));
    log!(
        "Alice DOC address={:?} balance={:?} unspendable_balance={:?} after swap",
        alice_balance.address,
        alice_balance.balance,
        alice_balance.unspendable_balance
    );

    // Rough check taker swap amount received
    assert!(
        &alice_balance.balance - &alice_enable_doc.balance > &doc_amount_to_buy - &"0.1".parse::<BigDecimal>().unwrap()
    );
    assert!(
        &alice_balance.balance - &alice_enable_doc.balance < &doc_amount_to_buy + &"0.1".parse::<BigDecimal>().unwrap()
    );

    //block_on(check_recent_swaps(&mm_bob, 1));
    //block_on(check_recent_swaps(&mm_alice, 1));
    block_on(cancel_order(&mm_bob, &token_1_order.uuid));
    block_on(cancel_order(&mm_bob, &token_2_order.uuid));
    block_on(cancel_order(&mm_bob, &token_3_order.uuid));
    block_on(cancel_order(&mm_bob, &token_4_order.uuid));

    block_on(Timer::sleep(10.0)); // wait for orderbook update

    // Disabling coins on both nodes should be successful at this point
    block_on(disable_coin(&mm_bob, MATIC, false));
    block_on(disable_coin(&mm_bob, DOC, false));
    block_on(disable_coin(&mm_alice, MATIC, false));
    block_on(disable_coin(&mm_alice, DOC, false));
}

/// Not used yet
/// We may use this to top-up a token before an agg swap test,
/// for e.g. if we want to do liquidity routing from this token
#[allow(unused)]
async fn top_up_token(mm: &mut MarketMakerIt, base: &str, rel: &str, amount: f64) -> Result<(), String> {
    make_1inch_swap(mm, base, rel, amount).await?;
    Ok(())
}

/// Not used yet
/// Make 1inch classic swap to top-up tokens for the test
/// Use default tx fee set by the 1inch rpc (should be medium)
#[allow(unused)]
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
    Err(format!("no ask {base}/{rel} in orderbook"))
}

async fn find_best_lr_swap(
    taker: &mut MarketMakerIt,
    my_base_coin: &str,
    asks: &[AsksForCoin],
    bids: &[BidsForCoin],
    volume_to_buy: &BigDecimal,
    my_rel_coin: &str,
) -> Result<LrFindBestQuoteResponse, String> {
    common::log::info!("Issue lr::best_quote {}/{} request", my_base_coin, my_rel_coin);
    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "experimental::liquidity_routing::find_best_quote",
            "mmrpc": "2.0",
            "params": {
                "user_base": my_base_coin,
                "volume": volume_to_buy,
                "asks": asks,
                "bids": bids,
                "method": "buy",
                "user_rel": my_rel_coin
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

async fn create_and_start_agg_taker_swap(
    taker: &mut MarketMakerIt,
    base: &str,
    atomic_swap_volume: Option<&BigDecimal>,
    slippage: f32,
    lr_swap_details_0: Option<ClassicSwapDetails>,
    order_entry: AskOrBidOrder,
    lr_swap_details_1: Option<ClassicSwapDetails>,
) -> Result<Uuid, String> {
    common::log::info!(
        "Issue taker {}/{} lr::fill_order request",
        base,
        order_entry.order().coin
    );

    assert!(
        lr_swap_details_0.is_some() && atomic_swap_volume.is_none()
            || lr_swap_details_0.is_none() && atomic_swap_volume.is_some()
    );
    let lr_swap_0 = lr_swap_details_0.map(|swap_details| {
        json!({
            "slippage": slippage,
            "swap_details": swap_details
        })
    });
    let lr_swap_1 = lr_swap_details_1.map(|swap_details| {
        json!({
            "slippage": slippage,
            "swap_details": swap_details
        })
    });

    let rc = taker
        .rpc(&json!({
            "userpass": taker.userpass,
            "method": "experimental::liquidity_routing::execute_routed_trade",
            "mmrpc": "2.0",
            "params": {
                "atomic_swap": {
                    "volume": atomic_swap_volume,
                    "base": base,
                    "rel": order_entry.order().coin,
                    "price": order_entry.order().price.rational,
                    "method": "buy",
                    "match_by": {
                        "type": "Orders",
                        "data": [order_entry.order().uuid]
                    }
                },
                "lr_swap_0": lr_swap_0,
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

fn check_results<T: std::fmt::Debug>(results: &[Result<T, String>], msg: &str) {
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
