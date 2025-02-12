use enum_derives::EnumFromStringify;

cfg_native!(
    pub(crate) mod sqlite;

    use db_common::async_sql_conn::{AsyncConnError, AsyncConnection};
    use futures::lock::Mutex;
    use std::sync::Arc;
);

cfg_wasm32!(
    pub(crate) mod wasm;

    use self::wasm::ChangeNoteDbInner;
    use mm2_db::indexed_db::{DbTransactionError, InitDbError, SharedDb};
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
    #[cfg(target_arch = "wasm32")]
    pub db: SharedDb<ChangeNoteDbInner>,
    address: String,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, EnumFromStringify)]
pub(crate) enum ChangeNoteStorageError {
    #[cfg(not(target_arch = "wasm32"))]
    #[display(fmt = "Sqlite Error: {_0}")]
    #[from_stringify("AsyncConnError", "db_common::sqlite::rusqlite::Error")]
    SqliteError(String),
    #[cfg(target_arch = "wasm32")]
    #[display(fmt = "IndexedDb Error: {_0}")]
    #[from_stringify("InitDbError", "DbTransactionError")]
    IndexedDbError(String),
}
