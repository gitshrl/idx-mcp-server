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
";

/// A consumed authorization code's bound data, returned by `consume_auth_code`.
pub struct AuthCode {
    pub client_id: String,
    pub code_challenge: String,
    pub resource: Option<String>,
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

    pub fn log_usage(&self, key_id: i64, tool: &str, latency_ms: i64, rows: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute(
            "INSERT INTO usage (key_id, tool, ts, latency_ms, rows) VALUES (?1, ?2, datetime('now'), ?3, ?4)",
            params![key_id, tool, latency_ms, rows],
        )
        .context("log usage")?;
        Ok(())
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
