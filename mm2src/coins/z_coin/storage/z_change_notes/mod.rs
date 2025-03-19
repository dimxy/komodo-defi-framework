use enum_derives::EnumFromStringify;

cfg_native!(
    pub(crate) mod sqlite;

    use db_common::async_sql_conn::{AsyncConnError, AsyncConnection};
    use futures::lock::Mutex;
    use std::sync::Arc;
);

cfg_wasm32!(
    pub(crate) mod wasm;

    use self::wasm::LockedNoteDbInner;
    use mm2_db::indexed_db::{DbTransactionError, InitDbError, SharedDb};
);

#[derive(Debug, Clone)]
pub(crate) struct LockedNote {
    pub(crate) hex: String,
    pub(crate) rseed: String,
}

/// A wrapper for the db connection to the change note cache database in native and browser.
#[derive(Clone)]
pub struct LockedNotesStorage {
    #[cfg(not(target_arch = "wasm32"))]
    pub db: Arc<Mutex<AsyncConnection>>,
    #[cfg(target_arch = "wasm32")]
    pub db: SharedDb<LockedNoteDbInner>,
    #[allow(unused)]
    address: String,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, EnumFromStringify)]
pub(crate) enum LockedNotesStorageError {
    #[cfg(not(target_arch = "wasm32"))]
    #[display(fmt = "Sqlite Error: {_0}")]
    #[from_stringify("AsyncConnError", "db_common::sqlite::rusqlite::Error")]
    SqliteError(String),
    #[cfg(target_arch = "wasm32")]
    #[display(fmt = "IndexedDb Error: {_0}")]
    #[from_stringify("InitDbError", "DbTransactionError")]
    IndexedDbError(String),
}

#[cfg(any(test, target_arch = "wasm32"))]
pub(super) mod change_notes_test {
    use crate::z_coin::storage::z_change_notes::LockedNotesStorage;
    use common::cross_test;

    use mm2_test_helpers::for_tests::mm_ctx_with_custom_db;

    common::cfg_wasm32! {
        use wasm_bindgen_test::*;
        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    }

    const MY_ADDRESS: &str = "my_address";

    cross_test!(test_insert_and_remove_note, {
        let ctx = mm_ctx_with_custom_db();
        let db = LockedNotesStorage::new(ctx, MY_ADDRESS.to_string()).await.unwrap();

        // insert note
        let result = db
            .insert_note(
                "0xcfec34a81e67e85aa1ce1a6666f92f9bc5606f0795be555bb3c9f9ac089aa4f7".to_string(),
                String::from("0x18b1acd8ceae8d71a2ae8b7e4a3e48ceb39dc237f0aa38c468425b88dc8d5f3e"),
            )
            .await;
        assert!(result.is_ok());

        // remove note
        let result = db
            .remove_note("0xcfec34a81e67e85aa1ce1a6666f92f9bc5606f0795be555bb3c9f9ac089aa4f7".to_owned())
            .await;
        assert!(result.is_ok());
        // get notes(should be empty)
        let notes = db.load_all_notes().await.unwrap();
        assert!(notes.is_empty());
    });

    cross_test!(test_load_all_notes, {
        let ctx = mm_ctx_with_custom_db();
        let db = LockedNotesStorage::new(ctx, MY_ADDRESS.to_string()).await.unwrap();

        let result = db
            .insert_note(
                "0xcfec34a81e67e85aa1ce1a6666f92f9bc5606f0795be555bb3c9f9ac089aa4f7".to_string(),
                String::from("0x18b1acd8ceae8d71a2ae8b7e4a3e48ceb39dc237f0aa38c468425b88dc8d5f3e"),
            )
            .await;
        assert!(result.is_ok());

        let notes = db.load_all_notes().await.unwrap();
        assert!(notes.len() == 1);
    });

    cross_test!(test_sum_changes, {
        let ctx = mm_ctx_with_custom_db();
        let db = LockedNotesStorage::new(ctx, MY_ADDRESS.to_string()).await.unwrap();

        let result = db
            .insert_note(
                "0xcfec34a81e67e85aa1ce1a6666f92f9bc5606f0795be555bb3c9f9ac089aa4f7".to_string(),
                String::from("0x18b1acd8ceae8d71a2ae8b7e4a3e48ceb39dc237f0aa38c468425b88dc8d5f3e"),
            )
            .await;
        assert!(result.is_ok());

        let result = db
            .insert_note(
                "0x8a7885455838d293cf2e5965dd34b7c11199c81fccfc71fbf33d606e24ce7f01".to_string(),
                String::from("0xab04f57873bfcd95b18e7495adfd055629eafe7a03f00e5c506c5d18e005e5c3"),
            )
            .await;
        assert!(result.is_ok());
    });
}
