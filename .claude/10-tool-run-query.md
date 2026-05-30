> Docs index: see [the index](01-index.md) to discover all pages.

# run_query

> Run a read-only SQL SELECT over the IDX data — the flexible tool for any analytical or derived question.

### Overview

The flexible core of the server. Write a single read-only `SELECT` over the documented tables and the analytical views, and get rows back as JSON. Use it whenever a typed shortcut doesn't fit: cross-sectional screens, rankings, comparisons, returns, broker-flow/bandarmology, foreign-flow analysis. Call [describe_schema](describe-schema.md) first to see the exact tables, views, and columns.

**Queryable relations.** The 12 base tables — `prices`, `eod_summary`, `indicators`, `fundamentals`, `broker_activity`, `broker_distribution`, `broker_rankings`, `announcements`, `companies`, `summary`, `analyst`, `ownership` — plus three pre-built views:

* `latest` — one row per ticker: latest close/volume, fundamentals, Yahoo ratios, and key indicators joined. For screening.
* `returns` — one row per ticker: trailing `ret_1w/1m/3m/6m/ytd/1y/3y` and annualized `cagr_3y` from close prices.
* `broker_net` — one row per ticker+date+broker_code: `buy_value`, `sell_value`, `net_value`, and the volume equivalents. Base for accumulation / flip / market-maker analysis.

**Sandbox.** The query runs against a locked, read-only connection. Only a single `SELECT`/`WITH` is allowed; statements referencing any table outside the list above, file/network functions (`read_parquet`, `read_csv`, …), or any write/DDL/`ATTACH`/`COPY`/`PRAGMA` are rejected. Results are capped at 5000 rows (a `truncated` flag signals when the cap was hit), and queries are interrupted after 15 seconds.

### Parameters

* `sql` (string, required) — one read-only `SELECT` (or `WITH … SELECT`).
* `limit` (integer, optional) — max rows, default and hard cap `5000`.

### Example

"Which IDX stocks compounded ~20%/year over the available history, by sector?"

```json
{
  "method": "tools/call",
  "params": {
    "name": "run_query",
    "arguments": {
      "sql": "SELECT l.ticker, l.sector, r.cagr_3y, r.ret_1y FROM latest l JOIN returns r USING(ticker) WHERE r.cagr_3y BETWEEN 18 AND 22 ORDER BY r.cagr_3y DESC",
      "limit": 25
    }
  }
}
```

Broker accumulation (who net-bought a ticker hardest last month):

```json
{
  "method": "tools/call",
  "params": {
    "name": "run_query",
    "arguments": {
      "sql": "SELECT broker_code, sum(net_value) net FROM broker_net WHERE ticker='BBCA' AND date >= '2026-04-01' GROUP BY broker_code ORDER BY net DESC LIMIT 10"
    }
  }
}
```

### Example Response

```json
{
  "row_count": 2,
  "truncated": false,
  "rows": [
    {"ticker": "XXXX", "sector": "Financials", "cagr_3y": 21.4, "ret_1y": 33.1}
  ]
}
```

### Notes

Yahoo-derived valuation ratios (`trailing_pe`, `forward_pe`, `price_to_book`) in `summary`/`latest` are unreliable for IDX names — treat with caution. A trailing-return column is `NULL` when a ticker's history is shorter than the window (e.g. `cagr_3y` needs ≥3 years of prices). `broker_rankings` has no `ticker` column (it is market-wide). On a rejected or invalid query the tool returns an error explaining why.
