use super::{ChangeNote, ChangeNoteStorage, ChangeNoteStorageError};

use common::async_blocking;
use db_common::sqlite::run_optimization_pragmas;
use db_common::sqlite::rusqlite::params;
use db_common::sqlite::validate_table_name;
use itertools::Itertools;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

fn table_name(addr: &str) -> String { format!("{addr}_change_notes_cache") }

fn create_change_note_table(for_addr: &str) -> MmResult<String, ChangeNoteStorageError> {
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
        async_blocking(move || {
            let conn = ctx
                .sqlite_connection
                .get()
                .ok_or(MmError::new(ChangeNoteStorageError::InitializationError(
                    "Unable to get sqlite connection from ctx".into(),
                )))?;
            {
                let conn_lock = conn.lock().unwrap();
                run_optimization_pragmas(&conn_lock)?;
                conn_lock.execute(&create_change_note_table(&address)?, [])?;
            }

            Ok(Self {
                db: conn.clone(),
                address,
            })
        })
        .await
    }

    pub(crate) async fn insert_note(
        &self,
        hex: String,
        hex_bytes: Vec<u8>,
        change: u64,
    ) -> MmResult<(), ChangeNoteStorageError> {
        let db = self.db.clone();
        let table_name = table_name(&self.address);
        async_blocking(move || {
            let db = db.lock().unwrap();
            db.prepare(&format!(
                "INSERT OR REPLACE INTO {table_name} (hex, hex_bytes, change) VALUES (?, ?, ?)"
            ))?
            .execute(params![hex, hex_bytes, change])?;

            Ok(())
        })
        .await
    }

    pub(crate) async fn remove_note(&self, hash: Vec<u8>) -> MmResult<(), ChangeNoteStorageError> {
        let db = self.db.clone();
        let table_name = table_name(&self.address);
        async_blocking(move || {
            let db = db.lock().unwrap();
            db.prepare(&format!("DELETE FROM {table_name} WHERE hash=?1"))?
                .execute(params![hash])?;

            Ok(())
        })
        .await
    }

    pub(crate) async fn load_all_notes(&self) -> MmResult<Vec<ChangeNote>, ChangeNoteStorageError> {
        let db = self.db.clone();
        let table_name = table_name(&self.address);
        async_blocking(move || {
            let db = db.lock().unwrap();
            let mut stmt = db.prepare(&format!("SELECT * FROM {table_name};"))?;
            let rows = stmt.query_map(params![], |row| {
                Ok(ChangeNote {
                    hex: row.get(0)?,
                    hex_bytes: row.get(1)?,
                    change: row.get(2)?,
                })
            })?;

            Ok(rows.flatten().collect_vec())
        })
        .await
    }

    pub(crate) async fn sum_changes(&self) -> MmResult<u64, ChangeNoteStorageError> {
        let db = self.db.clone();
        let table_name = table_name(&self.address);
        async_blocking(move || {
            let db = db.lock().unwrap();
            let mut stmt = db.prepare(&format!("SELECT SUM(change) FROM {table_name};"))?;
            let sum: u64 = stmt.query_row(params![], |row| row.get(0))?;
            Ok(sum)
        })
        .await
    }
}
