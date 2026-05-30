# Implementation Plan — Complex Q&A + Full Data Coverage

> Roadmap for making the server answer open-ended analytical questions over **all** the data. Companion to `01-architecture.md` (design), `02-data-contract.md` (the data spec), `llms.txt` (index). OAuth 2.1 + accounts is a **separate, parallel** milestone (see `01-architecture.md` §Auth) — not covered here.

**Goal:** Let an agent answer anything the data can answer — cross-sectional screens, rankings, comparisons, broker-flow/bandarmology, backtests, fundamentals, and filing content — via a flexible SQL tool, with all data queryable and untrusted SQL sandboxed.

**Tech:** Rust · rmcp 1.7 · axum 0.8 · DuckDB 1.105 (bundled) · `sqlparser` (SQL validation) · edition 2024 · strict CI (fmt + clippy all+pedantic `-D warnings` + tests).

---

## Decisions (resolved via grilling — these shape the plan)

1. **Build order:** analytics layer first; OAuth is a separate parallel track.
2. **Query path — unify:** all tools query **one loaded, locked read-only DuckDB serving DB** (not live `read_parquet`). One schema, one security surface; partitioning becomes ETL-only.
3. **No duplication:** the ETL writes **only the raw tier**; the **12 curated datasets + combinations are VIEWS over the raw tables** in the serving DB. Raw is the single source of truth. (Resolves the raw-twin storage-dup risk.)
4. **Docs layer:** crawl → **download PDFs to R2** → extract `filings_text` (queryable). **RAG deferred** (metadata locates the doc; extract + read). Add a vector/RAG layer later only for cross-corpus semantic search.
5. **Financials:** structured **XBRL/API** for *numbers* (deterministic), PDFs/`filings_text` for *prose*.
6. **`run_query` gating:** **flat now** (any valid key → full access); the loaded-DB model makes per-tier gating a cheap later add when billing lands.
7. **`describe_schema`:** **hybrid** — columns/types auto-introspected from the live serving DB at load, semantics/landmines overlaid from the checked-in catalog (can't drift on columns).
8. **Limits:** per-query external timeout (`spawn_blocking` + `interrupt`, ~15s) + `max_memory`/`threads` caps + small concurrency cap; **freshness = daily rebuild + atomic swap** of the serving file (matches the daily data cadence).

## Data & query model

```
EXTERNAL ETL (separate repo) ──writes Parquet──►  raw_<coll> (all 19, full schema, nested JSON, canonical ticker/date)
                                                  financials_{income,balance,cashflow}, financial_facts   (XBRL/API)
                                                  filings_text  (extracted from R2 PDF corpus)
        │
        ▼  boot + daily refresh
  analytics.rs LOADER (trusted RW conn): CREATE TABLE per Parquet dataset → serving-<epoch>.duckdb
                                         CREATE VIEW for the 12 curated datasets + combinations over raw
        │
        ▼  open SEPARATE read-only LOCKED serving conn (Config fixed, lock_configuration last)
  ALL tools query the serving DB:
    shortcuts / screen_stocks / get_financials / compute_financial_ratio / get_filing  → param-bound SELECTs on views/tables
    run_query  → sqlparser-validated single SELECT over the same tables/views
```

The dev `src/bin/etl.rs` is the local loader; the **data-contract is the spec the external ETL meets**. The MCP server stays **egress-free** — all Mongo export, XBRL ingest, and PDF download/extract live in the external ETL.

## Tools (target = 11 + 1 deferred)

Shortcuts (reframed "use `run_query` for analytical/derived"): `search_tickers`, `get_company`, `get_prices`, `get_broker_activity`, `get_ownership`, `get_announcements` (+`attachments`). Flexible core: **`run_query`**, `describe_schema`, `screen_stocks`. Financials: `get_financials`, `compute_financial_ratio`. Deferred: `get_filing` (reads `filings_text`).

---

## Milestones

### M0 — Data contract & raw-tier spec (docs only; gates everything)
- [ ] `04-raw-tier.md`: naming (`raw_<collection>`, no collision with curated), per-collection partitioned-vs-`latest` layout (the 6 dateless/snapshot collections → `latest.parquet`), schema rule (strip only `_id`; add canonical `ticker`/`date`; keep all fields incl nested JSON), and the **allowlist** (raw + curated + financials + filings_text names).
- [ ] Rewrite `02-data-contract.md` into 4 tiers — **RAW** (all 19, incl. the 5 previously-dropped, every field + nested), **CURATED** (the 12 + combinations, now defined as **views over raw**; add `attachments` to announcements), **FINANCIALS** (income/balance/cashflow + `financial_facts`: columns, grain `ticker+period_end+fiscal_period`, XBRL source), **FILINGS_TEXT** (`ticker, date, announcement_no, pdf_path, page, text, method`). Preserve all coercion/landmine notes.
- [ ] Update `03-sources.md` (every collection → `raw_<coll>`; no "dropped"; add XBRL + PDF-extraction provenance) and `01-architecture.md` §Datasets (the layered warehouse).
- **Verify:** `grep -c dropped 03-sources.md == 0`; allowlist has no name in both raw and curated; markdown renders.

### M1 — Static catalog (`src/catalog.rs`)
- [ ] `DatasetDoc`/`ColumnDoc` static tables transcribed from `02-data-contract.md`; `ALLOWED_TABLES` (union of dataset names); `describe_json()`.
- [ ] Unit tests: no dup table names; `DATASETS ⊆ ALLOWED_TABLES`; `describe_json()` serializes.
- **Verify:** `cargo test catalog`; clippy `-D warnings` clean.

### M2 — Sandboxed analytics engine (`src/analytics.rs`) — buildable NOW on current `./data`
- [ ] Add `sqlparser`; `Store`: add `is_remote` (Local vs R2, to replay the R2 secret in the loader) + `existing_datasets()`.
- [ ] **Loader** (trusted RW conn, external access ON): `CREATE TABLE` per raw/financials/filings Parquet; `CREATE VIEW` for the 12 curated datasets + combinations over raw; skip-missing datasets (so it runs before the external ETL lands financials/filings).
- [ ] **Locked RO serving conn:** `Config::default().access_mode(ReadOnly).enable_external_access(false).max_memory("2GB").threads(2)` + disable extension autoload/install + `disabled_filesystems`; `SET lock_configuration=true` **last**. (Verify exact duckdb-1.105 `Config` method names — e.g. `allow_unsigned_extensions` forces true with no arg.)
- [ ] **Validator** (`sqlparser` AST): exactly one statement; top node is `Query` starting `SELECT`/`WITH`; reject DDL/DML/COPY/ATTACH/INSTALL/PRAGMA/CALL + file/network funcs; `FROM`/`JOIN` targets ∈ `ALLOWED_TABLES`; auto-wrap outer `LIMIT` (cap 5000).
- [ ] **Guarded exec:** `spawn_blocking` + `tokio::time::timeout(15s)` + `InterruptHandle::interrupt()`; row cap; a **param-bound** `read_only_query_json` (for `screen_stocks`/financials/shortcuts).
- [ ] **Refresh/swap:** versioned `serving-<epoch>.duckdb` + `RwLock<ConnState>` swap + delete-old (RO conn holds the file → rename-over fails; versioned files avoid it).
- **Verify:** clippy clean; negative tests Err (`read_parquet(...)`, `INSTALL`, `ATTACH`, multi-statement, unknown table, `COPY`); a follow-up query works after a forced timeout (Mutex released).

### M3 — Wire flexible tools + migrate shortcuts to the serving conn (`server.rs`, `main.rs`)
- [ ] `IdxServer` gains `analytics: Arc<Analytics>`; **all 6 shortcuts** now query the serving conn (param-bound) instead of `store.rs` `read_parquet` (the unify decision); `store.rs` becomes the loader owner.
- [ ] Add `#[tool] run_query(sql, limit?)` and `#[tool] describe_schema(dataset?)`; `get_info` instructions: "start with `describe_schema`; prefer `run_query` for analytical/derived."
- [ ] `main.rs`: `mod catalog; mod analytics;`; build `Analytics` at boot (fail fast), log per-dataset load summary.
- **Verify:** clippy clean; server boots on `./data`, logs load summary; `tools/list` shows the new tools; a real cross-dataset `run_query` returns rows; a forbidden query 400s.

### M4 — Remaining tools + docs
- [ ] `screen_stocks` (typed filters → param-bound SQL over curated views; `SCREEN_FIELDS` allowlist), `get_financials`, `compute_financial_ratio` (fixed `RATIOS` set, `NULLIF` every denominator), `get_filing` (reads `filings_text`; deferred in description), `get_announcements` + `attachments`.
- [ ] New tool pages under `.claude/tools/` (run_query, describe_schema, screen_stocks, get_financials, compute_financial_ratio, get_filing) + reframe the 6 shortcut pages; update `llms.txt`.
- **Verify:** clippy/tests clean; `tools/list` shows 11 (+deferred); `screen_stocks` + a ratio return sane rows; injection attempts on field/ratio strings rejected.

### M5 — Dev ETL extension (`src/bin/etl.rs`) — parallel with M1–M4
- [ ] Runner already statement-agnostic; confirm raw `latest.parquet` snapshots covered; add per-dataset on-disk **size** to the OK line (`unwrap_or(0)` on metadata); update the module doc (now builds raw + financials + filings_text; spec in `02-data-contract.md`).
- **Verify:** clippy `--all-targets` clean; smoke a minimal raw partitioned + `latest` statement set.

### M6 — CI-green + end-to-end
- [ ] Run the exact CI gates (fmt --check; clippy --all-targets -D warnings pedantic; test); fix pedantic on the new modules.
- [ ] E2E smoke on a live server: `describe_schema` returns catalog; cross-dataset `run_query` returns rows; a forbidden query is rejected; `screen_stocks` works; document the catalog↔data gap (catalog advertises financials/filings tables that `run_query` rejects as UnknownTable until the external ETL lands them).
- **Verify:** all CI gates green; 5-case E2E passes.

## Sequencing

**M0 gates everything** (the frozen spec). Then two parallel tracks: **Track A (code)** M1→M2→M3→M4; **Track B (dev ETL)** M5 (depends only on M0). **M6 gates merge.** Key leverage: **M2/M3 are buildable and testable NOW** against the current `./data` — the analytics engine doesn't wait on the external ETL. OAuth + accounts runs as a fully separate parallel milestone.

## Critical risks

- **Sandbox-config correctness** is the top risk — verify each duckdb-1.105 `Config` setter against the crate (some force values / take no arg); `lock_configuration` must be the **last** `SET`, all caps applied via `Config` before open.
- **No DuckDB statement timeout** → external `spawn_blocking` + `timeout` + `interrupt`; ensure the task joins and releases the serving Mutex.
- **Refresh file-handle lifecycle** → versioned files + `RwLock` swap (no rename-over); a leaked read guard leaks a file.
- **Injection via typed tools** → `screen_stocks`/`compute_financial_ratio` field/op/ratio strings must be **exact-membership** against fixed allowlists; values bound as params.
- **`describe_schema` staleness** → hybrid (auto columns + hand semantics) + M6 documents/filters the catalog↔data gap.
- **Strict CI on new surface** (two new modules + nested schemars structs) → every milestone verifies clippy pedantic.

## Open questions (need your input)

1. **External-ETL ownership/timeline** — who builds the generator emitting `raw_*` + XBRL financials + PDF-extracted `filings_text`? `get_financials`/`compute_financial_ratio`/`get_filing` are inert until it lands (the engine ships now regardless).
2. **Financials column set** — defined from the contract, not a real XBRL sample. A bank + non-bank sample filing would de-risk bank-vs-nonbank field divergence and cumulative-vs-quarterly conventions.
3. **Storage budget** — raw tier ~0.9–1.2 GB (before financials/`filings_text`, which is unbounded). OK on local + R2?
4. **Limit defaults** — timeout 15s / row cap 5000 / `max_memory` 2GB / threads 2 — fixed or env-configurable?
5. **Refresh trigger** — manual `idx-mcp refresh` / SIGHUP for v1, scheduler later — acceptable staleness between rebuilds?
6. **`sqlparser` dialect** — `GenericDialect` vs `DuckDbDialect` for DuckDB syntax (QUALIFY, etc.) — test which parses the real query mix.

## Effort

~**5–8 focused engineering days** for the in-repo work (M0–M6), excluding the external ETL (separate system). M0 ~1–1.5d (the contract rewrite is the bulk); M2 ~2d (sandbox is the hard part); the rest ~0.5–1d each.
