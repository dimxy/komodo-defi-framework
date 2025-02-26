use std::path::PathBuf;

use crate::docker_tests::docker_tests_common::z_coin_from_spending_key;
use coins::{MarketCoinOps, RefundPaymentArgs, SendPaymentArgs, SwapOps, SwapTxTypeWithSecretHash};
use common::{block_on, now_sec, Future01CompatExt};

#[test]
fn zombie_coin_send_and_refund_maker_payment() {
    println!("creating coin");
    let (ctx, coin) = z_coin_from_spending_key("ZOMBIE", "secret-extended-key-main1q0k2ga2cqqqqpq8m8j6yl0say83cagrqp53zqz54w38ezs8ly9ly5ptamqwfpq85u87w0df4k8t2lwyde3n9v0gcr69nu4ryv60t0kfcsvkr8h83skwqex2nf0vr32794fmzk89cpmjptzc22lgu5wfhhp8lgf3f5vn2l3sge0udvxnm95k6dtxj2jwlfyccnum7nz297ecyhmd5ph526pxndww0rqq0qly84l635mec0x4yedf95hzn6kcgq8yxts26k98j9g32kjc8y83fe");
    println!("created coin");
    let time_lock = now_sec() - 3600;
    let taker_pub = coin.utxo_arc.priv_key_policy.activated_key_or_err().unwrap().public();
    let secret_hash = [0; 20];
    let pk_data = [1; 32];
    println!("got coin key");
    let args = SendPaymentArgs {
        time_lock_duration: 0,
        time_lock,
        other_pubkey: taker_pub,
        secret_hash: &secret_hash,
        amount: "0.01".parse().unwrap(),
        swap_contract_address: &None,
        swap_unique_data: &[],
        payment_instructions: &None,
        watcher_reward: None,
        wait_for_confirmation_until: 0,
    };
    println!("z_addr: {}", coin.my_z_address_encoded());
    let balance = block_on(coin.my_balance().compat()).unwrap();
    println!("balance: {balance:?}");
    println!("before send maker payement");
    let tx = block_on(coin.send_maker_payment(args)).unwrap();
    log!("swap tx {}", hex::encode(tx.tx_hash_as_bytes().0));
    println!("after send maker payement");

    let refund_args = RefundPaymentArgs {
        payment_tx: &tx.tx_hex(),
        time_lock,
        other_pubkey: taker_pub,
        tx_type_with_secret_hash: SwapTxTypeWithSecretHash::TakerOrMakerPayment {
            maker_secret_hash: &secret_hash,
        },
        swap_contract_address: &None,
        swap_unique_data: pk_data.as_slice(),
        watcher_reward: false,
    };
    println!("before send maker refund payment");
    let refund_tx = block_on(coin.send_maker_refunds_payment(refund_args)).unwrap();
    println!("after send maker refund payment");
    log!("refund tx {}", hex::encode(refund_tx.tx_hash_as_bytes().0));
}
