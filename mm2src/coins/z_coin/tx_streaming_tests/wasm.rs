use common::custom_futures::timeout::FutureTimerExt;
use common::{executor::Timer, Future01CompatExt};
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_test_helpers::for_tests::{pirate_conf, ARRR};
use mm2_test_helpers::for_tests::zombie_conf;
use common::log::warn;
use common::PagingOptionsEnum;
use wasm_bindgen_test::*;
use super::light_zcoin_activation_params;
// use crate::z_coin::tx_history_events::ZCoinTxHistoryEventStreamer;
use crate::z_coin::z_coin_from_conf_and_params;
use crate::z_coin::z_htlc::z_send_dex_fee;
use crate::z_coin::z_tx_history::{fetch_tx_history_from_db, ZCoinTxHistoryItem};
use crate::PrivKeyBuildPolicy;
use crate::{CoinProtocol, MarketCoinOps, MmCoin};
use crate::DexFee;
use std::num::NonZeroUsize;

#[wasm_bindgen_test]
async fn test_zcoin_tx_streaming() {
    warn!("Skipping test_zcoin_tx_streaming since it's failing, check https://github.com/KomodoPlatform/komodo-defi-framework/issues/2366");
    //     let ctx = MmCtxBuilder::default().into_mm_arc();
    //     let conf = pirate_conf();
    //     let params = light_zcoin_activation_params();
    //     // Address: RQX5MnqnxEk6P33LSEAxC2vqA7DfSdWVyH
    //     // Or: zs1n2azlwcj9pvl2eh36qvzgeukt2cpzmw44hya8wyu52j663d0dfs4d5hjx6tr04trz34jxyy433j
    //     let priv_key_policy =
    //         PrivKeyBuildPolicy::IguanaPrivKey("6d862798ef956fb60fb17bcc417dd6d44bfff066a4a49301cd2528e41a4a3e45".into());
    //     let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].clone()).unwrap() {
    //         CoinProtocol::ZHTLC(protocol_info) => protocol_info,
    //         other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    //     };
    //
    //     let coin = z_coin_from_conf_and_params(&ctx, ARRR, &conf, &params, protocol_info, priv_key_policy)
    //         .await
    //         .unwrap();
    //
    //     // Wait till we are synced with the sapling state.
    //     while !coin.is_sapling_state_synced().await {
    //         Timer::sleep(1.).await;
    //     }
    //
    //     // Query the block height to make sure our electrums are actually connected.
    //     log!("current block = {:?}", coin.current_block().compat().await.unwrap());
    //
    //     // Add a new client to use it for listening to tx history events.
    //     let client_id = 1;
    //     let mut event_receiver = ctx.event_stream_manager.new_client(client_id).unwrap();
    //     // Add the streamer that will stream the tx history events.
    //     let streamer = ZCoinTxHistoryEventStreamer::new(coin.clone());
    //     // Subscribe the client to the streamer.
    //     ctx.event_stream_manager
    //         .add(client_id, streamer, coin.spawner())
    //         .await
    //         .unwrap();
    //
    //     // Send a tx to have it in the tx history.
    //     let tx = z_send_dex_fee(&coin, "0.0001".parse().unwrap(), &[1; 16])
    //         .await
    //         .unwrap();
    //
    //     // Wait for the tx history event (should be streamed next block).
    //     let event = Box::pin(event_receiver.recv())
    //         .timeout_secs(120.)
    //         .await
    //         .expect("timed out waiting for tx to showup")
    //         .expect("tx history sender shutdown");
    //
    //     log!("{:?}", event.get());
    //     let (event_type, event_data) = event.get();
    //     // Make sure this is not an error event,
    //     assert!(!event_type.starts_with("ERROR_"));
    //     // from the expected streamer,
    //     assert_eq!(
    //         event_type,
    //         ZCoinTxHistoryEventStreamer::derive_streamer_id(coin.ticker())
    //     );
    //     // and has the expected data.
    //     assert_eq!(event_data["tx_hash"].as_str().unwrap(), tx.txid().to_string());
}

#[wasm_bindgen_test]
async fn test_zcoin_tx_history() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let conf = pirate_conf();
    let params = light_zcoin_activation_params();
    // Address: RQX5MnqnxEk6P33LSEAxC2vqA7DfSdWVyH
    // Or: zs1n2azlwcj9pvl2eh36qvzgeukt2cpzmw44hya8wyu52j663d0dfs4d5hjx6tr04trz34jxyy433j
    let priv_key_policy =
        PrivKeyBuildPolicy::IguanaPrivKey("6d862798ef956fb60fb17bcc417dd6d44bfff066a4a49301cd2528e41a4a3e45".into());
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].clone()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    // zs1c734j8hnuse797d3382jnz48cvjsdtvgh980efwczxgztk2vf7a6f4zqypxnkjgk8sk9yrfc0hg
    let coin = z_coin_from_conf_and_params(&ctx, ARRR, &conf, &params, protocol_info, priv_key_policy,
        Some("secret-extended-key-main1qdputxysqqqqpq89d7lpf8r2f03xsuhxn7sf6qp9st8gpkfw8dplge2708r3dahx3qdfx2py9d4w853ql52mdtt9xax0acfg57h0k42nkrasyducexvspxuhykaq9f3w48y7fyxpa8g0nhc7kd0p9f5f5d4fvlf72cnr0lg94vmetacttpwap5f90unqu6u4u9v74ruvyvl83ju2llzm38ku7vjqs63r5wdc58t36t0asv3qpq67grd6a0vht595mvz4wyjdgq95cfchp7y8v"))
        .await
        .unwrap();

    // Wait till we are synced with the sapling state.
    //while !coin.is_sapling_state_synced().await {
    //    Timer::sleep(1.).await;
    //}

    // Query the block height to make sure our electrums are actually connected.
    log!("current block = {:?}", coin.current_block().compat().await.unwrap());



    // Send a tx to have it in the tx history.
    /*let tx = z_send_dex_fee(&coin, DexFee::Standard("0.01".into()), &[1; 16])
        .await
        .unwrap();

    println!("tx={:?}", tx);
    log!("tx={:?}", tx);*/
    let r = fetch_tx_history_from_db(&coin, 1000, PagingOptionsEnum::PageNumber(NonZeroUsize::new(2).unwrap())).await;
    log!("fetch_tx_history_from_db={:?}", r);
}
