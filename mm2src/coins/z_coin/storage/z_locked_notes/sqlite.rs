use super::{LockedNote, LockedNotesStorage, LockedNotesStorageError};
use db_common::async_sql_conn::{AsyncConnError, AsyncConnection};
use db_common::sqlite::run_optimization_pragmas;
use db_common::sqlite::rusqlite::params;
use futures::lock::Mutex;
use itertools::Itertools;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use std::sync::Arc;

const TABLE_NAME: &str = "locked_notes_cache";

async fn create_table(conn: Arc<Mutex<AsyncConnection>>) -> Result<(), AsyncConnError> {
    let conn = conn.lock().await;
    conn.call(move |conn| {
        run_optimization_pragmas(conn)?;
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {TABLE_NAME} (
                        rseed VARCHAR NOT NULL UNIQUE,
                        hex TEXT NOT NULL
                    )"
            ),
            [],
        )?;

        Ok(())
    })
    .await
}

impl LockedNotesStorage {
    #[cfg(not(any(test, feature = "run-docker-tests")))]
    pub(crate) async fn new(ctx: &MmArc, address: String) -> MmResult<Self, LockedNotesStorageError> {
        let path = ctx.wallet_dir().join(format!("{}_locked_notes_cache.db", address));
        let db = AsyncConnection::open(path)
            .await
            .map_to_mm(|err| LockedNotesStorageError::SqliteError(err.to_string()))?;
        let db = Arc::new(Mutex::new(db));

        create_table(db.clone()).await?;

        Ok(Self { db, address })
    }

    #[cfg(any(test, feature = "run-docker-tests"))]
    pub(crate) async fn new(ctx: &MmArc, address: String) -> MmResult<Self, LockedNotesStorageError> {
        #[cfg(feature = "run-docker-tests")]
        let db = {
            let path = ctx.wallet_dir().join(format!("{}_locked_notes_cache.db", address));
            mm2_io::fs::create_parents_async(&path)
                .await
                .map_err(|err| LockedNotesStorageError::SqliteError(err.to_string()))?;
            Arc::new(Mutex::new(
                AsyncConnection::open(path)
                    .await
                    .map_to_mm(|err| LockedNotesStorageError::SqliteError(err.to_string()))?,
            ))
        };
        #[cfg(all(test, not(feature = "run-docker-tests")))]
        let db = {
            let test_conn = Arc::new(Mutex::new(AsyncConnection::open_in_memory().await.unwrap()));
            ctx.async_sqlite_connection.get().cloned().unwrap_or(test_conn)
        };

        create_table(db.clone()).await?;

        Ok(Self { db, address })
    }

    pub(crate) async fn insert_note(&self, hex: String, rseed: String) -> MmResult<(), LockedNotesStorageError> {
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                conn.prepare(&format!(
                    "INSERT OR REPLACE INTO {TABLE_NAME} (rseed, hex) VALUES (?, ?)"
                ))?
                .execute(params![rseed, hex])?;

                Ok(())
            })
            .await?)
    }

    pub(crate) async fn remove_note(&self, hex: String) -> MmResult<(), LockedNotesStorageError> {
        common::log::info!("unlocking {hex} notes");
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                conn.prepare(&format!("DELETE FROM {TABLE_NAME} WHERE hex=?"))?
                    .execute(params![hex])?;
                Ok(())
            })
            .await?)
    }

    pub async fn load_all_notes(&self) -> MmResult<Vec<LockedNote>, LockedNotesStorageError> {
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("SELECT rseed, hex FROM {TABLE_NAME};"))?;
                let rows = stmt.query_map(params![], |row| Ok(LockedNote { rseed: row.get(0)?, txid: row.get(1)? }))?;

                Ok(rows.flatten().collect_vec())
            })
            .await?)
    }
}
