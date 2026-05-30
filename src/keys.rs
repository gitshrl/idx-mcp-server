use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS api_keys (
  id         INTEGER PRIMARY KEY,
  key_hash   TEXT UNIQUE NOT NULL,
  label      TEXT,
  plan       TEXT NOT NULL DEFAULT 'free',
  active     INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS usage (
  id         INTEGER PRIMARY KEY,
  key_id     INTEGER NOT NULL REFERENCES api_keys(id),
  tool       TEXT NOT NULL,
  ts         TEXT NOT NULL,
  latency_ms INTEGER,
  rows       INTEGER
);
";

/// API-key + usage store backed by `SQLite`. Keys are stored only as SHA-256
/// hashes; the plaintext is shown once at creation and never persisted.
pub struct KeyStore {
    conn: Mutex<Connection>,
}

impl KeyStore {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open sqlite at {path}"))?;
        conn.execute_batch(SCHEMA).context("init schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Generate a new key, store its hash, and return the plaintext (shown once).
    pub fn add_key(&self, label: &str) -> Result<String> {
        let plaintext = generate_key();
        let hash = hash_key(&plaintext);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO api_keys (key_hash, label, created_at) VALUES (?1, ?2, datetime('now'))",
            params![hash, label],
        )
        .context("insert api key")?;
        Ok(plaintext)
    }

    /// Return the key id if the plaintext matches an active key.
    pub fn verify(&self, plaintext: &str) -> Result<Option<i64>> {
        let hash = hash_key(plaintext);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let id = conn
            .query_row(
                "SELECT id FROM api_keys WHERE key_hash = ?1 AND active = 1",
                params![hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .context("verify api key")?;
        Ok(id)
    }

    pub fn log_usage(&self, key_id: i64, tool: &str, latency_ms: i64, rows: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO usage (key_id, tool, ts, latency_ms, rows) VALUES (?1, ?2, datetime('now'), ?3, ?4)",
            params![key_id, tool, latency_ms, rows],
        )
        .context("log usage")?;
        Ok(())
    }
}

fn generate_key() -> String {
    let bytes: [u8; 24] = rand::random();
    let mut s = String::with_capacity(4 + bytes.len() * 2);
    s.push_str("idx_");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let mut s = String::with_capacity(64);
    for b in hasher.finalize() {
        let _ = write!(s, "{b:02x}");
    }
    s
}
