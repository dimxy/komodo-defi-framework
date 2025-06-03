use crate::{lp_init, lp_run};
use common::executor::{spawn, spawn_abortable, spawn_local_abortable, AbortOnDropHandle, Timer};
use common::log::warn;
use common::log::wasm_log::register_wasm_log;
use mm2_core::mm_ctx::MmArc;
use mm2_number::BigDecimal;
use mm2_rpc::data::legacy::OrderbookResponse;
use mm2_test_helpers::electrums::{doc_electrums, marty_electrums};
use mm2_test_helpers::for_tests::{check_recent_swaps, enable_electrum_json, enable_utxo_v2_electrum,
                                  enable_z_coin_light, get_wallet_names, morty_conf, pirate_conf, rick_conf,
                                  start_swaps, test_qrc20_history_impl, wait_for_swaps_finish_and_check_status,
                                  MarketMakerIt, Mm2InitPrivKeyPolicy, Mm2TestConf, Mm2TestConfForSwap, ARRR, MORTY,
                                  PIRATE_ELECTRUMS, PIRATE_LIGHTWALLETD_URLS, RICK};
use mm2_test_helpers::get_passphrase;
use mm2_test_helpers::structs::{Bip44Chain, EnableCoinBalance, HDAccountAddressId};
use serde_json::json;
use wasm_bindgen_test::wasm_bindgen_test;

const PIRATE_TEST_BALANCE_SEED: &str = "pirate test seed";
const STOP_TIMEOUT_MS: u64 = 1000;

/// Starts the WASM version of MM.
fn wasm_start(ctx: MmArc) {
    spawn(async move {
        lp_init(ctx.clone(), "TEST".into(), "TEST".into()).await.unwrap();
        lp_run(ctx).await;
    })
}

/// This function runs Alice and Bob nodes, activates coins, starts swaps,
/// and then immediately stops the nodes to check if `MmArc` is dropped in a short period.
async fn test_mm2_stops_impl(pairs: &[(&'static str, &'static str)], maker_price: f64, taker_price: f64, volume: f64) {
    let coins = json!([rick_conf(), morty_conf()]);

    let bob_passphrase = get_passphrase!(".env.seed", "BOB_PASSPHRASE").unwrap();
    let alice_passphrase = get_passphrase!(".env.client", "ALICE_PASSPHRASE").unwrap();

    let bob_conf = Mm2TestConf::seednode(&bob_passphrase, &coins);
    let mut mm_bob = MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();
    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    Timer::sleep(2.).await;

    let alice_conf = Mm2TestConf::light_node(&alice_passphrase, &coins, &[&mm_bob.my_seed_addr()]);
    let mut mm_alice = MarketMakerIt::start_async(alice_conf.conf, alice_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();
    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();
    Timer::sleep(2.).await;

    // Enable coins on Bob side. Print the replies in case we need the address.
    let rc = enable_electrum_json(&mm_bob, RICK, true, doc_electrums()).await;
    log!("enable RICK (bob): {:?}", rc);

    let rc = enable_electrum_json(&mm_bob, MORTY, true, marty_electrums()).await;
    log!("enable MORTY (bob): {:?}", rc);

    // Enable coins on Alice side. Print the replies in case we need the address.
    let rc = enable_electrum_json(&mm_alice, RICK, true, doc_electrums()).await;
    log!("enable RICK (bob): {:?}", rc);

    let rc = enable_electrum_json(&mm_alice, MORTY, true, marty_electrums()).await;
    log!("enable MORTY (bob): {:?}", rc);

    start_swaps(&mut mm_bob, &mut mm_alice, pairs, maker_price, taker_price, volume).await;

    mm_alice
        .stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS)
        .await
        .unwrap();
    mm_bob.stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS).await.unwrap();
}

#[wasm_bindgen_test]
async fn test_mm2_stops_immediately() {
    register_wasm_log();

    let pairs: &[_] = &[("RICK", "MORTY")];
    test_mm2_stops_impl(pairs, 1., 1., 0.0001).await;
}

#[wasm_bindgen_test]
async fn test_qrc20_tx_history() { test_qrc20_history_impl(Some(wasm_start)).await }

async fn trade_base_rel_electrum(
    mut mm_bob: MarketMakerIt,
    mut mm_alice: MarketMakerIt,
    bob_path_to_address: Option<HDAccountAddressId>,
    alice_path_to_address: Option<HDAccountAddressId>,
    pairs: &[(&'static str, &'static str)],
    maker_price: f64,
    taker_price: f64,
    volume: f64,
) {
    // Enable coins on Bob side. Print the replies in case we need the address.
    let rc = enable_utxo_v2_electrum(&mm_bob, "RICK", doc_electrums(), bob_path_to_address.clone(), 60, None).await;
    log!("enable RICK (bob): {:?}", rc);
    let rc = enable_utxo_v2_electrum(&mm_bob, "MORTY", marty_electrums(), bob_path_to_address, 60, None).await;
    log!("enable MORTY (bob): {:?}", rc);

    // Enable coins on Alice side. Print the replies in case we need the address.
    let rc = enable_utxo_v2_electrum(
        &mm_alice,
        "RICK",
        doc_electrums(),
        alice_path_to_address.clone(),
        60,
        None,
    )
    .await;
    log!("enable RICK (alice): {:?}", rc);
    let rc = enable_utxo_v2_electrum(&mm_alice, "MORTY", marty_electrums(), alice_path_to_address, 60, None).await;
    log!("enable MORTY (alice): {:?}", rc);

    let uuids = start_swaps(&mut mm_bob, &mut mm_alice, pairs, maker_price, taker_price, volume).await;

    wait_for_swaps_finish_and_check_status(&mut mm_bob, &mut mm_alice, &uuids, volume, maker_price).await;

    log!("Checking alice recent swaps..");
    check_recent_swaps(&mm_alice, uuids.len()).await;
    log!("Checking bob recent swaps..");
    check_recent_swaps(&mm_bob, uuids.len()).await;

    for (base, rel) in pairs.iter() {
        log!("Get {}/{} orderbook", base, rel);
        let rc = mm_bob
            .rpc(&json!({
                "userpass": mm_bob.userpass,
                "method": "orderbook",
                "base": base,
                "rel": rel,
            }))
            .await
            .unwrap();
        assert!(rc.0.is_success(), "!orderbook: {}", rc.1);

        let bob_orderbook: OrderbookResponse = serde_json::from_str(&rc.1).unwrap();
        log!("{}/{} orderbook {:?}", base, rel, bob_orderbook);

        assert_eq!(0, bob_orderbook.bids.len(), "{} {} bids must be empty", base, rel);
        assert_eq!(0, bob_orderbook.asks.len(), "{} {} asks must be empty", base, rel);
    }

    mm_bob.stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS).await.unwrap();
    mm_alice
        .stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS)
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn trade_test_rick_and_morty() {
    let coins = json!([rick_conf(), morty_conf()]);

    let bob_policy = Mm2InitPrivKeyPolicy::Iguana;

    let bob_conf = Mm2TestConfForSwap::bob_conf_with_policy(&bob_policy, &coins);
    let mm_bob = MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();

    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    Timer::sleep(1.).await;

    let alice_policy = Mm2InitPrivKeyPolicy::GlobalHDAccount;
    let alice_conf = Mm2TestConfForSwap::alice_conf_with_policy(&alice_policy, &coins, &mm_bob.my_seed_addr());
    let mm_alice = MarketMakerIt::start_async(alice_conf.conf, alice_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();
    Timer::sleep(2.).await;

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();

    let alice_path_to_address = HDAccountAddressId::default();

    let pairs: &[_] = &[("RICK", "MORTY")];
    trade_base_rel_electrum(
        mm_bob,
        mm_alice,
        None,
        Some(alice_path_to_address),
        pairs,
        1.,
        1.,
        0.0001,
    )
    .await;
}

#[wasm_bindgen_test]
async fn trade_v2_test_rick_and_morty() {
    register_wasm_log();

    let coins = json!([rick_conf(), morty_conf()]);

    let bob_conf = Mm2TestConf::seednode_with_hd_account_trade_v2(Mm2TestConfForSwap::BOB_HD_PASSPHRASE, &coins);
    let mm_bob = MarketMakerIt::start_async(bob_conf.conf, bob_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();

    let (_bob_dump_log, _bob_dump_dashboard) = mm_bob.mm_dump();
    Timer::sleep(1.).await;

    let alice_conf =
        Mm2TestConf::light_node_with_hd_account_trade_v2(Mm2TestConfForSwap::ALICE_HD_PASSPHRASE, &coins, &[
            &mm_bob.my_seed_addr()
        ]);
    let mm_alice = MarketMakerIt::start_async(alice_conf.conf, alice_conf.rpc_password, Some(wasm_start))
        .await
        .unwrap();
    Timer::sleep(2.).await;

    let (_alice_dump_log, _alice_dump_dashboard) = mm_alice.mm_dump();

    // use account: 1 to avoid possible UTXO re-usage between trade_v2_test_rick_and_morty and trade_test_rick_and_morty
    let bob_path_to_address = HDAccountAddressId {
        account_id: 1,
        chain: Bip44Chain::External,
        address_id: 0,
    };

    // use account: 1 to avoid possible UTXO re-usage between trade_v2_test_rick_and_morty and trade_test_rick_and_morty
    let alice_path_to_address = HDAccountAddressId {
        account_id: 1,
        chain: Bip44Chain::External,
        address_id: 0,
    };

    let pairs: &[_] = &[("RICK", "MORTY")];
    trade_base_rel_electrum(
        mm_bob,
        mm_alice,
        Some(bob_path_to_address),
        Some(alice_path_to_address),
        pairs,
        1.,
        1.,
        0.0001,
    )
    .await;
}

#[wasm_bindgen_test]
async fn activate_z_coin_light() {
    warn!("Skipping activate_z_coin_light since it's failing, check https://github.com/KomodoPlatform/komodo-defi-framework/issues/2366");
    // let coins = json!([pirate_conf()]);
    //
    // let conf = Mm2TestConf::seednode(PIRATE_TEST_BALANCE_SEED, &coins);
    // let mm = MarketMakerIt::start_async(conf.conf, conf.rpc_password, Some(wasm_start))
    //     .await
    //     .unwrap();
    //
    // let activation_result =
    //     enable_z_coin_light(&mm, ARRR, PIRATE_ELECTRUMS, PIRATE_LIGHTWALLETD_URLS, None, None).await;
    //
    // let balance = match activation_result.wallet_balance {
    //     EnableCoinBalance::Iguana(iguana) => iguana,
    //     _ => panic!("Expected EnableCoinBalance::Iguana"),
    // };
    // assert_eq!(balance.balance.spendable, BigDecimal::default());
}

#[wasm_bindgen_test]
async fn test_get_wallet_names() {
    const DB_NAMESPACE_NUM: u64 = 1;

    let coins = json!([]);

    // Initialize the first wallet with a specific name
    let wallet_1 = Mm2TestConf::seednode_with_wallet_name(&coins, "wallet_1", "pass");
    let mm_wallet_1 =
        MarketMakerIt::start_with_db(wallet_1.conf, wallet_1.rpc_password, Some(wasm_start), DB_NAMESPACE_NUM)
            .await
            .unwrap();

    // Retrieve and verify the wallet names for the first wallet
    let get_wallet_names_1 = get_wallet_names(&mm_wallet_1).await;
    assert_eq!(get_wallet_names_1.wallet_names, vec!["wallet_1"]);
    assert_eq!(get_wallet_names_1.activated_wallet.unwrap(), "wallet_1");

    // Stop the first wallet before starting the second one
    mm_wallet_1
        .stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS)
        .await
        .unwrap();

    // Initialize the second wallet with a different name
    let wallet_2 = Mm2TestConf::seednode_with_wallet_name(&coins, "wallet_2", "pass");
    let mm_wallet_2 =
        MarketMakerIt::start_with_db(wallet_2.conf, wallet_2.rpc_password, Some(wasm_start), DB_NAMESPACE_NUM)
            .await
            .unwrap();

    // Retrieve and verify the wallet names for the second wallet
    let get_wallet_names_2 = get_wallet_names(&mm_wallet_2).await;
    assert_eq!(get_wallet_names_2.wallet_names, vec!["wallet_1", "wallet_2"]);
    assert_eq!(get_wallet_names_2.activated_wallet.unwrap(), "wallet_2");

    // Stop the second wallet
    mm_wallet_2
        .stop_and_wait_for_ctx_is_dropped(STOP_TIMEOUT_MS)
        .await
        .unwrap();
}
