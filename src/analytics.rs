//! The analytics engine: a single loaded, locked, read-only `DuckDB` serving
//! database that every tool queries.
//!
//! At boot (and on manual refresh) a trusted read-write connection materializes
//! each Parquet dataset into a table and builds the analytical views, then the
//! file is reopened through a **locked read-only** connection with external
//! access disabled. `run_query` runs untrusted SQL against that connection;
//! safety rests on three layers:
//!   1. the connection (read-only, `enable_external_access=false`,
//!      `lock_configuration=true`) — no writes, no file/network access, period;
//!   2. a validator built on `DuckDB`'s own parser (`json_serialize_sql`) — one
//!      SELECT statement, tables in the allowlist, no file-reading functions;
//!   3. an external timeout that interrupts a runaway query.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use duckdb::{AccessMode, Config, Connection, InterruptHandle, ToSql};
use serde_json::Value;

use crate::catalog::{self, Kind};
use crate::config::{Config as AppConfig, DataBase};

const MAX_ROWS: usize = 5_000;
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_MEMORY: &str = "2GB";
/// Read-only connections over one shared locked instance, round-robined so
/// queries run concurrently instead of serializing on a single connection.
const SERVING_CONNECTIONS: usize = 8;

/// Monotonic id so each engine instance gets its own serving directory.
static INSTANCE: AtomicU64 = AtomicU64::new(0);

/// Result of a `run_query` call.
pub struct QueryOutput {
    pub rows: Vec<Value>,
    /// True if the row cap was hit and results were cut.
    pub truncated: bool,
}

/// How to (re)build the serving database from Parquet.
struct Source {
    /// Data root, e.g. `./data` or `r2://idx-data`.
    base: String,
    /// `CREATE SECRET` DDL replayed into each loader, for R2.
    secret_sql: Option<String>,
}

impl Source {
    fn glob(&self, name: &str, kind: Kind) -> String {
        match kind {
            Kind::TimeSeries => format!("{}/{name}/date=*/*.parquet", self.base),
            Kind::Snapshot => format!("{}/{name}/latest.parquet", self.base),
        }
    }
}

/// One read-only connection to the shared serving instance, with its own
/// interrupt handle for the timeout path.
#[derive(Clone)]
struct ConnSlot {
    conn: Arc<Mutex<Connection>>,
    interrupt: Arc<InterruptHandle>,
}

/// A built serving database: a pool of read-only connections over one shared,
/// locked instance (one buffer pool, one memory budget), round-robined so
/// queries run concurrently.
struct Serving {
    conns: Vec<ConnSlot>,
    next: AtomicUsize,
    path: PathBuf,
    tables: Vec<String>,
    views: Vec<String>,
}

impl Serving {
    /// Round-robin a connection. Distinct picks run concurrently; a collision
    /// merely serializes on that one connection's mutex.
    fn pick(&self) -> ConnSlot {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.conns.len();
        self.conns[i].clone()
    }
}

/// The analytics engine. One per process; cloneable handle via `Arc`.
pub struct Analytics {
    serving: RwLock<Arc<Serving>>,
    source: Source,
    dir: PathBuf,
    version: AtomicU64,
}

impl Analytics {
    /// Build the serving database from the configured data source and open it
    /// read-only. Fails fast if no datasets could be loaded.
    pub fn new(cfg: &AppConfig) -> Result<Self> {
        let source = match &cfg.data_base {
            DataBase::Local(dir) => Source {
                base: dir.trim_end_matches('/').to_string(),
                secret_sql: None,
            },
            DataBase::R2 {
                base,
                account_id,
                key_id,
                secret,
            } => Source {
                base: base.trim_end_matches('/').to_string(),
                secret_sql: Some(format!(
                    "CREATE OR REPLACE SECRET r2 (TYPE r2, KEY_ID '{key_id}', SECRET '{secret}', ACCOUNT_ID '{account_id}');"
                )),
            },
        };

        let root = std::env::var("IDX_SERVING_DIR").map_or_else(
            |_| std::env::temp_dir().join("idx-mcp-serving"),
            PathBuf::from,
        );
        // Unique per engine instance so concurrent instances never share files.
        let dir = root.join(format!(
            "{}-{}",
            std::process::id(),
            INSTANCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create serving dir {}", dir.display()))?;
        clear_serving_files(&dir);

        // Version 0 is the boot build; rebuilds start at 1 so they never collide
        // with (and delete) the file a still-open connection is reading.
        let serving = build_and_open(&source, &dir, 0)?;
        Ok(Self {
            serving: RwLock::new(Arc::new(serving)),
            source,
            dir,
            version: AtomicU64::new(1),
        })
    }

    /// Names of the loaded base tables, for boot logging.
    #[must_use]
    pub fn loaded_tables(&self) -> Vec<String> {
        self.serving
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .tables
            .clone()
    }

    /// Names of the created analytical views.
    #[must_use]
    pub fn loaded_views(&self) -> Vec<String> {
        self.serving
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .views
            .clone()
    }

    /// Rebuild the serving database and atomically swap it in. Manual: invoked
    /// at boot and on `SIGHUP` / `idx-mcp refresh`.
    pub fn rebuild(&self) -> Result<()> {
        let version = self.version.fetch_add(1, Ordering::SeqCst);
        let serving = build_and_open(&self.source, &self.dir, version)?;
        let old = {
            let mut guard = self.serving.write().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *guard, Arc::new(serving))
        };
        // On Linux unlinking an open file is safe; any in-flight query on the
        // old connection keeps reading until it finishes.
        if !old.path.as_os_str().is_empty()
            && let Err(e) = std::fs::remove_file(&old.path)
        {
            tracing::warn!(path = %old.path.display(), error = %e, "failed to remove old serving file");
        }
        Ok(())
    }

    /// Run untrusted SQL: validate, execute with a row cap and timeout.
    pub async fn run_query(&self, sql: &str, limit: Option<usize>) -> Result<QueryOutput> {
        let sql = sql.trim().trim_end_matches(';').trim().to_string();
        if sql.is_empty() {
            bail!("empty query");
        }
        let cap = limit.map_or(MAX_ROWS, |n| n.min(MAX_ROWS)).max(1);
        let serving = self.current();
        let ConnSlot { conn, interrupt } = serving.pick();

        let task = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            validate(&conn, &sql)?;
            let wrapped = format!(
                "SELECT to_json(t)::VARCHAR AS j FROM ({sql}) AS t LIMIT {}",
                cap + 1
            );
            collect_json(&conn, &wrapped, &[])
        });

        let Ok(joined) = tokio::time::timeout(QUERY_TIMEOUT, task).await else {
            interrupt.interrupt();
            bail!("query exceeded the {}s time limit", QUERY_TIMEOUT.as_secs());
        };
        let mut rows = joined.context("query task failed")??;
        let truncated = rows.len() > cap;
        rows.truncate(cap);
        Ok(QueryOutput { rows, truncated })
    }

    /// Run a trusted, parameterized query (the typed shortcut tools). The SQL
    /// is server-authored; `params` are bound, never interpolated.
    pub async fn query_json(&self, sql: String, params: Vec<String>) -> Result<Vec<Value>> {
        let serving = self.current();
        let ConnSlot { conn, interrupt } = serving.pick();

        let task = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            let wrapped = format!("SELECT to_json(t)::VARCHAR AS j FROM ({sql}) AS t");
            let bound: Vec<&dyn ToSql> = params.iter().map(|s| s as &dyn ToSql).collect();
            collect_json(&conn, &wrapped, &bound)
        });

        let Ok(joined) = tokio::time::timeout(QUERY_TIMEOUT, task).await else {
            interrupt.interrupt();
            bail!("query exceeded the {}s time limit", QUERY_TIMEOUT.as_secs());
        };
        joined.context("query task failed")?
    }

    /// Schema description: catalog docs merged with live columns from the
    /// serving database. `only` restricts to one table/view.
    pub async fn describe(&self, only: Option<String>) -> Result<Value> {
        let serving = self.current();
        let conn = serving.pick().conn;
        let mut relations: Vec<(String, &'static str)> = Vec::new();
        for t in &serving.tables {
            relations.push((t.clone(), "table"));
        }
        for v in &serving.views {
            relations.push((v.clone(), "view"));
        }

        let task = tokio::task::spawn_blocking(move || -> Result<Value> {
            let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            let mut out = Vec::new();
            for (name, relkind) in relations {
                if only
                    .as_deref()
                    .is_some_and(|want| !want.eq_ignore_ascii_case(&name))
                {
                    continue;
                }
                let cols = describe_columns(&conn, &name)?;
                out.push(serde_json::json!({
                    "name": name,
                    "relation": relkind,
                    "description": catalog::doc_for(&name).unwrap_or(""),
                    "columns": cols,
                }));
            }
            Ok(Value::Array(out))
        });
        task.await.context("describe task failed")?
    }

    fn current(&self) -> Arc<Serving> {
        self.serving
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Build a fresh serving file at version `version` and open it read-only.
fn build_and_open(source: &Source, dir: &Path, version: u64) -> Result<Serving> {
    let path = dir.join(format!("serving-{version}.duckdb"));
    let _ = std::fs::remove_file(&path);
    let (tables, views) = build_serving(source, &path)?;
    if tables.is_empty() {
        bail!("no datasets could be loaded from {}", source.base);
    }
    open_serving_ro(&path, tables, views)
}

/// Build the serving file with a trusted read-write connection (external access
/// on, to read Parquet). Returns the loaded table names and created view names.
fn build_serving(source: &Source, dst: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let conn =
        Connection::open(dst).with_context(|| format!("open serving db {}", dst.display()))?;
    conn.execute_batch("SET threads TO 4;")
        .context("set loader threads")?;
    if let Some(sql) = &source.secret_sql {
        conn.execute_batch(sql).context("create r2 secret")?;
    }

    let mut tables = Vec::new();
    for ds in catalog::DATASETS {
        let glob = source.glob(ds.name, ds.kind);
        let hive = if ds.kind == Kind::TimeSeries {
            ", hive_partitioning=true"
        } else {
            ""
        };
        let create = format!(
            "CREATE OR REPLACE TABLE \"{}\" AS SELECT * FROM read_parquet('{glob}'{hive})",
            ds.name
        );
        match conn.execute_batch(&create) {
            Ok(()) => tables.push(ds.name.to_string()),
            Err(e) => {
                tracing::warn!(dataset = ds.name, error = %e, "skipping dataset (load failed)");
            }
        }
    }

    let views = create_views(&conn, &tables);

    conn.close().map_err(|(_, e)| e).context("close loader")?;
    Ok((tables, views))
}

/// Create the analytical views whose required tables are all present.
fn create_views(conn: &Connection, tables: &[String]) -> Vec<String> {
    let mut created = Vec::new();
    for view in catalog::VIEWS {
        if !view.requires.iter().all(|r| tables.iter().any(|t| t == r)) {
            tracing::warn!(view = view.name, "skipping view (missing inputs)");
            continue;
        }
        let Some(sql) = view_sql(view.name) else {
            tracing::warn!(view = view.name, "no SQL defined for view; skipping");
            continue;
        };
        match conn.execute_batch(sql) {
            Ok(()) => created.push(view.name.to_string()),
            Err(e) => tracing::warn!(view = view.name, error = %e, "skipping view (create failed)"),
        }
    }
    created
}

fn view_sql(name: &str) -> Option<&'static str> {
    match name {
        "latest" => Some(LATEST_VIEW),
        "returns" => Some(RETURNS_VIEW),
        "broker_net" => Some(BROKER_NET_VIEW),
        _ => None,
    }
}

const LATEST_VIEW: &str = "\
CREATE VIEW latest AS
WITH p AS (
  SELECT ticker, close, volume, date AS price_date
  FROM prices QUALIFY row_number() OVER (PARTITION BY ticker ORDER BY date DESC) = 1
),
i AS (
  SELECT ticker, rsi_14, sma_50, sma_200
  FROM indicators QUALIFY row_number() OVER (PARTITION BY ticker ORDER BY date DESC) = 1
),
f AS (
  SELECT ticker, market_cap, enterprise_value, shares_outstanding, free_float_pct
  FROM fundamentals QUALIFY row_number() OVER (PARTITION BY ticker ORDER BY date DESC) = 1
)
SELECT
  c.ticker, c.company_name, c.sector, c.sub_sector,
  p.close, p.volume, p.price_date,
  f.market_cap, f.enterprise_value, f.shares_outstanding, f.free_float_pct,
  s.trailing_pe, s.forward_pe, s.price_to_book, s.dividend_yield, s.beta,
  s.return_on_equity, s.profit_margins,
  i.rsi_14, i.sma_50, i.sma_200
FROM companies c
LEFT JOIN p ON p.ticker = c.ticker
LEFT JOIN f ON f.ticker = c.ticker
LEFT JOIN summary s ON s.ticker = c.ticker
LEFT JOIN i ON i.ticker = c.ticker;";

const RETURNS_VIEW: &str = "\
CREATE VIEW returns AS
WITH lc AS (
  SELECT ticker, close, date AS as_of
  FROM prices QUALIFY row_number() OVER (PARTITION BY ticker ORDER BY date DESC) = 1
)
SELECT lc.ticker, lc.as_of, lc.close,
  100.0 * (lc.close / NULLIF(w1w.close, 0) - 1) AS ret_1w,
  100.0 * (lc.close / NULLIF(w1m.close, 0) - 1) AS ret_1m,
  100.0 * (lc.close / NULLIF(w3m.close, 0) - 1) AS ret_3m,
  100.0 * (lc.close / NULLIF(w6m.close, 0) - 1) AS ret_6m,
  100.0 * (lc.close / NULLIF(wyt.close, 0) - 1) AS ret_ytd,
  100.0 * (lc.close / NULLIF(w1y.close, 0) - 1) AS ret_1y,
  100.0 * (lc.close / NULLIF(w3y.close, 0) - 1) AS ret_3y,
  100.0 * (power(lc.close / NULLIF(w3y.close, 0), 1.0 / 3.0) - 1) AS cagr_3y
FROM lc
ASOF LEFT JOIN prices w1w ON w1w.ticker = lc.ticker AND w1w.date <= lc.as_of - INTERVAL '7 days'
ASOF LEFT JOIN prices w1m ON w1m.ticker = lc.ticker AND w1m.date <= lc.as_of - INTERVAL '1 month'
ASOF LEFT JOIN prices w3m ON w3m.ticker = lc.ticker AND w3m.date <= lc.as_of - INTERVAL '3 months'
ASOF LEFT JOIN prices w6m ON w6m.ticker = lc.ticker AND w6m.date <= lc.as_of - INTERVAL '6 months'
ASOF LEFT JOIN prices wyt ON wyt.ticker = lc.ticker AND wyt.date <  date_trunc('year', lc.as_of)
ASOF LEFT JOIN prices w1y ON w1y.ticker = lc.ticker AND w1y.date <= lc.as_of - INTERVAL '1 year'
ASOF LEFT JOIN prices w3y ON w3y.ticker = lc.ticker AND w3y.date <= lc.as_of - INTERVAL '3 years';";

const BROKER_NET_VIEW: &str = "\
CREATE VIEW broker_net AS
SELECT ticker, date, broker_code,
  sum(value) FILTER (WHERE side = 'B') AS buy_value,
  sum(value) FILTER (WHERE side = 'S') AS sell_value,
  coalesce(sum(value) FILTER (WHERE side = 'B'), 0) - coalesce(sum(value) FILTER (WHERE side = 'S'), 0) AS net_value,
  sum(volume_lot) FILTER (WHERE side = 'B') AS buy_volume_lot,
  sum(volume_lot) FILTER (WHERE side = 'S') AS sell_volume_lot,
  coalesce(sum(volume_lot) FILTER (WHERE side = 'B'), 0) - coalesce(sum(volume_lot) FILTER (WHERE side = 'S'), 0) AS net_volume_lot
FROM broker_activity
GROUP BY ticker, date, broker_code;";

/// Open the built file read-only, lock it down, and clone N connections that
/// share the one locked instance (one buffer pool, one memory budget) so
/// queries run concurrently. Read-only + external-access-off + lock are
/// instance-level, so every cloned connection is equally sandboxed.
fn open_serving_ro(path: &Path, tables: Vec<String>, views: Vec<String>) -> Result<Serving> {
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)?
        .enable_external_access(false)?
        .enable_autoload_extension(false)?
        .max_memory(MAX_MEMORY)?
        .threads(serving_threads())?;
    let primary = Connection::open_with_flags(path, config)
        .with_context(|| format!("open serving db read-only {}", path.display()))?;
    // Last config action: forbid any further configuration change for the
    // instance, so external access can never be turned back on.
    primary
        .execute_batch("SET lock_configuration = true;")
        .context("lock configuration")?;

    let mut conns = Vec::with_capacity(SERVING_CONNECTIONS);
    for _ in 1..SERVING_CONNECTIONS {
        let clone = primary.try_clone().context("clone serving connection")?;
        let interrupt = clone.interrupt_handle();
        conns.push(ConnSlot {
            conn: Arc::new(Mutex::new(clone)),
            interrupt,
        });
    }
    let interrupt = primary.interrupt_handle();
    conns.push(ConnSlot {
        conn: Arc::new(Mutex::new(primary)),
        interrupt,
    });

    Ok(Serving {
        conns,
        next: AtomicUsize::new(0),
        path: path.to_path_buf(),
        tables,
        views,
    })
}

/// Worker threads for the serving instance — the machine's parallelism, clamped
/// so a query gets real CPU without oversubscribing tiny or huge hosts.
fn serving_threads() -> i64 {
    let n = std::thread::available_parallelism().map_or(4, |p| p.get().clamp(2, 16));
    i64::try_from(n).unwrap_or(4)
}

/// Validate untrusted SQL using `DuckDB`'s own parser. Guarantees: exactly one
/// SELECT statement, every base table in the allowlist, no file/network table
/// functions. `json_serialize_sql` errors on any non-SELECT statement.
fn validate(conn: &Connection, sql: &str) -> Result<()> {
    // `json_serialize_sql` needs a VARCHAR literal, not a bound param. Embedding
    // the query as a single-quote-escaped literal is safe: it parses (never
    // executes) the string, and doubling quotes prevents breaking out of it.
    let probe = format!("SELECT json_serialize_sql('{}')", sql.replace('\'', "''"));
    let ast: String = conn
        .query_row(&probe, [], |r| r.get(0))
        .context("parse query")?;
    let v: Value = serde_json::from_str(&ast).context("read parsed ast")?;

    if v.get("error").and_then(Value::as_bool) == Some(true) {
        let msg = v
            .get("error_message")
            .and_then(Value::as_str)
            .unwrap_or("invalid SQL");
        bail!("only a single read-only SELECT is allowed: {msg}");
    }
    let stmts = v
        .get("statements")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("could not parse query"))?;
    if stmts.len() != 1 {
        bail!("exactly one statement is allowed (got {})", stmts.len());
    }

    let mut bases = BTreeSet::new();
    let mut ctes = BTreeSet::new();
    let mut bad_fn = None;
    walk(&v, &mut bases, &mut ctes, &mut bad_fn);

    if let Some(f) = bad_fn {
        bail!("table function not allowed: {f}");
    }
    for table in bases.difference(&ctes) {
        if !catalog::is_allowed_table(table) {
            bail!("unknown or disallowed table: {table}");
        }
    }
    Ok(())
}

/// Recursively collect base-table names, CTE names, and the first disallowed
/// table-function name from a `json_serialize_sql` AST.
fn walk(
    v: &Value,
    bases: &mut BTreeSet<String>,
    ctes: &mut BTreeSet<String>,
    bad_fn: &mut Option<String>,
) {
    match v {
        Value::Object(map) => {
            match map.get("type").and_then(Value::as_str) {
                Some("BASE_TABLE") => {
                    if let Some(t) = map.get("table_name").and_then(Value::as_str) {
                        bases.insert(t.to_ascii_lowercase());
                    }
                }
                Some("TABLE_FUNCTION") => {
                    if let Some(name) = map
                        .get("function")
                        .and_then(|f| f.get("function_name"))
                        .and_then(Value::as_str)
                        && bad_fn.is_none()
                        && !catalog::is_safe_table_function(name)
                    {
                        *bad_fn = Some(name.to_string());
                    }
                }
                _ => {}
            }
            if let Some(entries) = map
                .get("cte_map")
                .and_then(|c| c.get("map"))
                .and_then(Value::as_array)
            {
                for e in entries {
                    if let Some(k) = e.get("key").and_then(Value::as_str) {
                        ctes.insert(k.to_ascii_lowercase());
                    }
                }
            }
            for child in map.values() {
                walk(child, bases, ctes, bad_fn);
            }
        }
        Value::Array(arr) => {
            for child in arr {
                walk(child, bases, ctes, bad_fn);
            }
        }
        _ => {}
    }
}

/// Run a `... to_json(t) ...`-wrapped query and parse each row into JSON.
fn collect_json(conn: &Connection, wrapped: &str, params: &[&dyn ToSql]) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(wrapped).context("prepare query")?;
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

/// Live `(name, type)` column list for one relation.
fn describe_columns(conn: &Connection, relation: &str) -> Result<Value> {
    let sql = format!("SELECT column_name, column_type FROM (DESCRIBE \"{relation}\")");
    let mut stmt = conn.prepare(&sql).context("describe relation")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "name": row.get::<_, String>(0)?,
                "type": row.get::<_, String>(1)?,
            }))
        })
        .context("read columns")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("column row")?);
    }
    Ok(Value::Array(out))
}

/// Remove any stale `serving-*.duckdb` files left from a previous run.
fn clear_serving_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("serving-")
            && name.ends_with(".duckdb")
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            tracing::warn!(path = %entry.path().display(), error = %e, "failed to remove stale serving file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DataBase;

    fn local_cfg() -> AppConfig {
        AppConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            sqlite_path: ":memory:".to_string(),
            data_base: DataBase::Local("data".to_string()),
        }
    }

    /// One comprehensive test against the local ./data mirror — builds the
    /// (heavy) serving DB once. Skips, rather than fails, when data is absent,
    /// so CI without data stays green.
    #[tokio::test]
    async fn engine_on_real_data() {
        if !Path::new("data/prices").exists() {
            eprintln!("skip: ./data not present");
            return;
        }
        let a = Analytics::new(&local_cfg()).expect("build serving db");

        // all base tables + the three views loaded
        assert!(
            a.loaded_tables().len() >= 10,
            "tables: {:?}",
            a.loaded_tables()
        );
        for v in ["latest", "returns", "broker_net"] {
            assert!(a.loaded_views().iter().any(|x| x == v), "missing view {v}");
        }

        // valid query returns exactly the requested rows
        let out = a
            .run_query(
                "SELECT ticker, close FROM prices ORDER BY date DESC LIMIT 3",
                None,
            )
            .await
            .expect("valid query");
        assert_eq!(out.rows.len(), 3);
        assert!(!out.truncated);

        // the three analytical views are queryable
        a.run_query(
            "SELECT ticker, cagr_3y FROM returns ORDER BY cagr_3y DESC NULLS LAST LIMIT 5",
            None,
        )
        .await
        .expect("returns view");
        let bn = a
            .run_query("SELECT ticker, date, broker_code, net_value FROM broker_net ORDER BY net_value DESC LIMIT 3", None)
            .await
            .expect("broker_net view");
        assert!(!bn.rows.is_empty());
        a.run_query(
            "SELECT ticker FROM latest WHERE market_cap IS NOT NULL LIMIT 5",
            None,
        )
        .await
        .expect("latest view");

        // every dangerous shape is rejected
        for sql in [
            "SELECT * FROM read_parquet('data/prices/latest.parquet')",
            "DROP TABLE prices",
            "INSERT INTO prices VALUES (1)",
            "SELECT 1; SELECT 2",
            "SELECT * FROM duckdb_settings()",
            "ATTACH 'x.db' AS y",
            "COPY prices TO 'x.csv'",
            "PRAGMA database_list",
        ] {
            assert!(
                a.run_query(sql, None).await.is_err(),
                "should reject: {sql}"
            );
        }

        // the row cap truncates and flags it
        let capped = a
            .run_query("SELECT ticker FROM prices", Some(5))
            .await
            .expect("capped");
        assert_eq!(capped.rows.len(), 5);
        assert!(capped.truncated);

        // describe lists tables and views
        let d = a.describe(None).await.expect("describe");
        let arr = d.as_array().expect("array");
        assert!(arr.iter().any(|r| r["name"] == "prices"));
        assert!(arr.iter().any(|r| r["name"] == "returns"));

        // the connection pool serves concurrent queries without deadlock
        let (r1, r2, r3, r4) = tokio::join!(
            a.run_query("SELECT count(*) FROM prices", None),
            a.run_query("SELECT count(*) FROM companies", None),
            a.run_query("SELECT count(*) FROM eod_summary", None),
            a.run_query("SELECT count(*) FROM broker_rankings", None),
        );
        assert!(
            r1.is_ok() && r2.is_ok() && r3.is_ok() && r4.is_ok(),
            "concurrent pooled queries failed"
        );
    }
}
