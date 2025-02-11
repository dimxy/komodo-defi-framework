use std::sync::Arc;

use db_common::async_sql_conn::{AsyncConnError, AsyncConnection};
use enum_derives::EnumFromStringify;
use futures::lock::Mutex;

cfg_native!(
    pub(crate) mod sqlite;
);
cfg_wasm32!(
    pub(crate) mod wasm;
);

#[derive(Debug, Clone)]
pub(crate) struct ChangeNote {
    pub(crate) hex_bytes: Vec<u8>,
    pub(crate) hex: String,
    pub(crate) change: u64,
}

/// A wrapper for the db connection to the change note cache database in native and browser.
#[derive(Clone)]
pub(crate) struct ChangeNoteStorage {
    #[cfg(not(target_arch = "wasm32"))]
    pub db: Arc<Mutex<AsyncConnection>>,
    //     #[cfg(target_arch = "wasm32")]
    //     pub db: SharedDb<BlockDbInner>,
    address: String,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, EnumFromStringify)]
pub(crate) enum ChangeNoteStorageError {
    #[cfg(not(target_arch = "wasm32"))]
    #[display(fmt = "Sqlite Error: {_0}")]
    #[from_stringify("AsyncConnError", "db_common::sqlite::rusqlite::Error")]
    SqliteError(String),
    #[display(fmt = "Can't init from the storage: {_0}")]
    InitializationError(String),
}
