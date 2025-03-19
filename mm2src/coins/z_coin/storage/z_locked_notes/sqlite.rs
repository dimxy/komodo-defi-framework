use super::{LockedNote, LockedNotesStorage, LockedNotesStorageError};
use db_common::sqlite::run_optimization_pragmas;
use db_common::sqlite::rusqlite::params;
use itertools::Itertools;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

const TABLE_NAME: &str = "locked_notes_cache";

impl LockedNotesStorage {
    #[cfg(not(test))]
    pub(crate) async fn new(ctx: MmArc, address: String) -> MmResult<Self, LockedNotesStorageError> {
        let db = ctx
            .async_sqlite_connection
            .get()
            .ok_or(MmError::new(LockedNotesStorageError::SqliteError(
                "Unable to get sqlite connection from ctx".into(),
            )))?
            .clone();

        let db_clone = db.clone();
        let db_lock = db_clone.lock().await;
        Ok(db_lock
            .call(move |conn| {
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

                Ok(Self { db, address })
            })
            .await?)
    }

    #[cfg(test)]
    pub(crate) async fn new(ctx: MmArc, address: String) -> MmResult<Self, LockedNotesStorageError> {
        use std::sync::Arc;

        use db_common::async_sql_conn::AsyncConnection;
        use futures::lock::Mutex;

        let test_conn = Arc::new(Mutex::new(AsyncConnection::open_in_memory().await.unwrap()));
        let db = ctx.async_sqlite_connection.get().cloned().unwrap_or(test_conn);
        let db_clone = db.clone();
        let db_lock = db_clone.lock().await;

        Ok(db_lock
            .call(move |conn| {
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
                Ok(Self { db, address })
            })
            .await?)
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

    pub(crate) async fn load_all_notes(&self) -> MmResult<Vec<LockedNote>, LockedNotesStorageError> {
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("SELECT * FROM {TABLE_NAME};"))?;
                let rows = stmt.query_map(params![], |row| {
                    Ok(LockedNote {
                        rseed: row.get(0)?,
                        hex: row.get(1)?,
                    })
                })?;

                Ok(rows.flatten().collect_vec())
            })
            .await?)
    }
}
