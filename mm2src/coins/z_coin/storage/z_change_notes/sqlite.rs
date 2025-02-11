use super::{ChangeNote, ChangeNoteStorage, ChangeNoteStorageError};

use db_common::async_sql_conn::AsyncConnError;
use db_common::sqlite::rusqlite::params;
use db_common::sqlite::{run_optimization_pragmas, validate_table_name};
use itertools::Itertools;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

fn table_name(addr: &str) -> String { format!("{addr}_change_notes_cache") }

fn create_change_note_table(for_addr: &str) -> Result<String, AsyncConnError> {
    let table_name = table_name(for_addr);
    validate_table_name(&table_name)?;
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table_name} (
            hex TEXT NOT NULL UNIQUE,
            hex_bytes BLOB NOT NULL UNIQUE,
            change INTEGER NOT NULL,
        );"
    );

    Ok(sql)
}

impl ChangeNoteStorage {
    pub(crate) async fn new(ctx: MmArc, address: String) -> MmResult<Self, ChangeNoteStorageError> {
        let db = ctx
            .async_sqlite_connection
            .get()
            .ok_or(MmError::new(ChangeNoteStorageError::InitializationError(
                "Unable to get sqlite connection from ctx".into(),
            )))?;
        {
            let address = address.clone();
            let db_clone = db.lock().await;
            db_clone
                .call(move |conn| {
                    run_optimization_pragmas(conn)?;
                    conn.execute(&create_change_note_table(&address)?, [])?;
                    Ok(())
                })
                .await?;
        }

        Ok(Self {
            db: db.clone(),
            address,
        })
    }

    pub(crate) async fn insert_note(
        &self,
        hex: String,
        hex_bytes: Vec<u8>,
        change: u64,
    ) -> MmResult<(), ChangeNoteStorageError> {
        let table_name = table_name(&self.address);
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                conn.prepare(&format!(
                    "INSERT OR REPLACE INTO {table_name} (hex, hex_bytes, change) VALUES (?, ?, ?)"
                ))?
                .execute(params![hex, hex_bytes, change])?;

                Ok(())
            })
            .await?)
    }

    pub(crate) async fn remove_note(&self, hash: Vec<u8>) -> MmResult<(), ChangeNoteStorageError> {
        let table_name = table_name(&self.address);
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                conn.prepare(&format!("DELETE FROM {table_name} WHERE hash=?1"))?
                    .execute(params![hash])?;
                Ok(())
            })
            .await?)
    }

    pub(crate) async fn load_all_notes(&self) -> MmResult<Vec<ChangeNote>, ChangeNoteStorageError> {
        let table_name = table_name(&self.address);
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("SELECT * FROM {table_name};"))?;
                let rows = stmt.query_map(params![], |row| {
                    Ok(ChangeNote {
                        hex: row.get(0)?,
                        hex_bytes: row.get(1)?,
                        change: row.get(2)?,
                    })
                })?;

                Ok(rows.flatten().collect_vec())
            })
            .await?)
    }

    pub(crate) async fn sum_changes(&self) -> MmResult<u64, ChangeNoteStorageError> {
        let table_name = table_name(&self.address);
        let db = self.db.lock().await;
        Ok(db
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("SELECT SUM(change) FROM {table_name};"))?;
                let sum: u64 = stmt.query_row(params![], |row| row.get(0))?;
                Ok(sum)
            })
            .await?)
    }
}
