# Implementation Plan — v1: Flexible Q&A on the IDX data

> Resolved via a full design grill. Companion to `04-architecture.md` (design), `05-data-contract.md` (the data spec), `01-index.md` (index). OAuth 2.1 + accounts is a **separate, deferred** track (see `04-architecture.md` §Auth).

**Goal:** Let an agent answer open-ended analytical questions over the IDX data — screens, rankings, returns, broker-flow/bandarmology, foreign-flow — through a flexible SQL tool plus a few typed shortcuts, with untrusted SQL sandboxed.

**Tech:** Rust · rmcp 1.7 · axum 0.8 · DuckDB 1.105 (bundled) · `sqlparser` · edition 2024 · strict CI (fmt + clippy all+pedantic `-D warnings` + tests).

---

## Status (2026-05-31)

**Built + verified** — `clippy`/`fmt`/tests green and a live MCP end-to-end run (`scripts/e2e.sh`): the serving engine (`analytics.rs`), the catalog + allowlist (`catalog.rs`), all 10 tools, the `latest`/`returns`/`broker_net` views, and SIGHUP refresh. **Beyond the original plan:** the OAuth 2.1 authorization server (DCR + PKCE + audience-bound tokens) is built, and `get_filing` fetches announcement PDFs past Cloudflare on demand (see `19-tool-get-filing.md`). **Deviation from the plan:** SQL validation uses DuckDB's own parser (`json_serialize_sql`) instead of `sqlparser` — zero dialect drift, no extra dependency. **Remaining:** `broker_distribution` currently loads as nested JSON (the edge-explode is external-ETL work, M5); the financials tier (keystats metrics) stays deferred as planned.

## Locked decisions (the grill outcomes)

1. **Unify on one DB.** All tools query a single loaded, locked **read-only DuckDB serving DB**. The current live-`read_parquet` path is retired. (`run_query` *forces* a sandboxed conn with external access OFF, which forces materialized tables; serving the shortcuts from the same tables gives one schema, one security surface, faster reads, single source of truth.)
2. **Lean renamed surface, no raw tier.** Expose only the **12 source-neutral renamed tables** (`prices`, `eod_summary`, …) — clean names, curated columns. **No `raw_<collection>` tables, no nested-JSON dump.** "Cover it all" is deferred: add columns/tables on demand when a real question needs them.
3. **Refresh = manual.** Serving DB built at boot; refresh on `SIGHUP` or an `idx-mcp refresh` subcommand. ETL-triggered refresh / scheduler is a later add.
4. **Tools (9 in v1):** `search_tickers`, `get_company`, `get_prices`, `get_broker_activity`, `get_ownership`, `get_announcements` (the 6 shortcuts, migrated onto the serving conn) + `run_query`, `describe_schema`, `screen_stocks`.
5. **Defer financials + filings.** The financials tier (XBRL/API numbers) and `filings_text` (PDF prose) + their tools (`get_financials`, `compute_financial_ratio`, `get_filing`) are **out of v1** — they need an external feed that doesn't exist yet. Keep the `announcements.attachments` PDF pointer as the bridge.
6. **`run_query`:** flat-gated (any valid key → full access), `sqlparser`-validated (one `SELECT`/`WITH`, tables ∈ allowlist, reject DDL/DML/file-funcs), returns a **JSON array of objects**, hard cap **5000 rows** + `truncated: true` when hit.
7. **`describe_schema`:** hybrid — columns/types auto-introspected from the live serving DB, semantics/landmines overlaid from the checked-in catalog.
8. **Limits:** 15s query timeout (`spawn_blocking` + `interrupt`), 2 GB mem / 2 threads per serving conn. Env-tunable.

## Data & query model

```
EXTERNAL ETL (separate repo) ──writes Parquet──►  the 12 lean renamed tables (curated columns, canonical ticker/date)
        │
        ▼  boot + manual refresh (SIGHUP / `idx-mcp refresh`)
  analytics.rs LOADER (trusted RW conn): CREATE TABLE per renamed Parquet  → serving-<epoch>.duckdb
                                         CREATE VIEW latest / returns / broker_net  (over the loaded tables)
        │
        ▼  open SEPARATE read-only LOCKED conn (Config fixed: ReadOnly + external access OFF; lock_configuration last)
  ALL tools query the serving DB:
    shortcuts / screen_stocks  → param-bound SELECTs on tables/views
    run_query                  → sqlparser-validated single SELECT over the same tables/views
```

The MCP server stays **egress-free** — all Mongo export, future XBRL ingest, and future PDF download/extract live in the external ETL. The dev `src/bin/etl.rs` is the local loader; `05-data-contract.md` is the spec the external ETL meets.

### The 12 renamed tables (lean columns; `D`=double `I`=bigint `V`=varchar `T`=timestamp `B`=bool)

Time-series (keyed `ticker`+`date`):
- **prices** — `open D, high, low, close, volume I, dividends D, splits D`
- **eod_summary** — `stock_name V, open, high, low, close, previous, change D, volume I, value I, frequency I, foreign_buy I, foreign_sell I`
- **indicators** — 24 flat doubles: `rsi_14, macd, macd_signal, macd_hist, sma_5/10/20/50/200, ema_12/26/50, bb_upper/mid/lower, atr_14, vwap, vol_sma_20, vol_ratio, high_20d, low_20d, change, change_5d, change_20d`
- **fundamentals** — `market_cap D, enterprise_value D, shares_outstanding D, free_float_pct D, latest_dividend D, latest_dividend_year I, latest_dividend_ex_date, latest_dividend_payment_date`
- **broker_activity** — `broker_code V, side V(B|S), volume_lot I, value I, frequency I, avg_price D, domicile V(LOCAL|FOREIGN|GOVERNMENT)`
- **broker_distribution** — **exploded edges**, grain `ticker,date,side,source_code,counterparty_code`: `side V(buy|sell), source_code V, source_type V, counterparty_code V, counterparty_type V, value I (rupiah), volume I (lot)` (by_value ⋈ by_volume per edge; NULL where one side absent)
- **broker_rankings** — market-wide, NO ticker: `broker_code V, firm_name V, date, frequency I, value I, volume I, rank_no I`
- **announcements** — `source V(announcement|news), title V, subject V, announcement_type V, announcement_no V, published_at T, created_at T, attachments JSON` (PDF pointers — the filings bridge)

Snapshots (latest per ticker):
- **companies** — `company_name V, sector V, sub_sector V, exchange V, country V, status V, is_tradeable B, instrument_type V, listing_board V, ipo_date, ipo_price D, free_float_percent D, company_background V`
- **summary** (⚠️ yf ratios unreliable for IDX — tag, don't trust) — `name V, sector V, industry V, market_cap D, enterprise_value D, shares_outstanding D, trailing_pe D, forward_pe D, price_to_book D, beta D, return_on_equity D, profit_margins D, revenue D, revenue_growth D, earnings_growth D, debt_to_equity D, total_cash D, total_debt D, free_cash_flow D, dividend_yield D, payout_ratio D, week_high_52 D, week_low_52 D, target_mean_price D, recommendation_key V, number_of_analyst_opinions I`
- **analyst** — `rec_strong_buy I, rec_buy, rec_hold, rec_sell, rec_strong_sell I, target_current D, target_mean, target_median, target_high, target_low D, eps_est_avg_cy D, eps_est_avg_1y, eps_growth_1y, rev_est_avg_cy, rev_est_avg_1y D`
- **ownership** — exploded, holders ≥1%: `name V, type V, classification V, local_foreign V(LOCAL|FOREIGN), nationality V, total_shares I, percentage D`

### The 3 analytical views (built by the loader, over the tables)

- **`latest`** — one row/ticker: latest-of-each-source joined on `ticker` (close+volume from `prices`, rsi/sma from `indicators`, market_cap/free_float from `fundamentals`, pe/pb/roe/yield from `summary`, name/sector from `companies`). Each metric is its own as-of date.
- **`returns`** — one row/ticker: `ret_1w/1m/3m/6m/ytd/1y/3y` (% close change, nearest trading day on/before each target) + `cagr_3y` annualized. Powers "which stock did ~20%/yr".
- **`broker_net`** — one row per (ticker, date, broker_code): `buy_value, sell_value, net_value, buy_volume_lot, sell_volume_lot, net_volume_lot`. Base for flip / accumulation / market-maker analysis.

## Milestones

### M0 — Contract rewrite (docs only; gates code)
- [ ] Rewrite `05-data-contract.md` to the **12 lean renamed tables** above + the 3 views; drop the raw-tier / 4-tier model entirely (no `04-raw-tier.md`). Document: `broker_distribution` explode, `announcements.attachments`, `ownership` ≥1%, per-table coercion landmines, and the "add later" deferrals (keystats 90-metric tree, json matrices, profile explodes, <1% holders, extra yf fields).
- [ ] Update `04-architecture.md` §Datasets (loaded serving DB + views, no raw tier) and `06-sources.md` (map each source → renamed table). Update `01-index.md`.
- **Verify:** no "raw tier" / 4-tier references remain; markdown renders.

### M1 — Static catalog (`src/catalog.rs`)
- [ ] `DatasetDoc`/`ColumnDoc` for the 12 tables + 3 views (transcribed from `05-data-contract.md`); `ALLOWED_TABLES` (the 12 + 3 view names); `describe_json()`.
- [ ] Tests: no dup names; `DATASETS ⊆ ALLOWED_TABLES`; `describe_json()` serializes.

### M2 — Sandboxed analytics engine (`src/analytics.rs`) — buildable NOW on `./data`
- [ ] Add `sqlparser`. `Store`: add `is_remote` (replay the R2 secret in the loader) + `existing_datasets()`.
- [ ] **Loader** (trusted RW conn): `CREATE TABLE` per renamed Parquet; `CREATE VIEW latest / returns / broker_net` over the loaded tables; skip-missing datasets.
- [ ] **Locked RO serving conn:** `Config::default().access_mode(ReadOnly).enable_external_access(false).max_memory("2GB").threads(2)` + disable extension autoload/install + `disabled_filesystems`; `SET lock_configuration=true` **last**. (Verify exact duckdb-1.105 `Config` setter names — some force a value / take no arg.)
- [ ] **Validator** (`sqlparser` AST): one statement; top node `Query` starting `SELECT`/`WITH`; reject DDL/DML/COPY/ATTACH/INSTALL/PRAGMA/CALL + file/network funcs; `FROM`/`JOIN` targets ∈ `ALLOWED_TABLES`; auto-wrap outer `LIMIT` (cap 5000, `truncated` flag).
- [ ] **Guarded exec:** `spawn_blocking` + `tokio::time::timeout(15s)` + `InterruptHandle::interrupt()`; param-bound `read_only_query_json` for shortcuts/screen.
- [ ] **Refresh:** versioned `serving-<epoch>.duckdb` + `RwLock<ConnState>` swap + delete-old; trigger on `SIGHUP` / `idx-mcp refresh`.
- **Verify:** clippy clean; negative tests `Err` (`read_parquet(...)`, `INSTALL`, `ATTACH`, multi-statement, unknown table, `COPY`); a query after a forced timeout still works (lock released).

### M3 — Wire tools onto the serving conn (`server.rs`, `main.rs`)
- [ ] `IdxServer` holds `Arc<Analytics>`; **migrate the 6 shortcuts** off `read_parquet` onto the serving conn (param-bound).
- [ ] Add `#[tool] run_query(sql, limit?)` + `#[tool] describe_schema(dataset?)`; `get_info` instructions: "start with `describe_schema`; prefer `run_query` for analytical/derived; `latest`/`returns`/`broker_net` are pre-built."
- [ ] `main.rs`: `mod catalog; mod analytics;`; build `Analytics` at boot (fail fast), log per-dataset load summary; SIGHUP handler.
- **Verify:** boots on `./data`, logs load summary; `tools/list` shows the new tools; a cross-table `run_query` returns rows; a forbidden query 400s.

### M4 — screen_stocks + docs
- [ ] `screen_stocks`: `filters:[{field,op,value}]` (ANDed) + `sort` + `limit` over the `latest` view; `SCREEN_FIELDS` allowlist (`market_cap, trailing_pe, forward_pe, price_to_book, dividend_yield, return_on_equity, profit_margins, revenue_growth, earnings_growth, debt_to_equity, beta, close, volume, rsi_14, free_float_pct, sector`); ops `= != < <= > >= between` (num) / `= in` (sector); field+op exact-matched, values bound.
- [ ] Tool docs (run_query, describe_schema, screen_stocks) + reframe the 6 shortcut pages ("use run_query for derived"); update `01-index.md`.
- **Verify:** `tools/list` shows 9; a screen + a returns query return sane rows; injection on field/op rejected.

### M5 — Dev ETL extension (`src/bin/etl.rs`) — parallel with M1–M4
- [ ] Ensure the ETL emits the 12 lean renamed tables correctly — **new work:** `broker_distribution` edge explode (`by_value`⋈`by_volume`), keep `announcements.attachments`, `ownership` ≥1% filter. (The 3 views are loader-side, not ETL.)
- [ ] Per-dataset on-disk size in the OK line.
- **Verify:** clippy `--all-targets` clean; smoke the renamed-table statement set.

### M6 — CI-green + e2e
- [ ] Exact CI gates (fmt --check; clippy --all-targets -D warnings pedantic; test); fix pedantic on new modules.
- [ ] E2E on a live server: `describe_schema` returns catalog; cross-table `run_query` returns rows; a forbidden query rejected; `screen_stocks` works; a `returns`/`broker_net` query works.

## Sequencing

**M0 gates code.** Then **Track A** M1→M2→M3→M4 in parallel with **Track B** M5 (depends only on M0). **M6 last.** M2/M3 are buildable and testable **now** against `./data`. OAuth runs as a separate later track.

## Deferred (post-v1)

Financials tier (XBRL/API) · `filings_text` + PDF download/extract · `get_financials` / `compute_financial_ratio` / `get_filing` · RAG · OAuth 2.1 + accounts · ETL-triggered/scheduled refresh · per-tier `run_query` gating (when billing lands) · the "later" columns (keystats 90-metric tree, json matrices, profile explodes, <1% holders, extra yf fields).

## Risks

- **Sandbox-config correctness** (top risk) — verify each duckdb-1.105 `Config` setter; `lock_configuration` must be the **last** `SET`.
- **No DuckDB statement timeout** → external `spawn_blocking` + `timeout` + `interrupt`; ensure the task joins and releases the lock.
- **Refresh file-handle lifecycle** → versioned files + `RwLock` swap (no rename-over).
- **`screen_stocks` injection** → field/op exact-membership allowlist; values bound.
- **`broker_distribution` explode** → the `by_value`⋈`by_volume` per-edge join is the trickiest ETL transform; verify against a real doc.
- **`returns` trading-day alignment** → "nearest trading day on/before target" needs care (holidays, thin history); NULL when history too short.
- **`describe_schema` staleness** → hybrid (auto columns + hand semantics).
- **Strict CI** on two new modules + nested schemars structs.

## Effort

~**3–5 focused days** in-repo (M0–M6) — smaller than the original scope now that the raw tier, financials, and filings are out. M2 (the sandbox) is ~1.5–2d and the hard part; the rest ~0.5d each. Excludes the external ETL.
