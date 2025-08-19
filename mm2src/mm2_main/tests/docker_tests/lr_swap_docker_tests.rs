//! Docker tests for 'aggregated taker swaps with liquidity routing' (LR).

use crate::docker_tests::docker_tests_common::{generate_utxo_coin_with_privkey, enable_geth_tokens, GETH_ERC20_CONTRACT, 
    geth_approve_tokens, geth_deploy_lr_swap_contract, deposit_eth_to_lr_swap_contract, deposit_erc20_to_lr_swap_contract};
use crate::docker_tests::eth_docker_tests::{
    fill_eth_with_privkey, erc20_contract_checksum, GETH_DEV_CHAIN_ID, geth_erc20_decimals, geth_erc20_balance
};
use crate::integration_tests_common::enable_native;
use crate::integration_tests_common::lr_tests_common::{
    cancel_order, check_my_agg_swap_final_status, create_and_start_agg_taker_swap, create_maker_order,
    print_balances, print_quote_resp, set_swap_gas_fee_policy, find_best_lr_swap, wait_for_orderbook
};
use crate::GETH_ACCOUNT;
use coins::hd_wallet::AddrToString;
use common::executor::Timer;
use common::{block_on, log};
use crypto::privkey::key_pair_from_seed;
use ethereum_types::U256;
use mm2_number::BigDecimal;
use mm2_test_helpers::for_tests::{active_swaps, disable_coin, mm_dump, my_balance,
                                  my_swap_status, wait_for_swap_finished, MarketMakerIt, Mm2TestConf, eth_dev_conf, erc20_dev_conf, mycoin_conf};
use mm2_test_helpers::for_tests::best_orders_v2_by_number;
use num_traits::FromPrimitive;
use trading_api::one_inch_api::api_mock::ETH_ERC20_MOCK_PRICE;
use serde_json::json;

/// Test for an aggregated taker swap to sell ETH for MYCOIN interim routing ETH via a ERC20 token
#[test]
fn test_aggregated_swap_eth_utxo_sell() {
    let user_base = "ETH".to_owned();
    let user_rel = "MYCOIN".to_owned();
    let swap_amount: BigDecimal = "0.0333".parse().unwrap();
    let rel_amount: BigDecimal = "1".parse().unwrap();
    let method = "sell"; // Sell ETH for MYCOIN
    test_aggregated_swap_eth_utxo_impl(
        &user_base,
        &user_rel,
        swap_amount,
        rel_amount,
        method,
        true,
        false,
    );
}

/// Test for an aggregated taker swap with LR, to buy MYCOIN for ETH with interim routing ETH via a ERC20 token
#[test]
fn test_aggregated_swap_eth_utxo_buy() {
    let user_base = "MYCOIN".to_owned();
    let user_rel = "ETH".to_owned();
    let swap_amount: BigDecimal = "1.111".parse().unwrap();
    let rel_amount = swap_amount.clone();
    let method = "buy"; // Buy MYCOIN for ETH
    test_aggregated_swap_eth_utxo_impl(&user_base, &user_rel, swap_amount, rel_amount, method, true, false);
}

fn test_aggregated_swap_eth_utxo_impl(
    user_base: &str,
    user_rel: &str,
    swap_amount: BigDecimal,
    rel_amount: BigDecimal, // amount to check
    method: &str,
    run_swap: bool,
    use_asks: bool,
) {
    let receive_token = match method {
        "buy" => &user_base,
        "sell" => &user_rel,
        _ => panic!("method must be buy or sell"),
    };

    log!("GETH_ACCOUNT erc20 balance={}", geth_erc20_balance(unsafe { GETH_ACCOUNT }));
    let erc20_decimals = geth_erc20_decimals();
    let price_for_token_smallest_unit = (
        BigDecimal::from_f64(ETH_ERC20_MOCK_PRICE).unwrap() 
        * BigDecimal::from_u64(10_u64.pow(erc20_decimals as u32)).unwrap()
    ).round(0).to_string();
    let wei_to_deposit = U256::from(10) * U256::from(10_u64.pow(18)); // 10 eth
    let tokens_to_deposit = U256::from(500) * U256::from(10_u64.pow(erc20_decimals as u32)); // 500_0000_0000

    let lr_swap_test_contract = geth_deploy_lr_swap_contract(U256::from_dec_str(&price_for_token_smallest_unit).unwrap());
    deposit_eth_to_lr_swap_contract(lr_swap_test_contract, wei_to_deposit);
    geth_approve_tokens(unsafe { GETH_ERC20_CONTRACT }, lr_swap_test_contract, tokens_to_deposit);
    deposit_erc20_to_lr_swap_contract(lr_swap_test_contract, tokens_to_deposit);
    log!("lr_swap_test_contract={} balance={}", lr_swap_test_contract, geth_erc20_balance(lr_swap_test_contract));

    let alice_passphrase = String::from("spice describe gravity federal blast come thank unfair canal monkey style afraid");
    let bob_passphrase = String::from("also shoot benefit prefer juice shell elder veteran woman mimic image kidney");
    let alice_priv_key = key_pair_from_seed(&alice_passphrase).unwrap().private().secret;
    let bob_priv_key = key_pair_from_seed(&bob_passphrase).unwrap().private().secret;

    let utxo_conf = mycoin_conf(1000);
    let utxo_ticker = utxo_conf["coin"].as_str().unwrap().to_owned();
    let eth_conf = eth_dev_conf();
    let eth_ticker = eth_conf["coin"].as_str().unwrap().to_owned();
    let erc20_conf = erc20_dev_conf(&erc20_contract_checksum());
    let erc20_ticker = erc20_conf["coin"].as_str().unwrap().to_owned();

    generate_utxo_coin_with_privkey(&utxo_ticker, 1000.into(), bob_priv_key);
    fill_eth_with_privkey(bob_priv_key, 10.into());
    // fill_erc20_with_privkey(bob_priv_key, 100.into());

    generate_utxo_coin_with_privkey(&utxo_ticker, 1000.into(), alice_priv_key);
    fill_eth_with_privkey(alice_priv_key, 10.into());

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
        ]
    )).unwrap();

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
        ]
    ))
    .unwrap();

    let (_alice_dump_log, _alice_dump_dashboard) = mm_dump(&mm_alice.log_path);
    log!("Alice log path: {}", mm_alice.log_path.display());
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    log!("Bob log path: {}", mm_bob.log_path.display());

    let _bob_enable_tokens = block_on(enable_geth_tokens(&mm_bob, &[&erc20_ticker]));
    let _bob_enable_doc = block_on(enable_native(&mm_bob, &utxo_ticker, &[], None));
    print_balances(&mm_bob, "Bob balance before swap:", &[&utxo_ticker, &eth_ticker, &erc20_ticker]);

    let _alice_enable_tokens = block_on(enable_geth_tokens(&mm_alice, &[&erc20_ticker]));
    let _alice_enable_doc = block_on(enable_native(&mm_alice, &utxo_ticker, &[], None));
    print_balances(&mm_alice, "Alice balance before swap:", &[&utxo_ticker, &eth_ticker, &erc20_ticker]);

    let alice_balance_before = block_on(my_balance(&mm_alice, receive_token));

    if let Err(err) = block_on(set_swap_gas_fee_policy(&mm_alice, &eth_ticker, "Medium")) {
        log!("set_swap_transaction_fee_policy on {eth_ticker} error={}", err);
    }

    let buy_token_order = block_on(create_maker_order(&mut mm_bob, &utxo_ticker, &erc20_ticker, 1.25, 200.0)).unwrap();
    let buy_token_order = block_on(wait_for_orderbook(&mut mm_alice, &utxo_ticker, &erc20_ticker, &buy_token_order.uuid, 60)).unwrap();

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

    const LR_SLIPPAGE: f32 = 0.0;
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
    if !run_swap {
        return;
    }

    let agg_uuid = block_on(create_and_start_agg_taker_swap(&mut mm_alice, LR_SLIPPAGE, best_quote)).unwrap();
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

    let alice_balance_after = block_on(my_balance(&mm_alice, receive_token));
    print_balances(&mm_bob, "Bob balance after swap:", &[&utxo_ticker, &eth_ticker, &erc20_ticker]);
    print_balances(&mm_alice, "Bob balance after swap:", &[&utxo_ticker, &eth_ticker, &erc20_ticker]);

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

    // Rough check taker swap amount received
    let alice_bal_diff = &alice_balance_after.balance - &alice_balance_before.balance;
    log!("Alice received amount {}: {}", receive_token, alice_bal_diff);
    assert!(
        alice_bal_diff > &rel_amount * "0.80".parse::<BigDecimal>().unwrap(),
        "too little received {}",
        alice_bal_diff
    );
    assert!(
        alice_bal_diff < &rel_amount * "1.20".parse::<BigDecimal>().unwrap(),
        "too much received {}",
        alice_bal_diff
    );
}
