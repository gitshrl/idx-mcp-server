use std::sync::Arc;

use duckdb::ToSql;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::Store;

// Canonical keys: every served dataset exposes `ticker` + `date` (the ETL
// normalizes stock_code/StockCode/Code/ticker and date/Date/report_date).
// Column projections below follow `.claude/plans/field-map.md`.
const MAX_ROWS: u32 = 5_000;

/// The IDX market-data MCP server. One instance per session; all share the `Store`.
#[derive(Clone)]
pub struct IdxServer {
    store: Arc<Store>,
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

#[tool_router]
impl IdxServer {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
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
            "SELECT ticker, company_name, sector, exchange FROM {} \
             WHERE ticker ILIKE ? OR company_name ILIKE ? LIMIT {limit}",
            self.snapshot("companies")
        );
        let pattern = format!("%{}%", req.query);
        let p: &dyn ToSql = &pattern;
        let rows = self.store.query_json(&sql, &[p, p]).map_err(mcp_err)?;
        json_array(rows)
    }

    #[tool(
        description = "Company profile, key statistics, and Yahoo summary for an IDX ticker. \
                       Note: Yahoo valuation ratios (PE, price/book) are unreliable for IDX names."
    )]
    async fn get_company(
        &self,
        Parameters(req): Parameters<TickerReq>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self.first(
            &format!(
                "SELECT ticker, company_name, sector, sub_sector, exchange, country, status, \
                 instrument_type, listing_board, ipo_date, company_background \
                 FROM {} WHERE ticker = ? LIMIT 1",
                self.snapshot("companies")
            ),
            &req.ticker,
        )?;
        let key_stats = self.first(
            &format!(
                "SELECT ticker, date, market_cap, enterprise_value, shares_outstanding, \
                 free_float_pct, latest_dividend, latest_dividend_year \
                 FROM {} WHERE ticker = ? ORDER BY date DESC LIMIT 1",
                self.timeseries("fundamentals")
            ),
            &req.ticker,
        )?;
        let summary = self.first(
            &format!(
                "SELECT ticker, name, market_cap, trailing_pe, forward_pe, price_to_book, \
                 dividend_yield, beta, return_on_equity, profit_margins, week_high_52, \
                 week_low_52, target_mean_price, recommendation_key \
                 FROM {} WHERE ticker = ? LIMIT 1",
                self.snapshot("summary")
            ),
            &req.ticker,
        )?;
        json_object(json!({ "profile": profile, "key_stats": key_stats, "summary": summary }))
    }

    #[tool(
        description = "Daily OHLCV price history for an IDX ticker. source: 'yf' (default) or 'idx' (official, with foreign flow)."
    )]
    async fn get_prices(
        &self,
        Parameters(req): Parameters<PricesReq>,
    ) -> Result<CallToolResult, McpError> {
        let (cols, dataset) = match req.source.as_deref() {
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
        let mut sql = format!(
            "SELECT {cols} FROM {} WHERE ticker = ?",
            self.timeseries(dataset)
        );
        push_date_range(&mut sql, &req.from, &req.to)?;
        sql.push_str(&format!(" ORDER BY date LIMIT {MAX_ROWS}"));
        let t: &dyn ToSql = &req.ticker;
        let rows = self.store.query_json(&sql, &[t]).map_err(mcp_err)?;
        json_array(rows)
    }

    #[tool(
        description = "Per-broker buy/sell activity (bandarmology) for an IDX ticker over a date range."
    )]
    async fn get_broker_activity(
        &self,
        Parameters(req): Parameters<RangeReq>,
    ) -> Result<CallToolResult, McpError> {
        let mut sql = format!(
            "SELECT ticker, date, broker_code, side, volume_lot, value, frequency, avg_price, domicile \
             FROM {} WHERE ticker = ?",
            self.timeseries("broker_activity")
        );
        push_date_range(&mut sql, &req.from, &req.to)?;
        sql.push_str(&format!(" ORDER BY date, value DESC LIMIT {MAX_ROWS}"));
        let t: &dyn ToSql = &req.ticker;
        let rows = self.store.query_json(&sql, &[t]).map_err(mcp_err)?;
        json_array(rows)
    }

    #[tool(
        description = "KSEI depository ownership for an IDX ticker: holders with >=1% of shares, local/foreign."
    )]
    async fn get_ownership(
        &self,
        Parameters(req): Parameters<TickerReq>,
    ) -> Result<CallToolResult, McpError> {
        let sql = format!(
            "SELECT ticker, date, name, type, classification, local_foreign, total_shares, percentage \
             FROM {} WHERE ticker = ? ORDER BY percentage DESC LIMIT {MAX_ROWS}",
            self.snapshot("ownership")
        );
        let t: &dyn ToSql = &req.ticker;
        let rows = self.store.query_json(&sql, &[t]).map_err(mcp_err)?;
        json_array(rows)
    }

    #[tool(
        description = "IDX corporate announcements and news, optionally filtered by ticker and date range."
    )]
    async fn get_announcements(
        &self,
        Parameters(req): Parameters<AnnouncementsReq>,
    ) -> Result<CallToolResult, McpError> {
        let limit = req.limit.unwrap_or(50).min(MAX_ROWS);
        let mut sql = format!(
            "SELECT ticker, date, source, title, subject, announcement_type, announcement_no, published_at \
             FROM {} WHERE 1=1",
            self.timeseries("announcements")
        );
        let ticker = req.ticker.clone();
        if ticker.is_some() {
            sql.push_str(" AND ticker = ?");
        }
        push_date_range(&mut sql, &req.from, &req.to)?;
        sql.push_str(&format!(" ORDER BY date DESC LIMIT {limit}"));
        let rows = match &ticker {
            Some(t) => {
                let t: &dyn ToSql = t;
                self.store.query_json(&sql, &[t]).map_err(mcp_err)?
            }
            None => self.store.query_json(&sql, &[]).map_err(mcp_err)?,
        };
        json_array(rows)
    }

    /// `read_parquet(...)` source for a date-partitioned (daily) time-series
    /// dataset; the `date` column comes from the hive partition path.
    fn timeseries(&self, dataset: &str) -> String {
        format!(
            "read_parquet('{}', hive_partitioning=true)",
            self.store.parquet_glob(dataset, "date=*/*.parquet")
        )
    }

    /// `read_parquet(...)` source for a latest-per-ticker snapshot dataset.
    fn snapshot(&self, dataset: &str) -> String {
        format!(
            "read_parquet('{}')",
            self.store.parquet_glob(dataset, "latest.parquet")
        )
    }

    /// Run a query and return the first row, or JSON null if there are none.
    fn first(&self, sql: &str, ticker: &str) -> Result<Value, McpError> {
        let t: &dyn ToSql = &ticker;
        let rows = self.store.query_json(sql, &[t]).map_err(mcp_err)?;
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
                "Indonesian market data (IDX/KSEI). Tools: search_tickers, get_company, \
                 get_prices, get_broker_activity, get_ownership, get_announcements."
                    .to_string(),
            )
    }
}

// ---- helpers ----

fn mcp_err(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn json_array(rows: Vec<Value>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        Value::Array(rows).to_string(),
    )]))
}

fn json_object(obj: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        obj.to_string(),
    )]))
}

/// Append validated `date` range clauses (values inlined only after format check).
fn push_date_range(
    sql: &mut String,
    from: &Option<String>,
    to: &Option<String>,
) -> Result<(), McpError> {
    if let Some(f) = from {
        ensure_date(f)?;
        sql.push_str(&format!(" AND date >= '{f}'"));
    }
    if let Some(t) = to {
        ensure_date(t)?;
        sql.push_str(&format!(" AND date <= '{t}'"));
    }
    Ok(())
}

fn ensure_date(s: &str) -> Result<(), McpError> {
    let b = s.as_bytes();
    let ok = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    if ok {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            format!("date must be YYYY-MM-DD, got {s:?}"),
            None,
        ))
    }
}
