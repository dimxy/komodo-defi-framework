//! Docker tests for 'aggregated taker swaps with liquidity routing' (LR).

use crate::docker_tests::docker_tests_common::{deposit_erc20_to_lr_swap_contract, deposit_eth_to_lr_swap_contract,
                                               enable_geth_tokens, generate_utxo_coin_with_privkey,
                                               geth_approve_tokens, geth_deploy_lr_swap_contract, GETH_ERC20_CONTRACT};
use crate::docker_tests::eth_docker_tests::{erc20_contract_checksum, fill_erc20_with_privkey, fill_eth_with_privkey,
                                            geth_erc20_balance, geth_erc20_decimals, GETH_DEV_CHAIN_ID};
use crate::integration_tests_common::enable_native;
use crate::integration_tests_common::lr_tests_common::{cancel_order, check_my_agg_swap_final_status,
                                                       create_and_start_agg_taker_swap, create_maker_order,
                                                       find_best_lr_swap, print_balances, print_quote_resp,
                                                       set_swap_gas_fee_policy, wait_for_orderbook};
use crate::GETH_ACCOUNT;
use coins::hd_wallet::AddrToString;
use common::executor::Timer;
use common::{block_on, log};
use crypto::privkey::key_pair_from_seed;
use ethereum_types::U256;
use mm2_number::BigDecimal;
use mm2_test_helpers::for_tests::best_orders_v2_by_number;
use mm2_test_helpers::for_tests::{active_swaps, disable_coin, erc20_dev_conf, eth_dev_conf, mm_dump, my_balance,
                                  my_swap_status, mycoin_conf, wait_for_swap_finished, MarketMakerIt, Mm2TestConf};
use num_traits::FromPrimitive;
use serde_json::json;
use trading_api::one_inch_api::api_mock::ETH_ERC20_MOCK_PRICE;

const TEST_UTXO_AMOUNT: u32 = 1000;
const TEST_ETH_AMOUNT: u32 = 10;
const TEST_ERC20_AMOUNT: u32 = 500;

/// Test for an aggregated taker swap to sell ETH for MYCOIN interim routing ETH via a ERC20 token
#[test]
fn test_aggregated_swap_eth_utxo_sell() {
    let eth_base = eth_dev_conf()["coin"].as_str().unwrap().to_owned();
    let utxo_rel = mycoin_conf(1000)["coin"].as_str().unwrap().to_owned();
    let erc20_ticker = erc20_dev_conf(&erc20_contract_checksum())["coin"]
        .as_str()
        .unwrap()
        .to_owned();
    let utxo_to_erc20_price = 1.25;
    let swap_amount: BigDecimal = "0.0333".parse().unwrap(); // in ETH
    let maker_order_params = (utxo_rel.as_str(), erc20_ticker.as_str(), utxo_to_erc20_price, 200.0);
    let rel_amount = &swap_amount
        * &BigDecimal::from_u32(ETH_ERC20_MOCK_PRICE).unwrap() // ERC20 value
        / &BigDecimal::from_f64(utxo_to_erc20_price).unwrap(); // MYCOIN value
    let method = "sell"; // Sell ETH for MYCOIN
    test_aggregated_swap_eth_utxo_impl(
        &eth_base,
        &utxo_rel,
        swap_amount,
        rel_amount,
        method,
        10.0,
        maker_order_params,
        false,
    );
}

/// Test for an aggregated taker swap with LR, to buy MYCOIN for ETH with interim routing ETH via a ERC20 token
#[test]
fn test_aggregated_swap_eth_utxo_buy() {
    let utxo_base = mycoin_conf(1000)["coin"].as_str().unwrap().to_owned();
    let eth_rel = eth_dev_conf()["coin"].as_str().unwrap().to_owned();
    let erc20_ticker = erc20_dev_conf(&erc20_contract_checksum())["coin"]
        .as_str()
        .unwrap()
        .to_owned();
    let utxo_to_erc20_price = 1.25;
    let swap_amount: BigDecimal = "100.111".parse().unwrap(); // in MYCOIN
    let maker_order_params = (utxo_base.as_str(), erc20_ticker.as_str(), utxo_to_erc20_price, 200.0);
    let rel_amount = &swap_amount
        * &BigDecimal::from_f64(utxo_to_erc20_price).unwrap() // ERC20 value
        / &BigDecimal::from_u32(ETH_ERC20_MOCK_PRICE).unwrap(); // ETH value
    let method = "buy"; // Buy MYCOIN for ETH
    test_aggregated_swap_eth_utxo_impl(
        &utxo_base,
        &eth_rel,
        swap_amount,
        rel_amount,
        method,
        3.1,
        maker_order_params,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn test_aggregated_swap_eth_utxo_impl(
    user_base: &str,
    user_rel: &str,
    swap_amount: BigDecimal,
    rel_amount: BigDecimal, // amount to check
    method: &str,
    slippage: f32,
    (base, rel, price, max_vol): (&str, &str, f64, f64), // atomic swap maker order params
    use_asks: bool,
) {
    log!(
        "Starting taker swap with LR to {} {} of base={} for rel={}, estimated rel amount {}, slippage {} pct",
        method,
        swap_amount,
        user_base,
        user_rel,
        rel_amount,
        slippage
    );
    log!(
        "GETH_ACCOUNT erc20 balance={}",
        geth_erc20_balance(unsafe { GETH_ACCOUNT })
    );
    let erc20_decimals = geth_erc20_decimals();
    let price_for_token_smallest_unit = (BigDecimal::from_u32(ETH_ERC20_MOCK_PRICE).unwrap()
        * BigDecimal::from_u64(10_u64.pow(erc20_decimals as u32)).unwrap())
    .round(0)
    .to_string();
    let wei_to_deposit = U256::from(TEST_ETH_AMOUNT) * U256::from(10_u64.pow(18)); // 10 eth
    let tokens_to_deposit = U256::from(TEST_ERC20_AMOUNT) * U256::from(10_u64.pow(erc20_decimals as u32)); // 500_0000_0000

    // Deploy and top up LR provider emulator
    let lr_swap_test_contract =
        geth_deploy_lr_swap_contract(U256::from_dec_str(&price_for_token_smallest_unit).unwrap());
    deposit_eth_to_lr_swap_contract(lr_swap_test_contract, wei_to_deposit);
    geth_approve_tokens(unsafe { GETH_ERC20_CONTRACT }, lr_swap_test_contract, tokens_to_deposit);
    deposit_erc20_to_lr_swap_contract(lr_swap_test_contract, tokens_to_deposit);
    log!(
        "lr_swap_test_contract={} erc20 balance={}",
        lr_swap_test_contract,
        geth_erc20_balance(lr_swap_test_contract)
    );

    let alice_passphrase =
        String::from("spice describe gravity federal blast come thank unfair canal monkey style afraid");
    let bob_passphrase = String::from("also shoot benefit prefer juice shell elder veteran woman mimic image kidney");
    let alice_priv_key = key_pair_from_seed(&alice_passphrase).unwrap().private().secret;
    let bob_priv_key = key_pair_from_seed(&bob_passphrase).unwrap().private().secret;

    let utxo_conf = mycoin_conf(1000);
    let utxo_ticker = utxo_conf["coin"].as_str().unwrap().to_owned();
    let eth_conf = eth_dev_conf();
    let eth_ticker = eth_conf["coin"].as_str().unwrap().to_owned();
    let erc20_conf = erc20_dev_conf(&erc20_contract_checksum());
    let erc20_ticker = erc20_conf["coin"].as_str().unwrap().to_owned();

    generate_utxo_coin_with_privkey(&utxo_ticker, TEST_UTXO_AMOUNT.into(), bob_priv_key);
    fill_eth_with_privkey(bob_priv_key, TEST_ETH_AMOUNT.into());

    generate_utxo_coin_with_privkey(&utxo_ticker, TEST_UTXO_AMOUNT.into(), alice_priv_key);
    fill_eth_with_privkey(alice_priv_key, TEST_ETH_AMOUNT.into());
    if user_base == erc20_ticker || user_rel == erc20_ticker {
        fill_erc20_with_privkey(alice_priv_key, TEST_ERC20_AMOUNT.into());
    }

    let bob_coins = json!([utxo_conf, eth_conf, erc20_conf]);
    let bob_conf = Mm2TestConf::seednode_trade_v2(&bob_passphrase, &bob_coins);
    let mut mm_bob = block_on(MarketMakerIt::start_with_envs(
        bob_conf.conf,
        bob_conf.rpc_password,
        None,
        &[
            ("GETH_ERC20_CONTRACT", &unsafe { GETH_ERC20_CONTRACT }.addr_to_string()),
            ("GETH_ERC20_DECIMALS", &erc20_decimals.to_string()),
            ("GETH_CHAIN_ID", &GETH_DEV_CHAIN_ID.to_string()),
            ("GETH_TEST_LR_SWAP_CONTRACT", &lr_swap_test_contract.addr_to_string()),
        ],
    ))
    .unwrap();

    let alice_coins = json!([utxo_conf, eth_conf, erc20_conf]);
    let mut alice_conf = Mm2TestConf::light_node_trade_v2(&alice_passphrase, &alice_coins, &[&mm_bob.ip.to_string()]);
    alice_conf.conf["1inch_api"] = "https://api.1inch.dev".into();
    let mut mm_alice = block_on(MarketMakerIt::start_with_envs(
        alice_conf.conf,
        alice_conf.rpc_password,
        None,
        &[
            ("GETH_ERC20_CONTRACT", &unsafe { GETH_ERC20_CONTRACT }.addr_to_string()),
            ("GETH_ERC20_DECIMALS", &erc20_decimals.to_string()),
            ("GETH_CHAIN_ID", &GETH_DEV_CHAIN_ID.to_string()),
            ("GETH_TEST_LR_SWAP_CONTRACT", &lr_swap_test_contract.addr_to_string()),
        ],
    ))
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_dump(&mm_alice.log_path);
    log!("Alice log path: {}", mm_alice.log_path.display());
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!("Bob log path: {}", mm_bob.log_path.display());

    let _bob_enable_tokens = block_on(enable_geth_tokens(&mm_bob, &[&erc20_ticker]));
    let _bob_enable_doc = block_on(enable_native(&mm_bob, &utxo_ticker, &[], None));
    print_balances(&mm_bob, "Bob balance before swap:", &[
        &utxo_ticker,
        &eth_ticker,
        &erc20_ticker,
    ]);

    let _alice_enable_tokens = block_on(enable_geth_tokens(&mm_alice, &[&erc20_ticker]));
    let _alice_enable_doc = block_on(enable_native(&mm_alice, &utxo_ticker, &[], None));
    print_balances(&mm_alice, "Alice balance before swap:", &[
        &utxo_ticker,
        &eth_ticker,
        &erc20_ticker,
    ]);

    let alice_rel_balance_before = block_on(my_balance(&mm_alice, user_rel));

    if let Err(err) = block_on(set_swap_gas_fee_policy(&mm_alice, &eth_ticker, "Medium")) {
        log!("set_swap_transaction_fee_policy on {eth_ticker} error={}", err);
    }

    let buy_token_order = block_on(create_maker_order(&mut mm_bob, base, rel, price, max_vol)).unwrap();
    let buy_token_order = block_on(wait_for_orderbook(&mut mm_alice, base, rel, &buy_token_order.uuid, 60)).unwrap();

    let mut json_asks = vec![];
    let mut json_bids = vec![];
    if use_asks {
        // Taker action is 'buy' so returned orders are 'asks'
        let best_orders_res = block_on(best_orders_v2_by_number(&mm_alice, &utxo_ticker, "buy", 10, true));
        // Flatten orders to remove the aggregation level
        let best_asks = best_orders_res
            .result
            .orders
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(); // As taker used 'buy' actions, orders are returned as asks
        json_asks.push(json!({ "base": &utxo_ticker, "orders": &best_asks }));
    } else {
        // Taker action is 'sell' so returned orders are 'bids'
        let best_orders_res = block_on(best_orders_v2_by_number(&mm_alice, &erc20_ticker, "sell", 10, true));
        // Flatten orders to remove the aggregation level
        let best_bids = best_orders_res
            .result
            .orders
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        json_bids.push(json!({ "rel": &erc20_ticker, "orders": &best_bids }));
    }

    let best_quote = block_on(find_best_lr_swap(
        &mut mm_alice,
        user_base,
        &json!(json_asks),
        &json!(json_bids),
        &swap_amount,
        method,
        user_rel,
    ))
    .expect("best quote should be found");
    print_quote_resp(&best_quote);

    let agg_uuid = block_on(create_and_start_agg_taker_swap(&mut mm_alice, slippage, best_quote)).unwrap();
    log!("Aggregated taker swap uuid {:?} started", agg_uuid);
    block_on(Timer::sleep(1.0));

    let active_swaps_alice = block_on(active_swaps(&mm_alice));
    assert_eq!(active_swaps_alice.uuids, vec![agg_uuid]);
    block_on(wait_for_swap_finished(&mm_alice, &agg_uuid.to_string(), 600)); // Only taker has the aggregated swap
    log!("Aggregated taker swap uuid {:?} finished", agg_uuid);

    let taker_swap_status = block_on(my_swap_status(&mm_alice, &agg_uuid.to_string())).unwrap();
    log!(
        "Aggregated taker swap final status {}",
        serde_json::to_string(&taker_swap_status).unwrap()
    );
    check_my_agg_swap_final_status(&taker_swap_status);

    let alice_rel_balance_after = block_on(my_balance(&mm_alice, user_rel));
    print_balances(&mm_bob, "Bob balance after swap:", &[
        &utxo_ticker,
        &eth_ticker,
        &erc20_ticker,
    ]);
    print_balances(&mm_alice, "Alice balance after swap:", &[
        &utxo_ticker,
        &eth_ticker,
        &erc20_ticker,
    ]);

    let status = block_on(my_swap_status(&mm_alice, &agg_uuid.to_string())).unwrap();
    if let Some(atomic_swap_uuid) = status["result"]["atomic_swap_uuid"].as_str() {
        log!("Waiting for Maker to finish atomic_swap_uuid={atomic_swap_uuid}");
        block_on(wait_for_swap_finished(&mm_bob, atomic_swap_uuid, 60));
    }

    block_on(cancel_order(&mm_bob, &buy_token_order.uuid));
    block_on(Timer::sleep(10.0)); // wait for orderbook update

    block_on(disable_coin(&mm_bob, &eth_ticker, false));
    block_on(disable_coin(&mm_bob, &utxo_ticker, false));
    block_on(disable_coin(&mm_alice, &eth_ticker, false));
    block_on(disable_coin(&mm_alice, &utxo_ticker, false));

    let alice_bal_diff = if method == "sell" {
        &alice_rel_balance_after.balance - &alice_rel_balance_before.balance
    } else {
        &alice_rel_balance_before.balance - &alice_rel_balance_after.balance
    };
    log!(
        "Alice rel amount {}: {}, expected: {} slippage: {} pct",
        user_rel,
        alice_bal_diff,
        rel_amount,
        slippage
    );
    assert_rel_amount(&alice_bal_diff, &rel_amount, 10.0, slippage);
}

/// Check taker received or spend rel amount.
/// The 'tolerance' and 'slippage' are percentages.
/// The 'tolerance' is how much calculation errors and fee losses we can tolerate
fn assert_rel_amount(rel: &BigDecimal, expected: &BigDecimal, tolerance: f32, slippage: f32) {
    let min_expected = expected * (BigDecimal::from_u32(1).unwrap() - BigDecimal::from_f32(slippage / 100.0).unwrap());
    let min_with_tolerance =
        min_expected * (BigDecimal::from_u32(1).unwrap() - BigDecimal::from_f32(tolerance / 100.0).unwrap());
    if rel < &min_with_tolerance {
        panic!("too little rel amount: {}, expected: {}", rel, expected);
    }

    let max_expected = expected * (BigDecimal::from_u32(1).unwrap() + BigDecimal::from_f32(slippage / 100.0).unwrap());
    let max_with_tolerance =
        max_expected * (BigDecimal::from_u32(1).unwrap() + BigDecimal::from_f32(tolerance / 100.0).unwrap());
    if rel > &max_with_tolerance {
        panic!("too much rel amount: {}, expected: {}", rel, expected);
    }
}
