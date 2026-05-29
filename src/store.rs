use std::sync::Mutex;

use anyhow::{Context, Result};
use duckdb::{Connection, ToSql};
use serde_json::Value;

use crate::config::{Config, DataBase};

/// Read-only analytic store over Parquet, backed by an embedded DuckDB.
///
/// Queries go to a single in-memory connection guarded by a `Mutex` (DuckDB
/// connections are `Send` but not `Sync`); this serializes queries, which is
/// fine for the current volume. Pool later if it becomes a bottleneck.
pub struct Store {
    conn: Mutex<Connection>,
    base: String,
}

impl Store {
    pub fn open(cfg: &Config) -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory duckdb")?;

        let base = match &cfg.data_base {
            DataBase::Local(dir) => dir.trim_end_matches('/').to_string(),
            DataBase::R2 {
                base,
                account_id,
                key_id,
                secret,
            } => {
                // httpfs autoloads on first use; the R2 secret resolves the
                // <account>.r2.cloudflarestorage.com endpoint for r2:// URLs.
                conn.execute_batch(&format!(
                    "CREATE OR REPLACE SECRET r2 (TYPE r2, KEY_ID '{key_id}', SECRET '{secret}', ACCOUNT_ID '{account_id}');"
                ))
                .context("create r2 secret")?;
                base.trim_end_matches('/').to_string()
            }
        };

        Ok(Self {
            conn: Mutex::new(conn),
            base,
        })
    }

    /// Base URI for Parquet paths, e.g. "./data" or "r2://idx-data".
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Glob for a collection's Parquet, e.g. `parquet_glob("yfdaily", "year=*/*.parquet")`.
    pub fn parquet_glob(&self, collection: &str, suffix: &str) -> String {
        format!("{}/{collection}/{suffix}", self.base)
    }

    /// Run `sql` and return each row as a JSON object (column name -> value).
    ///
    /// Uses DuckDB's `to_json(row)` so we don't hand-map every column type;
    /// the inner query may reference any columns/files.
    pub fn query_json(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Value>> {
        let wrapped = format!("SELECT to_json(t)::VARCHAR AS j FROM ({sql}) AS t");
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(&wrapped)
            .with_context(|| format!("prepare query: {sql}"))?;
        let rows = stmt
            .query_map(params, |row| row.get::<_, String>(0))
            .context("execute query")?;

        let mut out = Vec::new();
        for row in rows {
            let json = row.context("read row")?;
            out.push(serde_json::from_str(&json).context("parse row json")?);
        }
        Ok(out)
    }
}
