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
    let db_dir = PathBuf::from("./for_tests"); // Note: db_dir is not used and in-memory db is created (see fn BlockDbImpl::new)
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
