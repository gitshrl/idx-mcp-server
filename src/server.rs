use std::fmt::Write as _;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::analytics::Analytics;
use crate::catalog;

// Every served relation exposes `ticker` (UPPERCASE) and, for time series,
// `date` (DATE). Tools query the loaded serving database, never Parquet files.
const MAX_ROWS: u32 = 5_000;

/// The IDX market-data MCP server. One instance per session; all share the
/// analytics engine.
#[derive(Clone)]
pub struct IdxServer {
    analytics: Arc<Analytics>,
    // Read by the rmcp `#[tool_handler]` macro; dead-code analysis misses that.
    #[allow(dead_code)]
    tool_router: ToolRouter<IdxServer>,
}

// ---- tool request types (schemas auto-derived from these) ----

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchTickersReq {
    /// Substring to match against ticker symbol or company name.
    pub query: String,
    /// Max results (default 20).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TickerReq {
    /// IDX ticker symbol, e.g. "BBCA".
    pub ticker: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PricesReq {
    /// IDX ticker symbol, e.g. "BBCA".
    pub ticker: String,
    /// Inclusive start date, YYYY-MM-DD.
    pub from: Option<String>,
    /// Inclusive end date, YYYY-MM-DD.
    pub to: Option<String>,
    /// Price source: "yf" (Yahoo OHLCV, default) or "idx" (official summary + foreign flow).
    pub source: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RangeReq {
    /// IDX ticker symbol, e.g. "BBCA".
    pub ticker: String,
    /// Inclusive start date, YYYY-MM-DD.
    pub from: Option<String>,
    /// Inclusive end date, YYYY-MM-DD.
    pub to: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AnnouncementsReq {
    /// Optional ticker filter, e.g. "BBCA".
    pub ticker: Option<String>,
    /// Inclusive start date, YYYY-MM-DD.
    pub from: Option<String>,
    /// Inclusive end date, YYYY-MM-DD.
    pub to: Option<String>,
    /// Max results (default 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunQueryReq {
    /// A single read-only SELECT over the documented tables and views
    /// (`latest`, `returns`, `broker_net`). Call `describe_schema` first.
    pub sql: String,
    /// Max rows to return (default and hard cap 5000).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DescribeReq {
    /// Optional single table/view name. Omit to describe everything.
    pub dataset: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenReq {
    /// Filters, all AND-ed, over the per-ticker `latest` snapshot.
    pub filters: Vec<ScreenFilter>,
    /// Optional sort (defaults to `market_cap` descending).
    pub sort: Option<ScreenSort>,
    /// Max results (default 50, hard cap 5000).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenFilter {
    /// Field to filter on; see `describe_schema latest` for the options.
    pub field: String,
    /// `= != < <= > >= between` for numeric fields, `= in` for `sector`.
    pub op: String,
    /// A number, a string (for `sector`), or a 2-element array for `between` / a list for `in`.
    pub value: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenSort {
    /// Field to sort by.
    pub field: String,
    /// Descending if true (default true).
    pub desc: Option<bool>,
}

#[tool_router]
impl IdxServer {
    pub fn new(analytics: Arc<Analytics>) -> Self {
        Self {
            analytics,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Search IDX tickers by symbol or company name.")]
    async fn search_tickers(
        &self,
        Parameters(req): Parameters<SearchTickersReq>,
    ) -> Result<CallToolResult, McpError> {
        let limit = req.limit.unwrap_or(20).min(MAX_ROWS);
        let sql = format!(
            "SELECT ticker, company_name, sector, exchange FROM companies \
             WHERE ticker ILIKE ? OR company_name ILIKE ? LIMIT {limit}"
        );
        let pattern = format!("%{}%", req.query);
        let rows = self
            .analytics
            .query_json(sql, vec![pattern.clone(), pattern])
            .await
            .map_err(mcp_err)?;
        Ok(json_array(rows))
    }

    #[tool(
        description = "Company profile, key statistics, and Yahoo summary for an IDX ticker. \
                       Note: Yahoo valuation ratios (PE, price/book) are unreliable for IDX names."
    )]
    async fn get_company(
        &self,
        Parameters(req): Parameters<TickerReq>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self
            .first(
                "SELECT ticker, company_name, sector, sub_sector, exchange, country, status, \
                 instrument_type, listing_board, ipo_date, company_background \
                 FROM companies WHERE ticker = ? LIMIT 1",
                &req.ticker,
            )
            .await?;
        let key_stats = self
            .first(
                "SELECT ticker, date, market_cap, enterprise_value, shares_outstanding, \
                 free_float_pct, latest_dividend, latest_dividend_year \
                 FROM fundamentals WHERE ticker = ? ORDER BY date DESC LIMIT 1",
                &req.ticker,
            )
            .await?;
        let summary = self
            .first(
                "SELECT ticker, name, market_cap, trailing_pe, forward_pe, price_to_book, \
                 dividend_yield, beta, return_on_equity, profit_margins, week_high_52, \
                 week_low_52, target_mean_price, recommendation_key \
                 FROM summary WHERE ticker = ? LIMIT 1",
                &req.ticker,
            )
            .await?;
        Ok(json_value(
            &json!({ "profile": profile, "key_stats": key_stats, "summary": summary }),
        ))
    }

    #[tool(
        description = "Daily OHLCV price history for an IDX ticker. source: 'yf' (default) or 'idx' (official, with foreign flow)."
    )]
    async fn get_prices(
        &self,
        Parameters(req): Parameters<PricesReq>,
    ) -> Result<CallToolResult, McpError> {
        let (cols, table) = match req.source.as_deref() {
            Some("idx") => (
                "ticker, date, open, high, low, close, previous, change, volume, value, \
                 frequency, foreign_buy, foreign_sell",
                "eod_summary",
            ),
            _ => (
                "ticker, date, open, high, low, close, volume, dividends, splits",
                "prices",
            ),
        };
        let mut sql = format!("SELECT {cols} FROM {table} WHERE ticker = ?");
        push_date_range(&mut sql, req.from.as_ref(), req.to.as_ref())?;
        let _ = write!(sql, " ORDER BY date LIMIT {MAX_ROWS}");
        let rows = self
            .analytics
            .query_json(sql, vec![req.ticker])
            .await
            .map_err(mcp_err)?;
        Ok(json_array(rows))
    }

    #[tool(
        description = "Per-broker buy/sell activity (bandarmology) for an IDX ticker over a date range."
    )]
    async fn get_broker_activity(
        &self,
        Parameters(req): Parameters<RangeReq>,
    ) -> Result<CallToolResult, McpError> {
        let mut sql = "SELECT ticker, date, broker_code, side, volume_lot, value, frequency, \
             avg_price, domicile FROM broker_activity WHERE ticker = ?"
            .to_string();
        push_date_range(&mut sql, req.from.as_ref(), req.to.as_ref())?;
        let _ = write!(sql, " ORDER BY date, value DESC LIMIT {MAX_ROWS}");
        let rows = self
            .analytics
            .query_json(sql, vec![req.ticker])
            .await
            .map_err(mcp_err)?;
        Ok(json_array(rows))
    }

    #[tool(
        description = "KSEI depository ownership for an IDX ticker: holders with >=1% of shares, local/foreign."
    )]
    async fn get_ownership(
        &self,
        Parameters(req): Parameters<TickerReq>,
    ) -> Result<CallToolResult, McpError> {
        let sql = "SELECT ticker, date, name, type, classification, local_foreign, total_shares, \
             percentage FROM ownership WHERE ticker = ? ORDER BY percentage DESC LIMIT 5000"
            .to_string();
        let rows = self
            .analytics
            .query_json(sql, vec![req.ticker])
            .await
            .map_err(mcp_err)?;
        Ok(json_array(rows))
    }

    #[tool(
        description = "IDX corporate announcements and news, optionally filtered by ticker and date range."
    )]
    async fn get_announcements(
        &self,
        Parameters(req): Parameters<AnnouncementsReq>,
    ) -> Result<CallToolResult, McpError> {
        let limit = req.limit.unwrap_or(50).min(MAX_ROWS);
        let mut sql = "SELECT ticker, date, source, title, subject, announcement_type, \
             announcement_no, published_at FROM announcements WHERE 1=1"
            .to_string();
        if req.ticker.is_some() {
            sql.push_str(" AND ticker = ?");
        }
        push_date_range(&mut sql, req.from.as_ref(), req.to.as_ref())?;
        let _ = write!(sql, " ORDER BY date DESC LIMIT {limit}");
        let params: Vec<String> = req.ticker.into_iter().collect();
        let rows = self
            .analytics
            .query_json(sql, params)
            .await
            .map_err(mcp_err)?;
        Ok(json_array(rows))
    }

    #[tool(
        description = "Run a read-only SQL SELECT over the IDX data — the flexible tool for any \
                       analytical or derived question. Query the documented tables plus the views \
                       `latest` (per-ticker snapshot), `returns` (trailing/annualized returns), and \
                       `broker_net` (per-broker net flow). Call describe_schema first for columns."
    )]
    async fn run_query(
        &self,
        Parameters(req): Parameters<RunQueryReq>,
    ) -> Result<CallToolResult, McpError> {
        let limit = req.limit.map(|n| n as usize);
        let out = self
            .analytics
            .run_query(&req.sql, limit)
            .await
            .map_err(user_err)?;
        let count = out.rows.len();
        Ok(json_value(
            &json!({ "row_count": count, "truncated": out.truncated, "rows": out.rows }),
        ))
    }

    #[tool(
        description = "List the queryable tables and views with their columns and types. Use this \
                       to discover the schema before writing run_query SQL. Pass a name to focus on one."
    )]
    async fn describe_schema(
        &self,
        Parameters(req): Parameters<DescribeReq>,
    ) -> Result<CallToolResult, McpError> {
        let out = self
            .analytics
            .describe(req.dataset)
            .await
            .map_err(mcp_err)?;
        Ok(json_value(&out))
    }

    #[tool(
        description = "Screen stocks cross-sectionally on the latest per-ticker snapshot: filter on \
                       fundamentals/valuation/price/indicator fields and sort. For derived screens \
                       (e.g. returns), use run_query against the `returns` view. Note: PE/price_to_book \
                       are unreliable for IDX names."
    )]
    async fn screen_stocks(
        &self,
        Parameters(req): Parameters<ScreenReq>,
    ) -> Result<CallToolResult, McpError> {
        let (sql, params) = build_screen(&req)?;
        let rows = self
            .analytics
            .query_json(sql, params)
            .await
            .map_err(user_err)?;
        Ok(json_array(rows))
    }

    /// Run a parameterized query and return its first row, or JSON null.
    async fn first(&self, sql: &str, ticker: &str) -> Result<Value, McpError> {
        let rows = self
            .analytics
            .query_json(sql.to_string(), vec![ticker.to_string()])
            .await
            .map_err(mcp_err)?;
        Ok(rows.into_iter().next().unwrap_or(Value::Null))
    }
}

#[tool_handler]
impl ServerHandler for IdxServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Indonesian (IDX/KSEI) market data. Start with describe_schema to see the tables \
                 and columns. For anything analytical or derived, use run_query — a read-only SQL \
                 tool over the tables plus the views latest, returns, and broker_net. screen_stocks \
                 filters the per-ticker snapshot. Typed shortcuts: search_tickers, get_company, \
                 get_prices, get_broker_activity, get_ownership, get_announcements."
                    .to_string(),
            )
    }
}

// ---- screen_stocks SQL builder ----

/// Build a safe `SELECT * FROM latest ...` from typed filters. Fields and
/// operators are matched against the catalog allowlist; numeric values are
/// bound and `CAST` to DOUBLE, text values bound — never interpolated.
fn build_screen(req: &ScreenReq) -> Result<(String, Vec<String>), McpError> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    for f in &req.filters {
        let field = f.field.to_ascii_lowercase();
        let op = f.op.to_ascii_lowercase();
        let is_num = catalog::SCREEN_FIELDS_NUM.contains(&field.as_str());
        let is_txt = catalog::SCREEN_FIELDS_TEXT.contains(&field.as_str());

        if is_num {
            match op.as_str() {
                "=" | "!=" | "<" | "<=" | ">" | ">=" => {
                    clauses.push(format!("\"{field}\" {op} CAST(? AS DOUBLE)"));
                    params.push(num(&f.value)?);
                }
                "between" => {
                    let arr = f
                        .value
                        .as_array()
                        .filter(|a| a.len() == 2)
                        .ok_or_else(|| invalid("between needs a [low, high] array"))?;
                    clauses.push(format!(
                        "\"{field}\" BETWEEN CAST(? AS DOUBLE) AND CAST(? AS DOUBLE)"
                    ));
                    params.push(num(&arr[0])?);
                    params.push(num(&arr[1])?);
                }
                _ => {
                    return Err(invalid(format!(
                        "operator '{op}' not allowed on numeric field {field}"
                    )));
                }
            }
        } else if is_txt {
            match op.as_str() {
                "=" => {
                    let s = f
                        .value
                        .as_str()
                        .ok_or_else(|| invalid("sector = needs a string"))?;
                    clauses.push(format!("\"{field}\" = ?"));
                    params.push(s.to_string());
                }
                "in" => {
                    let arr = f
                        .value
                        .as_array()
                        .filter(|a| !a.is_empty())
                        .ok_or_else(|| invalid("in needs a non-empty array"))?;
                    let mut holes = Vec::with_capacity(arr.len());
                    for v in arr {
                        let s = v
                            .as_str()
                            .ok_or_else(|| invalid("in values must be strings"))?;
                        holes.push("?");
                        params.push(s.to_string());
                    }
                    clauses.push(format!("\"{field}\" IN ({})", holes.join(", ")));
                }
                _ => {
                    return Err(invalid(format!(
                        "operator '{op}' not allowed on field {field}"
                    )));
                }
            }
        } else {
            return Err(invalid(format!("not a screenable field: {}", f.field)));
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    let order = match &req.sort {
        Some(s) => {
            let field = s.field.to_ascii_lowercase();
            if !catalog::SCREEN_FIELDS_NUM.contains(&field.as_str())
                && !catalog::SCREEN_FIELDS_TEXT.contains(&field.as_str())
            {
                return Err(invalid(format!("not a sortable field: {}", s.field)));
            }
            let dir = if s.desc.unwrap_or(true) {
                "DESC"
            } else {
                "ASC"
            };
            format!(" ORDER BY \"{field}\" {dir} NULLS LAST")
        }
        None => " ORDER BY market_cap DESC NULLS LAST".to_string(),
    };

    let limit = req.limit.unwrap_or(50).min(MAX_ROWS);
    Ok((
        format!("SELECT * FROM latest{where_sql}{order} LIMIT {limit}"),
        params,
    ))
}

fn num(v: &Value) -> Result<String, McpError> {
    if let Some(n) = v.as_f64() {
        return Ok(n.to_string());
    }
    if let Some(s) = v.as_str()
        && s.parse::<f64>().is_ok()
    {
        return Ok(s.to_string());
    }
    Err(invalid("expected a number"))
}

// ---- helpers ----

// Both are used as `.map_err(_)` adaptors, which hand over an owned error.
#[allow(clippy::needless_pass_by_value)]
fn mcp_err(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[allow(clippy::needless_pass_by_value)]
fn user_err(e: anyhow::Error) -> McpError {
    McpError::invalid_params(e.to_string(), None)
}

fn invalid(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

fn json_array(rows: Vec<Value>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(Value::Array(rows).to_string())])
}

fn json_value(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(value.to_string())])
}

/// Append validated `date` range clauses (values inlined only after format check).
fn push_date_range(
    sql: &mut String,
    from: Option<&String>,
    to: Option<&String>,
) -> Result<(), McpError> {
    if let Some(f) = from {
        ensure_date(f)?;
        let _ = write!(sql, " AND date >= '{f}'");
    }
    if let Some(t) = to {
        ensure_date(t)?;
        let _ = write!(sql, " AND date <= '{t}'");
    }
    Ok(())
}

fn ensure_date(s: &str) -> Result<(), McpError> {
    let b = s.as_bytes();
    let well_formed = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    // Cheap calendar-range check (no chrono dep): reject e.g. 2025-13-32 at the
    // boundary. Positions are guaranteed ASCII digits once `well_formed`.
    let valid = well_formed && {
        let month: u8 = s[5..7].parse().unwrap_or(0);
        let day: u8 = s[8..10].parse().unwrap_or(0);
        (1..=12).contains(&month) && (1..=31).contains(&day)
    };
    if valid {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            format!("date must be a valid YYYY-MM-DD, got {s:?}"),
            None,
        ))
    }
}
