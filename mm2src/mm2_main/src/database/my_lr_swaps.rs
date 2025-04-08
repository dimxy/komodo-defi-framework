#![allow(deprecated)] // TODO: remove this once rusqlite is >= 0.29

#![allow(unused)] // TODO: remove

/// This module contains code to work with my_swaps table in MM2 SQLite DB
use crate::lp_swap::{MyRecentSwapsUuids, MySwapsFilter, SavedSwap, SavedSwapIo};
use common::log::debug;
use common::PagingOptions;
use db_common::sqlite::rusqlite::{Connection, Error as SqlError, Result as SqlResult, ToSql};
use db_common::sqlite::sql_builder::SqlBuilder;
use db_common::sqlite::{offset_by_uuid, query_single_row};
use mm2_core::mm_ctx::MmArc;
use std::convert::TryInto;
use uuid::{Error as UuidError, Uuid};

const INSERT_LR_SWAP: &str = r#"INSERT INTO my_swaps (
    my_coin,
    other_coin,
    uuid,
    started_at,
    swap_type,
    maker_volume,
    taker_volume,
    premium,
    dex_fee,
    dex_fee_burn,
    secret,
    secret_hash,
    secret_hash_algo,
    p2p_privkey,
    lock_duration,
    maker_coin_confs,
    maker_coin_nota,
    taker_coin_confs,
    taker_coin_nota,
    other_p2p_pub,
    swap_version
) VALUES (
    :my_coin,
    :other_coin,
    :uuid,
    :started_at,
    :swap_type,
    :maker_volume,
    :taker_volume,
    :premium,
    :dex_fee,
    :dex_fee_burn,
    :secret,
    :secret_hash,
    :secret_hash_algo,
    :p2p_privkey,
    :lock_duration,
    :maker_coin_confs,
    :maker_coin_nota,
    :taker_coin_confs,
    :taker_coin_nota,
    :other_p2p_pub,
    :swap_version
);"#;

pub(crate) fn insert_new_lr_swap(ctx: &MmArc, params: &[(&str, &dyn ToSql)]) -> SqlResult<()> {
    let conn = ctx.sqlite_connection();
    conn.execute(INSERT_LR_SWAP, params).map(|_| ())
}


/// The SQL query selecting upgraded swap data and send it to user through RPC API
/// It omits sensitive data (swap secret, p2p privkey, etc) for security reasons
pub(crate) const SELECT_LR_SWAP_FOR_RPC_BY_UUID: &str = r#"SELECT
    my_coin,
    other_coin,
    uuid,
    started_at,
    is_finished,
    events_json,
    maker_volume,
    taker_volume,
    premium,
    dex_fee,
    lock_duration,
    maker_coin_confs,
    maker_coin_nota,
    taker_coin_confs,
    taker_coin_nota,
    swap_version
FROM my_swaps
WHERE uuid = :uuid;
"#;

/// The SQL query selecting upgraded swap data required to re-initialize the swap e.g., on restart.
pub const SELECT_LR_SWAP_BY_UUID: &str = r#"SELECT
    my_coin,
    other_coin,
    uuid,
    started_at,
    secret,
    secret_hash,
    secret_hash_algo,
    events_json,
    maker_volume,
    taker_volume,
    premium,
    dex_fee,
    dex_fee_burn,
    lock_duration,
    maker_coin_confs,
    maker_coin_nota,
    taker_coin_confs,
    taker_coin_nota,
    p2p_privkey,
    other_p2p_pub,
    swap_version
FROM my_swaps
WHERE uuid = :uuid;
"#;

