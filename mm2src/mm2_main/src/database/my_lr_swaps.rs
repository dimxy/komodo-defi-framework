#![allow(deprecated)] // TODO: remove this once rusqlite is >= 0.29
#![allow(unused)] // TODO: remove

/// This module contains code to work with my_swaps table in MM2 SQLite DB, to store and retrieve data for swaps with LR
use crate::lp_swap::{MyRecentSwapsUuids, MySwapsFilter, SavedSwap, SavedSwapIo};
use common::log::debug;
use common::PagingOptions;
use db_common::sqlite::rusqlite::{Connection, Error as SqlError, Result as SqlResult, ToSql};
use db_common::sqlite::sql_builder::SqlBuilder;
use db_common::sqlite::{offset_by_uuid, query_single_row};
use mm2_core::mm_ctx::MmArc;
use std::convert::TryInto;
use uuid::{Error as UuidError, Uuid};

/// Adds new fields required for aggregated swaps with liquidity routing
pub const LR_SWAP_MIGRATION: &[&str] = &[
    "ALTER TABLE my_swaps ADD COLUMN lr_swap_0 TEXT;",
    "ALTER TABLE my_swaps ADD COLUMN sell_buy_req TEXT;",
    "ALTER TABLE my_swaps ADD COLUMN lr_swap_1 TEXT;",
];

// NOTE: 'maker_volume' unused for now
// NOTE: 'taker_volume' used as source_volume
const INSERT_LR_SWAP: &str = r#"INSERT INTO my_swaps (
    my_coin,
    other_coin,
    uuid,
    started_at,
    swap_type,
    maker_volume,
    taker_volume,
    swap_version,
    lr_swap_0,
    sell_buy_req,
    lr_swap_1
) VALUES (
    :my_coin,
    :other_coin,
    :uuid,
    :started_at,
    :swap_type,
    :maker_volume,
    :taker_volume,
    :swap_version,
    :lr_swap_0,
    :sell_buy_req,
    :lr_swap_1
);"#;

pub(crate) fn insert_new_lr_swap(ctx: &MmArc, params: &[(&str, &dyn ToSql)]) -> SqlResult<()> {
    let conn = ctx.sqlite_connection();
    conn.execute(INSERT_LR_SWAP, params).map(|_| ())
}

/// The SQL query selecting swap with LR required to re-initialize the swap e.g., on restart.
/// TODO: 'maker_volume' is not set for now, add estimation for it
/// NOTE: 'taker_volume' used as source_volume
pub(crate) const SELECT_LR_SWAP_BY_UUID: &str = r#"SELECT
    my_coin,
    other_coin,
    uuid,
    started_at,
    is_finished,
    events_json,
    swap_version,
    taker_volume,
    maker_volume,
    lr_swap_0,
    sell_buy_req,
    lr_swap_1
FROM my_swaps
WHERE uuid = :uuid;
"#;
