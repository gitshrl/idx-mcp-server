use std::env;

/// Where the Parquet data lives. Local dir for dev, R2 for prod.
#[derive(Clone)]
pub enum DataBase {
    /// Local directory holding parquet, e.g. "./data".
    Local(String),
    /// Cloudflare R2 bucket read via `DuckDB` httpfs. `base` is like `r2://idx-data`.
    R2 {
        base: String,
        account_id: String,
        key_id: String,
        secret: String,
    },
}

// Manual Debug so a stray `{cfg:?}` can never leak the R2 credentials.
impl std::fmt::Debug for DataBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(dir) => f.debug_tuple("Local").field(dir).finish(),
            Self::R2 {
                base, account_id, ..
            } => f
                .debug_struct("R2")
                .field("base", base)
                .field("account_id", account_id)
                .field("key_id", &"<redacted>")
                .field("secret", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub sqlite_path: String,
    pub data_base: DataBase,
    /// Canonical externally-visible base URL (no trailing slash) used as the
    /// OAuth issuer, the RFC 9728 `resource`, and the token audience. Must be
    /// byte-identical to what the client uses. Defaults to `http://<bind_addr>`
    /// for local dev; set `IDX_PUBLIC_URL` to the public HTTPS origin in prod.
    pub public_url: String,
}

impl Config {
    /// Build config from environment.
    ///
    /// Data source resolution:
    /// - `IDX_DATA_DIR` set        -> read local Parquet from that dir.
    /// - R2_* all set              -> read from `r2://$R2_BUCKET` via httpfs.
    /// - neither                   -> default to local `./data`.
    pub fn from_env() -> Self {
        let bind_addr = env::var("IDX_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let sqlite_path = env::var("IDX_SQLITE").unwrap_or_else(|_| "./idx.sqlite".to_string());
        let public_url = env::var("IDX_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://{bind_addr}"))
            .trim_end_matches('/')
            .to_string();

        let data_base = if let Ok(dir) = env::var("IDX_DATA_DIR") {
            DataBase::Local(dir)
        } else if let (Ok(account_id), Ok(key_id), Ok(secret), Ok(bucket)) = (
            env::var("R2_ACCOUNT_ID"),
            env::var("R2_KEY_ID"),
            env::var("R2_SECRET"),
            env::var("R2_BUCKET"),
        ) {
            DataBase::R2 {
                base: format!("r2://{bucket}"),
                account_id,
                key_id,
                secret,
            }
        } else {
            DataBase::Local("./data".to_string())
        };

        Self {
            bind_addr,
            sqlite_path,
            data_base,
            public_url,
        }
    }
}
