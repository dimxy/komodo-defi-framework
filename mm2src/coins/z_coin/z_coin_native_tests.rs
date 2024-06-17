use bitcrypto::dhash160;
use common::{block_on, now_sec, one_thousand_u32};
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_test_helpers::for_tests::zombie_conf;
use std::path::PathBuf;
use std::time::Duration;
use zcash_client_backend::encoding::decode_extended_spending_key;

use super::{z_coin_from_conf_and_params_with_z_key, z_mainnet_constants, Future, PrivKeyBuildPolicy,
            RefundPaymentArgs, SendPaymentArgs, SpendPaymentArgs, SwapOps, ValidateFeeArgs, ValidatePaymentError,
            ZTransaction};
use crate::z_coin::{z_htlc::z_send_dex_fee, ZcoinActivationParams, ZcoinRpcMode};
use crate::{CoinProtocol, SwapTxTypeWithSecretHash};
use crate::{DexFee, DexFeeBurnDestination};
use mm2_number::MmNumber;

#[tokio::test]
async fn zombie_coin_send_and_refund_maker_payment() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let pk_data = [1; 32];
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin = z_coin_from_conf_and_params_with_z_key(
        &ctx,
        "ZOMBIE",
        &conf,
        &params,
        PrivKeyBuildPolicy::IguanaPrivKey(pk_data.into()),
        db_dir,
        z_key,
        protocol_info,
    )
    .await
    .unwrap();

    let time_lock = now_sec() - 3600;
    let maker_uniq_data = [3; 32];

    let taker_uniq_data = [5; 32];
    let taker_key_pair = coin.derive_htlc_key_pair(taker_uniq_data.as_slice());
    let taker_pub = taker_key_pair.public();

    let secret_hash = [0; 20];

    let args = SendPaymentArgs {
        time_lock_duration: 0,
        time_lock,
        other_pubkey: taker_pub,
        secret_hash: &secret_hash,
        amount: "0.01".parse().unwrap(),
        swap_contract_address: &None,
        swap_unique_data: maker_uniq_data.as_slice(),
        payment_instructions: &None,
        watcher_reward: None,
        wait_for_confirmation_until: 0,
    };
    let tx = coin.send_maker_payment(args).wait().unwrap();
    log!("swap tx {}", hex::encode(tx.tx_hash_as_bytes().0));

    let refund_args = RefundPaymentArgs {
        payment_tx: &tx.tx_hex(),
        time_lock,
        other_pubkey: taker_pub,
        tx_type_with_secret_hash: SwapTxTypeWithSecretHash::TakerOrMakerPayment {
            maker_secret_hash: &secret_hash,
        },
        swap_contract_address: &None,
        swap_unique_data: maker_uniq_data.as_slice(),
        watcher_reward: false,
    };
    let refund_tx = coin.send_maker_refunds_payment(refund_args).await.unwrap();
    log!("refund tx {}", hex::encode(refund_tx.tx_hash_as_bytes().0));
}

#[tokio::test]
async fn zombie_coin_send_and_spend_maker_payment() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let pk_data = [1; 32];
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin = z_coin_from_conf_and_params_with_z_key(
        &ctx,
        "ZOMBIE",
        &conf,
        &params,
        PrivKeyBuildPolicy::IguanaPrivKey(pk_data.into()),
        db_dir,
        z_key,
        protocol_info,
    )
    .await
    .unwrap();

    let lock_time = now_sec() - 1000;

    let maker_uniq_data = [3; 32];
    let maker_key_pair = coin.derive_htlc_key_pair(maker_uniq_data.as_slice());
    let maker_pub = maker_key_pair.public();

    let taker_uniq_data = [5; 32];
    let taker_key_pair = coin.derive_htlc_key_pair(taker_uniq_data.as_slice());
    let taker_pub = taker_key_pair.public();

    let secret = [0; 32];
    let secret_hash = dhash160(&secret);

    let maker_payment_args = SendPaymentArgs {
        time_lock_duration: 0,
        time_lock: lock_time,
        other_pubkey: taker_pub,
        secret_hash: secret_hash.as_slice(),
        amount: "0.01".parse().unwrap(),
        swap_contract_address: &None,
        swap_unique_data: maker_uniq_data.as_slice(),
        payment_instructions: &None,
        watcher_reward: None,
        wait_for_confirmation_until: 0,
    };

    let tx = coin.send_maker_payment(maker_payment_args).wait().unwrap();
    log!("swap tx {}", hex::encode(tx.tx_hash_as_bytes().0));

    let spends_payment_args = SpendPaymentArgs {
        other_payment_tx: &tx.tx_hex(),
        time_lock: lock_time,
        other_pubkey: maker_pub,
        secret: &secret,
        secret_hash: secret_hash.as_slice(),
        swap_contract_address: &None,
        swap_unique_data: taker_uniq_data.as_slice(),
        watcher_reward: false,
    };
    let spend_tx = coin.send_taker_spends_maker_payment(spends_payment_args).await.unwrap();
    log!("spend tx {}", hex::encode(spend_tx.tx_hash_as_bytes().0));
}

#[tokio::test]
async fn zombie_coin_send_dex_fee() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let priv_key = PrivKeyBuildPolicy::IguanaPrivKey([1; 32].into());
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin =
        z_coin_from_conf_and_params_with_z_key(&ctx, "ZOMBIE", &conf, &params, priv_key, db_dir, z_key, protocol_info)
            .await
            .unwrap();

    let dex_fee = DexFee::WithBurn {
        fee_amount: "0.0075".into(),
        burn_amount: "0.0025".into(),
        burn_destination: DexFeeBurnDestination::BurnAccount,
    };
    let tx = z_send_dex_fee(&coin, dex_fee, &[1; 16]).await.unwrap();
    log!("dex fee tx {}", tx.txid());
}

#[tokio::test]
async fn zombie_coin_send_standard_dex_fee() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let priv_key = PrivKeyBuildPolicy::IguanaPrivKey([1; 32].into());
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin =
        z_coin_from_conf_and_params_with_z_key(&ctx, "ZOMBIE", &conf, &params, priv_key, db_dir, z_key, protocol_info)
            .await
            .unwrap();

    let dex_fee = DexFee::Standard("0.01".into());
    let tx = z_send_dex_fee(&coin, dex_fee, &[1; 16]).await.unwrap();
    log!("dex fee tx {}", tx.txid());
}

/// Use to create ZOMBIE_wallet.db
#[test]
fn prepare_zombie_sapling_cache() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let priv_key = PrivKeyBuildPolicy::IguanaPrivKey([1; 32].into());
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin = block_on(z_coin_from_conf_and_params_with_z_key(
        &ctx,
        "ZOMBIE",
        &conf,
        &params,
        priv_key,
        db_dir,
        z_key,
        protocol_info,
    ))
    .unwrap();

    while !block_on(coin.is_sapling_state_synced()) {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[tokio::test]
async fn zombie_coin_validate_dex_fee() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = default_zcoin_activation_params();
    let priv_key = PrivKeyBuildPolicy::IguanaPrivKey([1; 32].into());
    let db_dir = PathBuf::from("./for_tests");
    let z_key = decode_extended_spending_key(z_mainnet_constants::HRP_SAPLING_EXTENDED_SPENDING_KEY, "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe").unwrap().unwrap();
    let protocol_info = match serde_json::from_value::<CoinProtocol>(conf["protocol"].take()).unwrap() {
        CoinProtocol::ZHTLC(protocol_info) => protocol_info,
        other_protocol => panic!("Failed to get protocol from config: {:?}", other_protocol),
    };

    let coin =
        z_coin_from_conf_and_params_with_z_key(&ctx, "ZOMBIE", &conf, &params, priv_key, db_dir, z_key, protocol_info)
            .await
            .unwrap();

    // https://zombie.explorer.lordofthechains.com/tx/806792b7bac6b2289351ef159dc23ef5b71a4eeb9c5391847768d216be333bf6
    let tx_hex = "0400008085202f890000000000008be20600e8030000000000000147c7eb3adc7e098bd70d02ece9ecae8ddad818bc9d3bb6c25a3a4cacc42daddae715ea6f1025e3bbfc71a38f9c45c1ae40df51122d7c7351d74699728b97962add0eeb2675c4e1efb8dff71838721249e4f78fc096c740cbed0763cea130c0b8d20bdfe23c171f5ebae49760c3efb403285fdac04d939f9707db337aa2221528a37d1edd9d3927cd645dc15669f6371dc667651b8b3d2e6868e72ede5ae38d5089ff19af76c595759432b937500b8e81acb5c2aa1cecb83646636fccdaf430b8e8d058ed292bbcb3bd54eefb7ab9df16a9a9315b89a0c7776e26fc2183ff190b17407f4a498d978da737cc2f6a10772a10e1665fdf444fa8870d8d917f0d136f1df159af3a4ecd72a835a275973dd1cab952d57a2253b77dd309580b89077a51ba816757f4c43cd61acc978278316f1062e0341d2d066773a342975d99f50f2046305808950da46707666214b854cfde0b0719d34426ecef37ebbf30b675b6a7897041495f78568cc4cf4cd1b3bdf06baac9a65fd30130b0bd6b2966bb5aa70d0366256ec496e5f75daa0ad61eaa086b9f879b88df488b354c0b7663dab5e50448835be0fa7aa0b66ca8fe10b1ed99c97f4291fd7e91b13a1b2229a9b393a753667db886e4361a05322854cf5a0b970ee1e9b35a2dfac257ef2a977340062b628cfd8d969015618a9ebaebe274f131c88292284d622c2ee6a37d0519e182035178e794665a6fcefd4aeff3c468b50be13be49476a0131814f073eb93c22fcc17384e2f0ac54174d20663d06807467f22d2035ba22e6dab761cacef194fb20edfbe1abe91b07d72de577a5242438a3c85229f67b67bb582ceae748dbf5e7e77b7d8f17deaafc9cbb5a8d871ca23081ccd7d525236c83bc163e4b3fc7230606c23ed602f38afedbb55ceee90f64a513aa3d4599fe9c5ae55dd271eaac3941c551340e2ccbdc421b4d342e07823f6d35d4adfa599f2ad3b8c0b6e3279ec237b8ba11d98d912d00ee1ddb2d863b14ad8ed4e7bcdc3ba39bc2d427a5c3c93ee9db089ade88734acf26a4c380210a19b79c05fbe7725a8f1e9b0dc4b461a6eacee3cb72333fe52a3a7b70ed56c45d672862dbb442df4fa494ff02b60eacc00ec2923e838c51f857626bfc40e890c8514b096c892333bf5d92860392a972e508416422fe858c6c7412409d5f2ca2c8f754601541ac8b0c128219800a7e59d46f70362663b6dc059ccb2de3cd270677e023fa31623a39635db1d360924910ae4857db5e0888e134f1ad1a830dd53f56ff7d794ed81960ac4fb7952d44ebb849d8bbb38b45b7028c94076d24c48c1aaa5e8e40567775efbda870b9842116674cab3d0f969429e874d3f384bd26fd75bf7ad3fb7210cd688ba5a8cf7030e6b263e560b2e65761708df860288cf038904430104ec70ef61a8c1afe0ef8268a3ff900b054e6bdef58518e91f5ae0038b03f70a5fb23117c558b63c58edb55ced12d2aa130178a74f8872c0d3baf329d6ce3eb6b488cf6923c0881e7b79979228ae16215138bd0db779ec1c9bb4d3f1c4d3e57d2525ac50c68d73193fc24590903df063f872e5ef2dd9d2e6364eeaccf78655d05f8c052462f5b731b78f1409fb891b33a537596752aa2d2747edf6c70a1a58cee918f85b02a945e061e8df2f1382372019465730a0853f8494a75c86a521f40955453bfbf47e12d3949e9948d2d50b148d026559af350b5ee3c8b7400c6e801491a84eba638f88970bba50e17cfb48f4b5c4f0431b0c17f72b56d4a3f2ad00cf2e9f33555bccfadd7b3a0ee92b57a2be2f73ebba6eb2b1f1b584235e412c50b0baa0617b3045ec3ad03a1ed21fdde6dd9a3653fb2b74527d3595b8e15357e6302f83c54ff2255ec6695a184660eab00eed77d2dae82957a957acb2d1b2a5b2a9555f917802a14c512eba76907153ad2cd28b3cafb3c6d9a1076fd3c6b7e6a791f98e34a4a454cd69ca930b38875bad0a4aca6030ac2f7d66d39d56e7489d7b55dae58e786816cc396b86aa5a98695ab5dbf2dde4d545f151444aedd7780733a74a6a117a12915fbeb6b9ad32f893871b2813d2f1a3067d7f7bc90789b9ba66240e416153f1fce98fdd7507f7a337e59aa0d70508e833254bd94c105ee3f0c8a3945aa2fd1c4c330e70676f545a5907475f96dc474f56fe61e70194395fd6a0b9ec30364059016b4a9df1139a6bcaa8f65eb440729fdf063b779344ad6d853e9adb5601800d0c61e15b823e5bc4d6aef195591b5888aaeec17dc3fc2ee7ce3a1ffbb713e8d87e35680870e367bda8ae8ff1e4e938c18fd7435f8c861b7314c20dbf0c3c218c37bd149923df4efd64afa3ce9b8b36b9340cadd07499ccc021b43a47bab5038918a5b42fa3839b1576d34446673ef6c006b8a5cd6096621e8e0af387b20669f2bda27dd8003b8fd304a7727201c296bd334baaee58fdde56a5c5464935834bf36f14b9943cff582f5110542581c6094eeeba80c634f6ddb9b085fc1c6002726219a6446a2c1d54d02bfbb7982876d2423b815d460a9ce88c516ee8e3d1b4720c921b7619bafe5c5dbb48b8317180e3e8785192198a0fa31700bb9da0e40e48d3b2297223e28dd499ad019a13a7a6e666014db2706539ca2b0bdbe4a1a38cbf90cfff23dc0e612768b4977c5a60056f5ae623804105302bddf263dce6a1d4e6e8acd2a5b8c91ba0d49b3d100b5c93bd5d03052736ce62ba61d303856171aecf42d4d80ded0adcb18abbda42d1daf6f31f75502e5286b8698dcca901b250fd3a384fa6a292d907fd1bbce07a7aeb56fde4398e0e4592dbaf718964e704042adeedc7a2f3778b11d2cf9cbc2ec2abfe8d07dc8129361572ebd0c1b88a1cac5068a3f7aadf6b9d57d96d393697dc6865e768376b87bcaeebf6c4e6c81515c3c9736c930b48997c928abf44a1476b96cb913588c2c0d5369466b85585706ccd01bea7ec303abb27e50fa98d6a59717f13fb986f2595225e13b8572f3cddb02079bffad0c1054dbc7a62855eec86be3e63bb6f68a30bcbb6f54c2912ce0b4659d3ac467f752d49eb844ccf6c4f120e6dd3f2c8c68134402c238305d77f06a1a5e2bfea3b74e1c4270338ff77460cd3090cd6ed88031e437dd08ab784ecc612a569bbbc5e669ed9583c85874dd35c33acc086a4f9f4f0bfd9213019dfc689dda3e6a65d7ba23b1be711d747ee65404cf736b757c208ed1544c2a29a54d25dabeee331809c82b5c9081126dbb25bb27b451533aa274b25f7cc179f299ce59500e0f2818b16bfe124ca29868be761d28a3d937c1bc9131f3c9f4d6354288ad1eb4fdb807d84ba2b136deb8e217d053188a2ba4690491cdf1dfa88a9379069a2facd354f3762fb2964a6f3b154b4c51d90d4a4e0a58101fb1134d354724e944e1fc0c74c5c2dcc64a38cbaaa68623c2cfbfdba26572348e9d989431c7430a0ca3d9f79b3a8e8a5b8fdb855cd00df4503d300b6755509521c09f8463e92ceae63b2af767a60b8bd20d9c8d917b30b1e18035f5abc3d374ff99e1c1bdf26af68758f1d60b3552dc8868aa2ffa12a4228a52625335935b44c8145923c755549ca033e101ba9969ee55014e66e6271337ae161f45a2026a0e45e33182112e697c655f4ed649bcf111d0c80a720b091a90e095728b00e76c444ad89aeb328d2c4de249922633483eadec699edec0c3c44be60a62a0e2099e9bfef8b6a4072105a3ad0d2110fa4d3740eddfbb08a989f6d2503a85ee7fa664703ebbd2e718abf2c9f5b68ec0eca9568c2facba591987d0c271b1819055d15852fb5c81ac8d0114fca855dacf693d06cfc2878a810da975f815165d47ebb8e0ac63f98cb96304e9d2c44c3083a776f61d480a53056e67e1a7bd92034d21b03a7f4343c105d17a42b4705ff9b19f8f9afa840e43977df168d5779900008584acfd0f1985035ec820fffe37c37fa494bab95723c0b4661a2f8cbe042b6f6bc6366c5cd79b9fa839c714feffd942c24a76bf77d5d16580540e2a33d2a09f0c471ba6577532aa43ce34696ca1fe52e3ad14cf46e5aedced11d7578c3521a8f1cd97d7d958f16b7a241a0b887c8b8217ffcd0f46ab60ede9896c69cf92941d653353ef82a83bd00bfafda5da757e57bc055aa40ffc1303469c215d922500c126f4c501815b067ff0269419c001f598c087552ba5952cc1d4797289918c0a98a18ae82f43a31069f8d5f29ea1c64c1edc2b8211dfd6df362b3517ff4a7879c1fc996d32af9fbcf643c5f6011654b07ebe6b653b481b988556ffb7248db89225014a0498f861e4c62d75711c255ab813d5193ebcaaf00678bcbd38f82180a60530e528affd04827c09514f8f8aca872e5e345ce86b9e55a4f85de9fb07758d643a085f042e4758406c237982aac3f85cf900b4216ecb2248077a5c8439610e7e10e5286870327a3ad7b262d2e1043b3546620463f48a257e931c5b347ed5b9a5633694668e023a0ec10e01b0ca6a23b350c1ba2bb74bbefbae71e905f31a8705bf0d2ab491f798e2631fa4592a409bf685660c5e04afed0084a8e93bad2008a3ffcf8575a64627e635a33e32b393cbc55fffb2f18afa574d89e29d26604ef02deac1fa2ff15a272d83a38cc8d7b63eae15ffb4bc1bc3ce029def5c989c10c";
    let tx_bytes = hex::decode(tx_hex).unwrap();
    let tx = ZTransaction::read(tx_bytes.as_slice()).unwrap();
    let tx = tx.into();

    let expected_fee = DexFee::WithBurn {
        fee_amount: "0.0075".into(),
        burn_amount: "0.0025".into(),
        burn_destination: DexFeeBurnDestination::BurnAccount,
    };

    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        dex_fee: &DexFee::Standard(MmNumber::from("0.001")),
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    // Invalid amount should return an error
    let err = coin.validate_fee(validate_fee_args).wait().unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("Dex fee has invalid amount")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    // Invalid memo should return an error
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        dex_fee: &expected_fee,
        min_block_number: 12000,
        uuid: &[2; 16],
    };
    let err = coin.validate_fee(validate_fee_args).wait().unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("Dex fee has invalid memo")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    /* Fix realtime min_block_number to run this test:
    // Confirmed before min block
    let min_block_number = 451208;
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        dex_fee: &expected_fee,
        min_block_number: ,
        uuid: &[1; 16],
    };
    let err = coin.validate_fee(validate_fee_args).wait().unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("confirmed before min block")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    } */

    // Success validation
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        dex_fee: &expected_fee,
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    coin.validate_fee(validate_fee_args).wait().unwrap();

    // Test old standard dex fee with no burn output
    // TODO: disable when the upgrade transition period ends

    // https://zombie.explorer.lordofthechains.com/tx/9890bdf3216de5703db1191e742507e6f3d99153c6f4649ac1f5d1a6c9dc4ef2
    let tx_2_hex = "0400008085202f890000000000004bf30600e80300000000000001ce2e86d5be524b09c499ee24300e602f5a23ad2ad2caf1ea5006e11635f7c289529cc9e4a07f6c1da4effa9c7732532f9f4f5a2cfa4a56b95b895fb0d5e4605b52b47a48061d9273be3768a128565c4a322b425b91eac5cda7c472fa569063b32aeda9101b4977c3f4c88a86b10a56e7e26836dd27d5e284e633a8bf9275cb5fb4e591342ae176d6ee799f90ebbaf8b350876dbdcd35fd8b9c3300b344a23a17356fa530277e246ea9bd971c0c72b4f2a982af8e69ecdd0ba3aef60cf3680f87ec620f59ad1fbde772ec31868bb9f7ddbe9176cc37bd6bc81a70b4a627ec298a0bfe947d69cd6038baf86217b03b7036e7f7b72da7105748ff7ed1590b82d3fa91ee08532a9e97af2aa7a975e1e1818f8df40f6016d66f3e3b1b7b8292f028d6f597327d6beff979abd4292776824509b565e1b07a4cea07af33c6867e51223e6559d3e6501049905388b732bad6cc038c58145f83a92e57f452d3c6da06c219320246cc6bbb94f97312bb6a7e235c44b587ab81c0b0e13818e9323c31a2f3010247d5d2f4a788ea8a7b4d86bd27e5282ca5d37fe4622918b5315c7dd60349093bbc3f4e855d0f37de1b5ac694a94e09e87c30dfe9480b2564fc2ff059ca33165ea9def502d9099bad081fe2b95595f9566ac22f7ad913ab5a0705ef4d5ede9b245159592427e9feaf4752c93d5264091371ce41b39c7180675c004c148fba84fa48e383edac296c685c1aba3d21fbeade39cf2e3efd9713fbc93985d0e791dd6703ed40d4b936697579da3a0418b74343769fe3f1c1f1babed4d710cb8a2e6705711cc5dcf856c4b42ae2cc6c59ebeb188ea18a62f32565bef077a0a6d7e0246f4fb51c358f41780a75ed07592428ade8b74bbbdb45cfd130fae9c7f35ffa4ecaa30bc3e399c60b0726221ddff730003e5591093df401bba3998022a9a686bdaf4dc2b49879b11d49d507c2003aa9dc4bc968b0f9f0bb5f5c95da2687e24eac51ce779109c1699a804193fac4c167acc274662f071b9c0727e5f643b2da18f088e461d7b6cedfc36722f8c7f8c0c754283cdef8fb9bd2a9df1e527835f4182f32f67f044a44a386db0aba4d94863606c24fc39ffbc5649ce03d4ba2050943549980a7a713a6b4fccba397b86701a07c35193643871c5e582230f0e702be1781d5cd37e2b6d6c7f1ff522232c5e60f5060f71c5416892677ec42252c2cad4e3c62d98923f3aee515a91390f80ba1ea1a2229fce8ca74b63f24132894ee4f28df962c80093e7f0668db63c392d9cb1b5d048e10e1b9b15787a177f5c5ee5e48d84eae3376842dfa05b5480645bae0bbd3255e76c2fab28741f31b745867f74502e32f6dae83f464d4fff583c10e4171e30d32f4debdbfee1c75bb540752bd97257ace24ef4760e8a0631426eb829d29cab26f99915d5487d555df334cac23931ef04bbec58c9e10847adc3ca6de8141210c2f4a61ba3d9f3ad257350763af4eba728583a6112bd7f0d16eb79527955bca3265eaa8678441253ff015be98dd72623c16cc64cc6f4c2fac2b30282a947806bcabe7dae41b246e01374fb6bf8661df7f6d1ec98f231a32c58558f95c7a08f0c0cca0ed1f89a495fa39a91915ab63ee71f3c46da192260eef9337ab0c82aa1a79c464a98a7f6640ce38c3524025be76b45bbe2dd9a3321c59bcca79b3fc894a608fd6bec7c0495c4d36bcb32f68e6f72f8a578fe7256e7fc398e59f68557577b122cfdc090ec1135b97692d6d75cb73bf7be95f790321cdaf1c47612749fc452029804020e19ee04e66df2b21d69b265f6e863ec083f8c472f654b39497fca9c2b61046adfe875a5e90cda3a4a2c07ecb73ab5d25bb942cb6f2e3c38dc1f924d862afa6bce528ee785dc25207ffc62ac3436369ec0e7723eb258427b8def8af709b017931b10f3ea385a9c6d46fd953f71b5d37a318518100c06f01ed4a7553ef96981f0b3b6cf746c97be84a9c4a6a3b7db3818ce6e12c413809b00a8e5af36506a70fa95979fafc19aeea85f9bb2d967fec73e3970d8242fb17fc2cc9b0bc0aaf01d62c552fdb73023d640f8d210050d8628537c2413893238efa398d5c52900e2040864bf29c98b24e4b8be049b134be92b7390b0a7960851b142cb1c3498efef58c1229fdb15b645c264a05c91ebe0f76b4b649f47a6e666a07ee32acd46d612e5f9ed913d2217154cb4fcf2d93624ae2fe21e65674b2b660e6b232bdb30424e6e14be89dad54b37139aee6aba23829cefd09a4555fd6ef102c44698d940ac8e7a193bb2a6591293fcb78224dba61c825dfea42fc849f19bbed4b79a7eefae13d92c3aa5a9eaa48788662da56f707b36cd82c639a8f0f208e94c8f733cbacca2e3e708198b721c03c3eb53ef0a77a6cb1378a8fc65b29bb8fbc07433ba85bbcbc290a86fd47434512f0f5d79db744ccb8762f3baaa8d3660ee9066e88df2973e010389d0c770b6367c31d404d03392df66dae4def35274ff479670bfab8571c11c3df7c07a19094f83a4e798b8cbf386a5fed0f8e2b9ed55953526ef4d4c76d9fd28e241043642101434a95e091a7f8b3cbafafbedceef37b5396dbe84bd01e0b569ee9d615d200e1d52ac4beac89395f797051dd24d449f9aa7f7f1005b2ffceb6a8698e634320772fa4f855de0252294dc8518a937b91b1bbebc230c6d93569441e2ce430f99ae099bdb992cf58980be033c8edfc36470e17c1ea326ea780b35549ea4c3cb222f4e87bb118df4fbb1841509e9dff3a3de8406a8bdde2108ade128e705f14001d38d6f382f27cefbf9e41a2369085c1897d1ec8c40ed4c63306e4c0aee3f35a1ba5e756881241e895c4a3af6ef6e0f712f8f01cf2cd0b03324d683d2093d4f1b73e029afde83aeed8ca7dd04b824797812d78e5b23e9003d3171f02070c2de75e7f9c6b6b7352bbccdd20df2c34c9a98338e00f487b00e6c02d610421b82d299ff35c9eb3b6fca8bc18804844b27c97bbb7dcb688f14e1491029bc94ab957aca94acb17a6e22bc6aae221b710b45e8eeaed33be65b0be10407574b1593c5210893306cf919b8db7c87f6473395fea950b9996c1e40dceabd5200401342b4f94a95d6ed00f2fdacead095ba3190dfc2e2e4560d01bf79f39342a74d5cede10d8917a949d8069f42971289a35bb82e8d5d87b09427ebb0bd1dca16328d409840929aa50c58fc3c1053b372af6a2e5dbe800bcfcd890cd7d89b5f3dbd9db49805acd89f9a198419a646312c43697700f1833c9c644ca357e9176ce74b0c744ff6b8218589cd1aef963259ff2c9c0ac5ba308";
    let tx_2_bytes = hex::decode(tx_2_hex).unwrap();
    let tx_2 = ZTransaction::read(tx_2_bytes.as_slice()).unwrap();
    let tx_2 = tx_2.into();

    // Success validation
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx_2,
        expected_sender: &[],
        dex_fee: &DexFee::Standard("0.00999999".into()),
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    let err = coin.validate_fee(validate_fee_args).wait().unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("Dex fee has invalid amount")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    // Success validation
    let expected_std_fee = DexFee::Standard("0.01".into());
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx_2,
        expected_sender: &[],
        dex_fee: &expected_std_fee,
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    coin.validate_fee(validate_fee_args).wait().unwrap();
}

fn default_zcoin_activation_params() -> ZcoinActivationParams {
    ZcoinActivationParams {
        mode: ZcoinRpcMode::Native,
        required_confirmations: None,
        requires_notarization: None,
        zcash_params_path: None,
        scan_blocks_per_iteration: one_thousand_u32(),
        scan_interval_ms: 0,
        account: 0,
    }
}
