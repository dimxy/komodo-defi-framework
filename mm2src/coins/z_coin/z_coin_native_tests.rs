use bitcrypto::dhash160;
use common::{block_on, now_sec};
use mm2_core::mm_ctx::MmCtxBuilder;
use mm2_test_helpers::for_tests::zombie_conf;
use std::path::PathBuf;
use std::time::Duration;
use zcash_client_backend::encoding::decode_extended_spending_key;

use super::{z_coin_from_conf_and_params_with_z_key, z_mainnet_constants, PrivKeyBuildPolicy, RefundPaymentArgs,
            SendPaymentArgs, SpendPaymentArgs, SwapOps, ValidateFeeArgs, ValidatePaymentError, ZTransaction};
use crate::z_coin::{z_htlc::z_send_dex_fee, ZcoinActivationParams, ZcoinRpcMode};
use crate::DexFee;
use crate::{CoinProtocol, SwapTxTypeWithSecretHash};
use mm2_number::MmNumber;

fn native_zcoin_activation_params() -> ZcoinActivationParams {
    ZcoinActivationParams {
        mode: ZcoinRpcMode::Native,
        ..Default::default()
    }
}

#[test]
fn zombie_coin_send_and_refund_maker_payment() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = native_zcoin_activation_params();
    let pk_data = [1; 32];
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
        PrivKeyBuildPolicy::IguanaPrivKey(pk_data.into()),
        db_dir,
        z_key,
        protocol_info,
    ))
    .unwrap();

    let time_lock = now_sec() - 3600;
    //let taker_pub = coin.utxo_arc.priv_key_policy.activated_key_or_err().unwrap().public();
    let secret_hash = [0; 20];

    let maker_uniq_data = [3; 32];
    let maker_key_pair = coin.derive_htlc_key_pair(maker_uniq_data.as_slice());
    let maker_pub = maker_key_pair.public();

    let taker_uniq_data = [5; 32];
    let taker_key_pair = coin.derive_htlc_key_pair(taker_uniq_data.as_slice());
    let taker_pub = taker_key_pair.public();

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
    let tx = block_on(coin.send_maker_payment(args)).unwrap();
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
    let refund_tx = block_on(coin.send_maker_refunds_payment(refund_args)).unwrap();
    log!("refund tx {}", hex::encode(refund_tx.tx_hash_as_bytes().0));
}

#[test]
fn zombie_coin_send_and_spend_maker_payment() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = native_zcoin_activation_params();
    let pk_data = [1; 32];
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
        PrivKeyBuildPolicy::IguanaPrivKey(pk_data.into()),
        db_dir,
        z_key,
        protocol_info,
    ))
    .unwrap();

    let lock_time = now_sec() - 1000;
    //let taker_pub = coin.utxo_arc.priv_key_policy.activated_key_or_err().unwrap().public();
    let secret = [0; 32];
    let secret_hash = dhash160(&secret);

    let maker_uniq_data = [3; 32];
    let maker_key_pair = coin.derive_htlc_key_pair(maker_uniq_data.as_slice());
    let maker_pub = maker_key_pair.public();

    let taker_uniq_data = [5; 32];
    let taker_key_pair = coin.derive_htlc_key_pair(taker_uniq_data.as_slice());
    let taker_pub = taker_key_pair.public();

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

    let tx = block_on(coin.send_maker_payment(maker_payment_args)).unwrap();
    log!("swap tx {}", hex::encode(tx.tx_hash_as_bytes().0));

    //let maker_pub = taker_pub;

    let spends_payment_args = SpendPaymentArgs {
        other_payment_tx: &tx.tx_hex(),
        time_lock: lock_time,
        other_pubkey: maker_pub,
        secret: &secret,
        secret_hash: &secret_hash.as_slice(),
        swap_contract_address: &None,
        swap_unique_data: taker_uniq_data.as_slice(),
        watcher_reward: false,
    };
    let spend_tx = block_on(coin.send_taker_spends_maker_payment(spends_payment_args)).unwrap();
    log!("spend tx {}", hex::encode(spend_tx.tx_hash_as_bytes().0));
}

#[test]
fn zombie_coin_send_dex_fee() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = native_zcoin_activation_params();
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

    let tx = block_on(z_send_dex_fee(&coin, "0.01".parse().unwrap(), &[1; 16])).unwrap();
    log!("dex fee tx {}", tx.txid());
}

#[test]
fn prepare_zombie_sapling_cache() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = native_zcoin_activation_params();
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

#[test]
fn zombie_coin_validate_dex_fee() {
    let ctx = MmCtxBuilder::default().into_mm_arc();
    let mut conf = zombie_conf();
    let params = native_zcoin_activation_params();
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

    // https://zombie.explorer.lordofthechains.com/tx/462d2fe9494561f9fcf77b21d97add8e58845d24006c9b7f17525b3abb795679
    let tx_hex = "0400008085202f89000000000000cc5a0c00e8030000000000000265068137314aacbdcc86318bf223b08b79b35a5a1851b3a1ce5e8d99edd26f2ac947a165e711641e1636178747da5bef80669fcecbc819bbf66659d18540674d9346389b223fa19d6043cf7fe31b9623427a14f988e6a7ec95dd4489e6ea818d3f11d930bc6c83e0fbf7b4e11ab943ebd8da926b5f6dfd0d45917620655a84919509172e7239dcbd3324af6f6ce333b2bb7ef39ee91b8e5cc44afb04dcb868d02e6f8c1bc79000c41d92359e9d2c6467a89dbf707a3c8b36e3dd0bc5e1a9dac5e7b3139b01c30c9245c4aba9e76ae404f6130a7a066e5f8f76a1ade89a966a49041c75b5664a1d9bd9bc931cad8f3a75930f023fe10a42e87e0d93d82522738488eb7875488b5496a419ff6188a892d4abcb421d483a0212b610f330082c6d497b4c474748ce7ac0617cff04ee5c920318aa40d9a9da09028a522fd17385d1eb72d26660c324b5594fd7197b74985e9ee1a79c8d55dd5cba42cd652321d8a34d3b0dce3f98a003864afb0b4fdcea800ce9fe1111013cb630ac7bf03b5ddfc009ad4eb1274a1bebf6a81015744d1066642a4eb01abd84ee161261877c85e6b9bec947a165e711641e1636178747da5bef80669fcecbc819bbf66659d18540674d3aebb412f20ad300cb255ea53de585f24c7e81cb7f8a5642403c8138fe6c27cb1251b91642107ad94db38933043ff9972a82e856a9955f0fc54aefab94ea7786868feefcc679d7c72b0070f1d2d2d1f32fb3820ab54705461085430eee85bdbb5cfb383cf2c5ed554f259199c4804b36abc9e7a0dcbfccb946035b2dd505b1a0f648d8013785ddf1dfed70a94d3524916f2dcfc59478a626528fd126e829d9ec0e3bd7ec6c46ae7c5b7b2844c39996ecf2f9be681be1aeb4120c78baa97a028da5d7e68f082b8fa39509321f8a7200ada073dd4c3998e8a21c93d795323147a0f9de305ed80dff240928e9d3ffe64e4f0d1563f970b542c37d420b68568f5c12e80aa31b84eae9a4cd562f1a9813d5e541bfd49f4a2af3054e3e61c4c966893b46a6fee01fd65a12b396f94010bc8e4a60dc14e14b7ab1111df20f57fbc2b203025c27fccdbebabe770357aabc019892bfa243ea338a35c5586099b63d905958385c781cdc4962bc96645ed2c957cf506d76983a535dddae99d22a70693f116c0a2fda38952c381169fa022de69b853348e2094baa022b65e838eef20c2315e51825c64a284414a686afde870a40b7fbaea80f074ec8c71217e48b32363112a716c626214b9b59500c1d9dc3891814f80979d9bd6b2a95a93fbdf680498f70dbbf0826627eb3257be25614744d32fe3062d8c2d3e52d12e889b9870cd4211aa03fc4c4b380128235021a6c7fb676633e54d1ad7e78388fc1592d1562158bc65bc0d7a4325649a5e91d79863c6e1244fcda26da8ed7d906d5eb2a841ea437e1a422eb8ab6bd66f42492316b2ca084c38ed789f6f2d210db961536e31d2c9a76ba73548617292546538d4b2b9fd0553b2166765a4f3d8d26ba1af98f033c086a9da4656c9ae04cb8394810bd7e3c742c58ae8a2b810eb45253f12061e18adcad69408e3668d6decf5b6d677c0529d83d5fc61f4ea9c334838a595a77b40559c73b166327c2ae94389dfe6187eb6924ee00dcad113c1778bb32be7cd0f56e70746489baa3cff0b72c0073decb59e6012744cde06b7187d90d69e56722c8ef34f3d232e9e2e3625ac509d17663ee449109744c7f6901c01510eae923cb837f26416d6ea307caf13e4d4eccd88b636915855f35ece043c5efdc306d7f25cb28fd311a6b3e14091ee15cb21aa94a44af344859ac5ba62d22083ab2e7b64cf28dff10501c942c19df655232956e5977442c556905306bcfd33e905a16ff88acc4ba184eb2ce0737cd4ec8d515c4b160e87e0a9fb3130268cde51873208b23d020a05a8af2b40739ee0f1bd59687e915f1349bef841ed173283e7bb2d18f069fde51fd4b84e802fbfc8ea52f1fc6d2112ba3b5d84af657aff4b68780785c005c7feda221904efef81fbfe8ebbdd7b0d54234b60a6b471318b9fce6252e8844147ca0c4dcb6a054941546d01dfd0966be19ccbc6c23b156728558378a0504249315c2ffeb986f35dddaa587f1c448194c88d056a91e7ff95528b7eae7f3f554973e6137c30c1150d03800da1d5c0f0a10e62ea83d7bace7e209db8583dc9eb48010fabbbc2692909aaea1a7bca5fae32f968a0bb29048e12fe694809e4c13b69e2b1ab9eb9e01d159dc0b856100d0d65aed1bd8279f1aab18ff05f75a4fceae78f003c405d3715da5af91ad8c7f241ee21330f3215c377730ad88ad84d08dbecbaaea8802c8417634d6a4631f05b1435496b0b809d8253b7546c6b4330645abf749614d80554984b3b79f46eb1017490232910ce26e97a1963128e81de675aabe389bb7d446a9a1a748d49bc2c0d8e511dbb38c5f3bdae5d047694c6b272d7d8e0bbfbf2065ba273c1261ef136110d171a36c4fcd2238c6c772e310203faebd8d3f1ee6b75a24906f3ca05327be04499c339362999426ac3c87831de94641741bdb29c7b25f1fc0b9e4c674b6ee8bcf3fd2f414e855792b00a37d22eb78b7c0955e2a14fb6f0b0d11037eebf98aec5a484bc238d6de5395aa9bae040a7fd00a4e67255abedac63d9fced90d74230ebda117847d4b3bc9b7ebd02c502bbe4740a9290572d694f7e7dcdcf9731d7c115a037042787949de2ef7f17fae5132044849fb84fd9a0fce1c010b71ba784a5927e16aeb3c4d159bc5199b0bf15309e1a56b86b3eda3ca8201372855c1db87e4970ddea25e81f04ad21ff398b027a1c906e1590d857497898ec908772a969dd96d6144efff93f90fb6fb87fe0567ee180ef9a1b9bf1c8590187fbf72cfb7c87743305dcaedfb1a0f9b2dac79ec55962db2ecb771fe5f998632425d843d96015495fbf78c243d437e5776a3ff49be16ad21ee8ed5697d28ec009c627cf9b451d09af654cbe76c191d3bcf35a1dc0b1908a7cab84e76a3805d17e092f90c6dee3518ef97d20858820697b70e0c5d87abb7ca0409044edd242a429054f6d0a3d8d455d1aa5c1d2ce07ca06bcc20b01f178fcb3815e3e432801874b9f55e2fa294477f8360647d58e2ab00605d1a988a0aca60c1e6472aff88103942b0c875832a0ef28ee3a7d77ce6cb79088711f046f84f029651cdb74a2907924a142d10b8fbf38cdb4cc94c728ad6f1f7802aa0c2337c47124a71c23c199aaf4d6c561ef1bf96298482b25278317a677e9065c42373ef5624e48bd7c02a704479c4d2306ba2ac26162f23e9a36d3258da66c67a1b4eff4ba478a75711a6b81457cf95c06b6d4279aa023d1f4214f6e4b55da29f7da145ae442c97e3ad3299679a8343246e3a9500af15152cc48681f7bc33b42a9684a97fdeabe361e98a98e6ae632871a93d0ce973da0f274cae06463c2e8548f9842916d2b68e62d85f0858723eee5768cd8ce569f9357e7d10610a581147620287a05d338b70bdc2016abfdac79304ae73cd134b0f0dc938511d3f81512556aec38d9076c33bf90281d3282deb0416771191f553aa26562afb0929f52563842db2bbd10c9ebb7f81e7747344633214539b149d70af8252f04bf249a5d50dfbc1b215fd6f0939588a0a84c1a6ec2ea25c366cc970a80e9e9bc9e0c799aa199f19ceafb7e7b6d90fba1343833ad11c4cb8e1f5bf005d1eee9643fa002d613166583eba224e2e5b1fc6a8c6a473b372de8c1100f9e8a79d95e042229cde7336ccf8e02467d75452cb9c0e0d4782576af52bed6e549bd89d0aa1743a64886ca62f796bf59aa33be8d2d59748645efabfcdcc660d";
    let tx_bytes = hex::decode(tx_hex).unwrap();
    let tx = ZTransaction::read(tx_bytes.as_slice()).unwrap();
    let tx = tx.into();

    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        fee_addr: &[],
        dex_fee: &DexFee::Standard(MmNumber::from("0.001")),
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    // Invalid amount should return an error
    let err = block_on(coin.validate_fee(validate_fee_args)).unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("Dex fee has invalid amount")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    // Invalid memo should return an error
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        fee_addr: &[],
        dex_fee: &DexFee::Standard(MmNumber::from("0.01")),
        min_block_number: 12000,
        uuid: &[2; 16],
    };
    let err = block_on(coin.validate_fee(validate_fee_args)).unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("Dex fee has invalid memo")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    // Confirmed before min block
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        fee_addr: &[],
        dex_fee: &DexFee::Standard(MmNumber::from("0.01")),
        min_block_number: 810617,
        uuid: &[1; 16],
    };
    let err = block_on(coin.validate_fee(validate_fee_args)).unwrap_err().into_inner();
    match err {
        ValidatePaymentError::WrongPaymentTx(err) => assert!(err.contains("confirmed before min block")),
        _ => panic!("Expected `WrongPaymentTx`: {:?}", err),
    }

    // Success validation
    let validate_fee_args = ValidateFeeArgs {
        fee_tx: &tx,
        expected_sender: &[],
        fee_addr: &[],
        dex_fee: &DexFee::Standard(MmNumber::from("0.01")),
        min_block_number: 12000,
        uuid: &[1; 16],
    };
    block_on(coin.validate_fee(validate_fee_args)).unwrap();
}
