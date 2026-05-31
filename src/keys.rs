use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

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
CREATE TABLE IF NOT EXISTS oauth_clients (
  client_id     TEXT PRIMARY KEY,
  redirect_uris TEXT NOT NULL,   -- JSON array
  created_at    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oauth_codes (
  code_hash      TEXT PRIMARY KEY,
  client_id      TEXT NOT NULL,
  code_challenge TEXT NOT NULL,  -- PKCE S256, base64url-nopad
  redirect_uri   TEXT NOT NULL,
  resource       TEXT,           -- requested audience (RFC 8707)
  expires_at     TEXT NOT NULL,
  used           INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS oauth_tokens (
  token_hash TEXT PRIMARY KEY,
  client_id  TEXT,
  audience   TEXT NOT NULL,
  scope      TEXT,
  expires_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS filings (
  url        TEXT PRIMARY KEY,
  bytes      INTEGER NOT NULL,
  chars      INTEGER NOT NULL,
  truncated  INTEGER NOT NULL,
  text       TEXT NOT NULL,
  fetched_at TEXT NOT NULL DEFAULT (datetime('now'))
);
";

/// A consumed authorization code's bound data, returned by `consume_auth_code`.
pub struct AuthCode {
    pub client_id: String,
    pub code_challenge: String,
    pub resource: Option<String>,
}

/// A persisted filing — the extracted text plus the metadata `get_filing`
/// returns. Stored so a fetched PDF survives a restart (the L2 cache).
pub struct CachedFiling {
    pub url: String,
    pub bytes: usize,
    pub chars: usize,
    pub truncated: bool,
    pub text: String,
}

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

    /// Insert a batch of usage events in one transaction (called by the single
    /// background writer, never per-request — see `UsageLogger`).
    pub fn log_usage_batch(&self, batch: &[UsageEvent]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction().context("begin usage tx")?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO usage (key_id, tool, ts, latency_ms, rows) VALUES (?1, ?2, datetime('now'), ?3, 0)",
                )
                .context("prepare usage insert")?;
            for e in batch {
                stmt.execute(params![e.key_id, e.tool, e.latency_ms])
                    .context("insert usage")?;
            }
        }
        tx.commit().context("commit usage")?;
        Ok(())
    }

    /// Delete usage rows older than `keep_days` so the table can't grow without
    /// bound on a long-lived server. Returns the number of rows removed.
    pub fn prune_usage(&self, keep_days: u32) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let n = conn
            .execute(
                "DELETE FROM usage WHERE ts < datetime('now', ?1)",
                params![format!("-{keep_days} days")],
            )
            .context("prune usage")?;
        Ok(n)
    }

    // ---- OAuth authorization-server store (opaque, SHA-256-hashed) ----

    /// Register a public OAuth client (DCR); returns the new `client_id`.
    pub fn register_client(&self, redirect_uris: &[String]) -> Result<String> {
        let client_id = format!("idxc_{}", random_hex(12));
        let uris = serde_json::to_string(redirect_uris).context("encode redirect_uris")?;
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO oauth_clients (client_id, redirect_uris, created_at) VALUES (?1, ?2, datetime('now'))",
            params![client_id, uris],
        )
        .context("insert oauth client")?;
        Ok(client_id)
    }

    /// The registered redirect URIs for a client, if it exists.
    pub fn client_redirect_uris(&self, client_id: &str) -> Result<Option<Vec<String>>> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let row: Option<String> = conn
            .query_row(
                "SELECT redirect_uris FROM oauth_clients WHERE client_id = ?1",
                params![client_id],
                |r| r.get(0),
            )
            .optional()
            .context("load oauth client")?;
        match row {
            Some(s) => Ok(Some(
                serde_json::from_str(&s).context("decode redirect_uris")?,
            )),
            None => Ok(None),
        }
    }

    /// Issue a single-use authorization code (60s TTL); returns the plaintext.
    pub fn create_auth_code(
        &self,
        client_id: &str,
        code_challenge: &str,
        redirect_uri: &str,
        resource: Option<&str>,
    ) -> Result<String> {
        let code = random_hex(32);
        let hash = hash_key(&code);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO oauth_codes (code_hash, client_id, code_challenge, redirect_uri, resource, expires_at, used)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now', '+60 seconds'), 0)",
            params![hash, client_id, code_challenge, redirect_uri, resource],
        )
        .context("insert oauth code")?;
        Ok(code)
    }

    /// Atomically consume a valid, unused, unexpired code matching `redirect_uri`.
    /// Marks it used in the same statement so a code can never be replayed.
    pub fn consume_auth_code(&self, code: &str, redirect_uri: &str) -> Result<Option<AuthCode>> {
        let hash = hash_key(code);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.query_row(
            "UPDATE oauth_codes SET used = 1
             WHERE code_hash = ?1 AND used = 0 AND redirect_uri = ?2 AND expires_at > datetime('now')
             RETURNING client_id, code_challenge, resource",
            params![hash, redirect_uri],
            |r| {
                Ok(AuthCode {
                    client_id: r.get(0)?,
                    code_challenge: r.get(1)?,
                    resource: r.get(2)?,
                })
            },
        )
        .optional()
        .context("consume oauth code")
    }

    /// Issue an opaque access token bound to `audience`; returns the plaintext.
    pub fn issue_token(
        &self,
        client_id: &str,
        audience: &str,
        scope: &str,
        ttl_secs: i64,
    ) -> Result<String> {
        let token = format!("idxoat_{}", random_hex(32));
        let hash = hash_key(&token);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO oauth_tokens (token_hash, client_id, audience, scope, expires_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now', ?5))",
            params![
                hash,
                client_id,
                audience,
                scope,
                format!("+{ttl_secs} seconds")
            ],
        )
        .context("insert oauth token")?;
        Ok(token)
    }

    /// True if `token` is an unexpired access token bound to `expected_audience`.
    pub fn verify_oauth(&self, token: &str, expected_audience: &str) -> Result<bool> {
        let hash = hash_key(token);
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let hit: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM oauth_tokens WHERE token_hash = ?1 AND audience = ?2 AND expires_at > datetime('now')",
                params![hash, expected_audience],
                |r| r.get(0),
            )
            .optional()
            .context("verify oauth token")?;
        Ok(hit.is_some())
    }

    // ---- On-demand filing cache (survives restart; L2 behind the in-memory L1) ----

    /// The cached filing for `url`, if it was fetched before.
    pub fn filing_get(&self, url: &str) -> Result<Option<CachedFiling>> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let row = conn
            .query_row(
                "SELECT bytes, chars, truncated, text FROM filings WHERE url = ?1",
                params![url],
                |r| {
                    Ok((
                        usize::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                        usize::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                        r.get::<_, i64>(2)? != 0,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .context("read cached filing")?;
        Ok(row.map(|(bytes, chars, truncated, text)| CachedFiling {
            url: url.to_string(),
            bytes,
            chars,
            truncated,
            text,
        }))
    }

    /// Persist a fetched filing so it survives a restart (idempotent on `url`).
    pub fn filing_put(&self, c: &CachedFiling) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT OR REPLACE INTO filings (url, bytes, chars, truncated, text, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                c.url,
                i64::try_from(c.bytes).unwrap_or(i64::MAX),
                i64::try_from(c.chars).unwrap_or(i64::MAX),
                i64::from(c.truncated),
                c.text
            ],
        )
        .context("write cached filing")?;
        Ok(())
    }
}

/// A single tool-call usage record, queued to the background writer.
pub struct UsageEvent {
    pub key_id: i64,
    pub tool: String,
    pub latency_ms: i64,
}

/// Bounded, off-request usage logger. One background task drains a bounded
/// channel, batch-writes to `SQLite`, and periodically prunes old rows — so the
/// request path never spawns an unbounded task or contends on the DB, and the
/// `usage` table can't grow without limit. Telemetry is best-effort: a full
/// queue drops the event rather than backpressuring the request.
#[derive(Clone)]
pub struct UsageLogger {
    tx: mpsc::Sender<UsageEvent>,
}

impl UsageLogger {
    /// Spawn the background writer over `keys`; returns a cloneable handle.
    #[must_use]
    pub fn spawn(keys: Arc<KeyStore>) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageEvent>(4096);
        tokio::spawn(async move {
            let mut buf: Vec<UsageEvent> = Vec::with_capacity(128);
            // `Duration::from_hours` is unstable on the pinned 1.96 toolchain.
            #[allow(clippy::duration_suboptimal_units)]
            let mut prune = tokio::time::interval(Duration::from_secs(6 * 3600));
            prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    n = rx.recv_many(&mut buf, 128) => {
                        if n == 0 {
                            break; // all senders dropped
                        }
                        let batch = std::mem::take(&mut buf);
                        let k = keys.clone();
                        match tokio::task::spawn_blocking(move || k.log_usage_batch(&batch)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => tracing::warn!(error = %e, "usage batch write failed"),
                            Err(e) => tracing::warn!(error = %e, "usage writer panicked"),
                        }
                    }
                    _ = prune.tick() => {
                        let k = keys.clone();
                        if let Ok(Ok(removed)) =
                            tokio::task::spawn_blocking(move || k.prune_usage(90)).await
                            && removed > 0
                        {
                            tracing::info!(removed, "pruned old usage rows");
                        }
                    }
                }
            }
        });
        Self { tx }
    }

    /// Record a usage event without blocking the request path; drops it if the
    /// queue is full (best-effort telemetry).
    pub fn record(&self, event: UsageEvent) {
        let _ = self.tx.try_send(event);
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

/// `n` random bytes as lowercase hex (for opaque client ids, codes, tokens).
fn random_hex(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for _ in 0..n {
        let _ = write!(s, "{:02x}", rand::random::<u8>());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filing_cache_roundtrip() {
        let store = KeyStore::open(":memory:").expect("open store");
        let url = "https://www.idx.co.id/x.pdf";
        assert!(store.filing_get(url).expect("miss").is_none());
        store
            .filing_put(&CachedFiling {
                url: url.to_string(),
                bytes: 1234,
                chars: 56,
                truncated: true,
                text: "hello".to_string(),
            })
            .expect("put");
        let got = store.filing_get(url).expect("get").expect("present");
        assert_eq!(got.url, url);
        assert_eq!(got.bytes, 1234);
        assert_eq!(got.chars, 56);
        assert!(got.truncated);
        assert_eq!(got.text, "hello");
    }
}
