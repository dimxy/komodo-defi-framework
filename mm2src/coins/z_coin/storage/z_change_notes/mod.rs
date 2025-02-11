use enum_derives::EnumFromStringify;

cfg_native!(
    pub(crate) mod sqlite;

    use db_common::sqlite::rusqlite::Connection;
    use std::sync::{Arc, Mutex};
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
    pub db: Arc<Mutex<Connection>>,
    //     #[cfg(target_arch = "wasm32")]
    //     pub db: SharedDb<BlockDbInner>,
    address: String,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, EnumFromStringify)]
pub(crate) enum ChangeNoteStorageError {
    #[cfg(not(target_arch = "wasm32"))]
    #[display(fmt = "Sqlite Error: {_0}")]
    #[from_stringify("db_common::sqlite::rusqlite::Error")]
    SqliteError(String),
    #[display(fmt = "Can't init from the storage: {_0}")]
    InitializationError(String),
}
