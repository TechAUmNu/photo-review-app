//! SQLite access. One process-wide connection behind a mutex; every write
//! happens in a transaction. The DB is the single source of truth for all
//! culling decisions — losing it loses review work, losing anything else
//! (cache, previews) only costs recompute time.

pub mod queries;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use rusqlite::Connection;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

const SCHEMA: &str = include_str!("schema.sql");

/// Open (or create) the library database. Idempotent; called once from Dart
/// at startup with a path inside the app-support directory.
pub fn init(db_path: &Path) -> Result<()> {
    if DB.get().is_some() {
        return Ok(());
    }
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db dir {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening db {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA).context("applying schema")?;
    migrate(&conn)?;
    let _ = DB.set(Mutex::new(conn));
    Ok(())
}

/// Additive migrations for databases created before a column existed.
fn migrate(conn: &Connection) -> Result<()> {
    let has_export_rate: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('bursts') WHERE name='export_rate'",
        [],
        |r| r.get(0),
    )?;
    if has_export_rate == 0 {
        conn.execute(
            "ALTER TABLE bursts ADD COLUMN export_rate REAL NOT NULL DEFAULT 1.0",
            [],
        )?;
    }
    Ok(())
}

/// In-memory DB for tests.
#[cfg(test)]
pub fn init_in_memory() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    migrate(&conn).unwrap();
    conn
}

pub fn conn() -> Result<MutexGuard<'static, Connection>> {
    let db = DB.get().context("database not initialised — call init first")?;
    Ok(db.lock().expect("db mutex poisoned"))
}
