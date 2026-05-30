# IDX MCP — Field Map & Data Contract

Derived from real sample documents in the source MongoDB (one `findOne()` per collection) plus an adversarial review. This is the contract the **external ETL** implements when producing Parquet, and the **server** reads. One sample per collection ⇒ optional/variant fields may exist; validate against a full-collection profile before freezing (see §Validation).

## Served dataset names

The per-dataset sections below are keyed by **source collection** name; the **served** dataset uses the source-neutral name (lineage stripped). Mapping:

| Served dataset | ← Source collection(s) |
|---|---|
| `prices` | yfdaily |
| `eod_summary` | idxstocksummary |
| `indicators` | yfindicators |
| `fundamentals` | keystats |
| `summary` | yfsummary |
| `analyst` | yfanalyst |
| `companies` | stockprofiles (latest/ticker) |
| `ownership` | kseiownership (investors[] exploded, ≥1%) |
| `broker_activity` | grwbrokeractivity (exploded; marketdetectors enrichment optional) |
| `broker_distribution` | brokerdistribution |
| `broker_rankings` | idxbrokersummary |
| `announcements` | idxannouncement ∪ idxnewsannouncement |

## Conventions

- **Canonical keys.** Every served dataset exposes `ticker` (VARCHAR, UPPERCASE, no `.JK` suffix) and, for time-series, `date` (DATE). The ETL renames the source field (`stock_code` / `StockCode` / `Code` / `ticker` → `ticker`; `date` / `Date` / `report_date` / `date_current` → `date`). The server filters uniformly on `ticker`/`date` — no per-collection key branching.
- **Snapshots vs time-series.** `stockprofiles`, `yfsummary`, `yfanalyst`, `kseiownership` are **latest-per-ticker** snapshots. For `yfsummary`/`yfanalyst` the only timestamp is `updated_at` (a refresh stamp) — a date-range filter is meaningless; serve latest only.
- **`updated_at`** is the one true BSON `$date` in idx*/ksei*; most other dates are plain `'YYYY-MM-DD'` strings → cast to DATE.
- Columns marked **json** keep a nested subtree verbatim (DuckDB `JSON`/`VARCHAR`). Columns marked **explode** become their own child rows (grain noted).

## Cross-cutting ETL coercion rules (apply at the I/O edge)

The vendor collections (`keystats`, `stockprofiles`, `marketdetectors`, `tradebook`) store most numbers as **strings**; IDX/KSEI/Yahoo are mostly native numbers. A single shared coercion layer must handle:

1. **Thousands commas:** `'79,251 B'`, `'1,509.11'`, `'385,583'` → strip `,`.
2. **Magnitude suffix:** `B`/`M`/`K` → ×1e9/1e6/1e3 (e.g. `'7.79 B'` → 7_790_000_000).
3. **Accounting parens = negative:** `'(6,191 B)'` → -6.191e12; `'(-345)'`.
4. **Scientific notation strings:** `'8.69187e+07'`, sell-side `'-1.390475e+08'`.
5. **Leading `+`:** `'+4.00'`.
6. **Percent convention (worst landmine):** store ONE canonical form = **numeric percent** (`18.81` means 18.81%). String `'%'` fields (`'18.81%'`, `'40.47%'`) → strip `%`. Yahoo native fractions (`0.21`, margins/growth/yields) → ×100. Document per-field.
7. **Null sentinels:** `'-'`, `''`, `'NA'`, empty domicile, `'0001-01-01T00:00:00Z'` zero-date → NULL.
8. **Dates:** ISO `'YYYY-MM-DD'` string → DATE; `marketdetectors.netbs_date` `'YYYYMMDD'` → DATE; dividend `'18 Nov 25'` (`dd Mon yy`) and IPO `'2 Jan 2026'` → DATE (mind 2-digit year window); naive WIB datetime strings (`CreatedDate`, `TglPengumuman`, `PublishDate`) → TIMESTAMP (assume Asia/Jakarta).
9. **Foreign/local enum** → canonical `{LOCAL, FOREIGN, GOVERNMENT, UNKNOWN}`: `'Lokal'/'Asing'/'Pemerintah'` (brokermaster, brokerdistribution), domicile `'I'/'A'/'B'` (grwbrokeractivity), `'L'/'A'` (kseiownership).
10. **Dedup snapshots** on `(ticker, date)`: prefer `max(updated_at)` where present (idx*/ksei*); for vendor snapshots define a deterministic tiebreaker (`max(_id)`). Single-sample grain uniqueness is **unverified** — confirm.
11. **`_id`** (`$oid`) is dropped everywhere; `ticker`+`date` (or natural key) is the row identity.

---

## Time-series datasets

### `yfdaily` — daily OHLCV (primary price source) · tool: `get_prices`
Grain: one row per `ticker` + `date`. Clean native numbers.

| column | type | source |
|---|---|---|
| ticker | VARCHAR | `ticker` |
| date | DATE | `date` |
| open, high, low, close | DOUBLE | `open`/`high`/`low`/`close` |
| volume | BIGINT | `volume` |
| dividends, splits | DOUBLE | `dividends`/`splits` (0 = none) |

### `idxstocksummary` — official EOD summary · tool: `get_prices` (`source=idx`)
Grain: one row per `ticker` + `date`. Source keys `StockCode`/`Date` (PascalCase).

| column | type | source |
|---|---|---|
| ticker | VARCHAR | `StockCode` |
| date | DATE | `Date` |
| stock_name | VARCHAR | `StockName` |
| open/high/low/close/previous/change/first_trade | DOUBLE | `OpenPrice`/`High`/`Low`/`Close`/`Previous`/`Change`/`FirstTrade` |
| volume/value/frequency | BIGINT | `Volume`/`Value`/`Frequency` |
| bid/offer | DOUBLE | `Bid`/`Offer` |
| bid_volume/offer_volume | BIGINT | `BidVolume`/`OfferVolume` |
| foreign_buy/foreign_sell | BIGINT | `ForeignBuy`/`ForeignSell` |
| listed_shares/tradeable_shares/weight_for_index | BIGINT | `ListedShares`/`TradebleShares`(sic)/`WeightForIndex` |
| index_individual | DOUBLE | `IndexIndividual` |
| non_regular_volume/value/frequency | BIGINT | `NonRegular*` |
| remarks | VARCHAR | `Remarks` |
| delisting_date | DATE | `DelistingDate` (often `''` → NULL) |
| updated_at | TIMESTAMP | `updated_at` ($date) |

### `yfindicators` — daily technicals · tool: `get_indicators`
Grain: `ticker` + `date`. All native doubles; nullable during warmup. f32→f64 precision noise — round at presentation.

`ticker, date` + DOUBLE: `atr_14, bb_lower, bb_mid, bb_upper, change, change_5d, change_20d, ema_12, ema_26, ema_50, high_20d, low_20d, macd, macd_hist, macd_signal, rsi_14, sma_5, sma_10, sma_20, sma_50, sma_200, vol_ratio, vol_sma_20, vwap`.

### `keystats` — fundamentals/valuation snapshot · tool: `get_company`
Grain: one row per `ticker` + `date` (daily snapshot). Heavy string-numeric coercion.

| column | type | source |
|---|---|---|
| ticker | VARCHAR | `stock_code` |
| date | DATE | `date` |
| market_cap | DOUBLE | `stats.market_cap` (`'54,898 B'`) |
| enterprise_value | DOUBLE | `stats.enterprise_value` |
| shares_outstanding | DOUBLE | `stats.current_share_outstanding` (`'7.79 B'`) |
| free_float_pct | DOUBLE | `stats.free_float` (`'18.81%'` → 18.81) |
| latest_dividend | DOUBLE | `dividend_group.dividend_year_values[-1].dividend` |
| latest_dividend_year | BIGINT | `…[-1].period` |
| latest_dividend_ex_date / _payment_date | DATE | `…[-1].ex_date` / `.payment_date` (`'18 Nov 25'`) |
| financial_report_currency | JSON | array e.g. `['IDR','USD']` |
| keystats_raw | JSON | `closure_fin_items_results` (full 90-metric tree, verbatim) |
| financial_history_raw | JSON | `financial_year_parent` (quarterly chart payload) |

**Explode (companion table `keystats_metrics`)**, grain `(ticker, date, fitem_id)`: from `closure_fin_items_results[].fin_name_results[]` → `ticker, date, block (=keystats_name), fitem_id, metric_name (=fitem.name), value_raw (VARCHAR), value_num (DOUBLE)`. Join/pivot on **`fitem_id`** (stable, ~90 unique), NOT display name. Drop UI flags `hidden_graph_ico`/`is_new_update`. Do NOT hard-code 90 wide columns — the block/metric set varies by ticker. `value_num` parsing must handle percent / parens-negative / B-M-K / commas / embedded dates / `'-'`→NULL.

### `brokersummary` — broker flow + bandar signal · tool: `get_broker_activity`
**Combined** from `grwbrokeractivity` ⋈ `marketdetectors` on `(stock_code, date)`. Two grains; serve as two related shapes:

**(a) broker-level rows** (primary) — from `grwbrokeractivity.buy[]`/`sell[]` (and the equivalent `marketdetectors.broker_summary.brokers_buy/sell[]`), grain `(ticker, date, broker_code, side)`:
`ticker, date, broker_code, side (B|S), volume_lot BIGINT, value BIGINT, frequency BIGINT, avg_price DOUBLE, domicile {LOCAL|FOREIGN|GOVERNMENT}`. Union buy+sell into one table with `side`. **Field-name trap:** grw uses `total_volume/total_transaction/transaction_frequency/average_transaction`; marketdetectors uses `blot/blotv/bval/bvalv` (buy) vs `slot/slotv/sval/svalv` (sell) — map both to the unified names. Sell magnitudes are signed-negative strings; normalize sign by `side`. `grw.transaction_frequency` is 0 in every sample row — treat as suspect/nullable.

**(b) bandar-detector signal** (per `ticker, date`) — from `marketdetectors.bandar_detector`:
`ticker, date, accdist VARCHAR (broker_accdist), num_broker_buysell BIGINT, total_buyer BIGINT, total_seller BIGINT, net_value DOUBLE (avg.amount), net_percent DOUBLE (avg.percent), net_vol DOUBLE (avg.vol), value DOUBLE, volume BIGINT, average_price DOUBLE`. Keep `avg5/top1/top3/top5/top10` as **json**. `get_broker_activity` returns the broker rows + this signal summary for the (ticker, date) range.

### `brokerdistribution` — broker-to-broker distribution graph · tool: `get_broker_activity`
Grain: one row per `ticker` + `date`. The graph is nested; keep as json for v1.

| column | type | source |
|---|---|---|
| ticker | VARCHAR | `stock_code` |
| date | DATE | `date` (ignore equal `date_info`/`start_date`/`end_date`) |
| by_value | JSON | `by_value` — `{top_broker_buy[], top_broker_sell[]}`, each `{detail:{code,type,amount}, distribute_to:[{code,type,amount}]}` (rupiah) |
| by_volume | JSON | `by_volume` — identical shape, amounts in lots |

Future child table `broker_distribution_edges` (grain `ticker,date,side,source_code,counterparty_code`) by exploding `distribute_to[]` — the counterparty edges that make the graph.

### `idxbrokersummary` — market-wide broker league table · tool: `get_broker_summary`
Grain: one row per **broker** + `date` — **NO ticker**. Tools must reject ticker filtering here.

| column | type | source |
|---|---|---|
| broker_code | VARCHAR | `IDFirm` |
| firm_name | VARCHAR | `FirmName` |
| date | DATE | `Date` |
| frequency/value/volume | BIGINT | `Frequency`/`Value`/`Volume` |
| rank_no | BIGINT | `No` |
| updated_at | TIMESTAMP | `updated_at` ($date) |

### `announcements` — corporate disclosures + news · tool: `get_announcements`
**Combined** = `idxannouncement` ∪ `idxnewsannouncement`, normalized to a common schema. Grain: one row per announcement (NOT one per ticker+date — many per ticker per day). Add `source` discriminator. **Locale-twin risk:** `idxannouncement.Id2` has an `_id-id` suffix implying an `_en-us` duplicate — dedup on the announcement number / normalized id.

| column | type | idxannouncement | idxnewsannouncement |
|---|---|---|---|
| ticker | VARCHAR | `stock_code` | `Code` |
| date | DATE | `Date` | `Date` |
| source | VARCHAR | `'announcement'` | `'news'` |
| title | VARCHAR | `JudulPengumuman` | `Title` |
| subject | VARCHAR | `PerihalPengumuman` | — |
| announcement_type | VARCHAR | `JenisPengumuman` | `Jenis` |
| announcement_no | VARCHAR | `NoPengumuman` | `AnnouncementNo` |
| announced_at / published_at | TIMESTAMP | `TglPengumuman` | `PublishDate` |
| created_at | TIMESTAMP | `CreatedDate` | — |
| updated_at | TIMESTAMP | `updated_at` ($date) | `updated_at` ($date) |
| attachments | JSON | `Attachments`(absent) | `Attachments` (`{PDFFilename, FullSavePath, IsAttachment:0=primary}`) |

---

## Snapshot datasets (latest per ticker)

### `stockprofiles` — company profile · tools: `search_tickers`, `get_company`
Dedupe to latest per ticker (source has daily history). Source `stock_code`/`date`; profile fields nested under `info` and `profile`.

| column | type | source |
|---|---|---|
| ticker | VARCHAR | `stock_code` |
| date | DATE | `date` |
| company_name | VARCHAR | `info.name` |
| sector / sub_sector | VARCHAR | `info.sector` / `info.sub_sector` |
| exchange / country / status | VARCHAR | `info.exchange` / `info.country` / `info.status` |
| is_tradeable | BOOLEAN | `info.tradeable` |
| instrument_type | VARCHAR | `info.type_company` |
| price / previous_price / change / change_percent | DOUBLE | `info.price` / `info.previous` / `info.change` / `info.percentage` (strings → num) |
| volume | BIGINT | `info.volume` |
| average_price / value | DOUBLE | `info.average` / `info.value` |
| followers | BIGINT | `info.followers` |
| haircut_percent / margin_percent | DOUBLE | `info.trading_limit_info.haircut_percentage` / `info.margin_info.percentage_raw` |
| company_background | VARCHAR | `profile.background` |
| ipo_date / ipo_price / listing_board / free_float_percent | DATE/DOUBLE/VARCHAR/DOUBLE | `profile.history.*` |
| contact_email / secretary_name | VARCHAR | `profile.address` / `profile.secretary` (inner JSON-in-string — parse) |
| indexes | JSON | `info.indexes` |
| key_executive / listing_information | JSON | `profile.key_executive` / `profile.listing_information` |

Explode candidates (companion tables): `profile.shareholder[]` → ownership rows; `profile.subsidiary[]`; `profile.shareholder_numbers[]` (monthly holder counts). Drop fund-only empties (`profile.fee/asset_allocation/top_holdings/...`) and UI dupes (`info.catalogs`, `info.indexes_data`).

### `yfsummary` — Yahoo fundamentals snapshot · tool: `get_company`
Grain: one row per `ticker` (`date` = `updated_at` refresh stamp; latest only). Fully flat, native numbers. Columns: `ticker, date, name, business_summary, sector, industry, market_cap, enterprise_value, shares_outstanding, float_shares, avg_volume, avg_volume_10d, beta, book_value, price_to_book, price_to_sales, trailing_pe, forward_pe, peg_ratio, enterprise_to_revenue, enterprise_to_ebitda, trailing_eps, forward_eps, revenue, revenue_per_share, revenue_growth, earnings_growth, earnings_quarterly_growth, gross_margins, ebitda_margins, operating_margins, profit_margins, return_on_assets, return_on_equity, current_ratio, quick_ratio, debt_to_equity, total_debt, total_cash, total_cash_per_share, free_cash_flow, operating_cash_flow, dividend_rate, dividend_yield, payout_ratio, day_avg_50, day_avg_200, week_high_52, week_low_52, week_change_52, short_ratio, held_percent_insiders, held_percent_institutions, number_of_analyst_opinions, target_mean_price, target_high_price, target_low_price, recommendation_key`.

> **Data-quality flag:** Yahoo mixes IDR-scaled and USD-scaled fields for IDX names — valuation ratios (`forward_pe` 72790, `price_to_book` 19626, `enterprise_to_ebitda` 78005) and `revenue` vs `market_cap` are unreliable. `get_company` must NOT present these as authoritative; tag currency per field and mark `unreliable_ratio`. Margins/growth/yields are native fractions → ×100 to match the percent convention.

### `yfanalyst` — analyst estimates snapshot · tool: `get_analyst`
Grain: one row per `ticker` (latest). Columns: `ticker, date(updated_at), rec_strong_buy, rec_buy, rec_hold, rec_sell, rec_strong_sell, target_current, target_mean, target_median, target_high, target_low, eps_est_avg_cy (earnings_estimate.avg.0y), eps_est_avg_1y (.avg.+1y), eps_growth_1y (.growth.+1y), rev_est_avg_cy (revenue_estimate.avg.0y), rev_est_avg_1y (.avg.+1y)`. Keep `earnings_estimate`, `revenue_estimate`, `eps_trend`, `eps_revisions` as **json** (metric→period matrices).

### `kseiownership` — KSEI depository ownership · tool: `get_ownership`
**Filtered + exploded.** Monthly snapshot per issuer; serve latest per ticker.

**Summary row** (per `ticker`): `ticker, date, issuer_name, updated_at, total_holders, total_strategic_pct, free_float_pct, local_pct, foreign_pct, unclassified_pct` (from `summary.*`).

**Investor rows** (explode `investors[]`, **keep `percentage` ≥ 1% only** per the scope decision), grain `(ticker, date, investor)`: `name, type, classification, local_foreign {LOCAL|FOREIGN}, nationality, holdings_scripless, holdings_scrip, total_shares, percentage`. `get_ownership` returns the summary + the ≥1% holder rows. Note: holder list assumed complete (used for free-float math) — verify not top-N truncated.

---

## Tool → relation map (server)

The server loads each served dataset into one read-only serving database and adds the views `latest` (per-ticker snapshot), `returns` (trailing/annualized returns), `broker_net` (per-broker net flow). Datasets without a typed shortcut (`indicators`, `analyst`, `broker_distribution`, `broker_rankings`) are reached through `run_query`.

| tool | relations | notes |
|---|---|---|
| `run_query` | any table + `latest` / `returns` / `broker_net` | read-only SELECT; allowlisted + sandboxed |
| `describe_schema` | (catalog) | live tables/views + columns |
| `screen_stocks` | `latest` | typed cross-sectional filter/sort |
| `search_tickers` | `companies` | match ticker/company_name |
| `get_company` | `companies` + `fundamentals`(latest) + `summary` | flag yf ratio reliability |
| `get_prices` | `prices` (default) / `eod_summary` (`source=idx`) | |
| `get_broker_activity` | `broker_activity` | per-broker buy/sell rows |
| `get_ownership` | `ownership` | ≥1% holders |
| `get_announcements` | `announcements` | dedup locale twins |

## Validation before freezing (run a full-collection profile)

1. `(ticker, date)` uniqueness for the vendor snapshots (`keystats`, `stockprofiles`, `marketdetectors`) — single sample can't prove grain.
2. `idxannouncement` `_id-id`/`_en-us` locale-twin duplication → dedup rule.
3. KSEI `investors[]` is the full holder list, not top-N.
4. `idxbrokersummary` has no per-stock variant.
5. Confirm whether `keystats` dividend history is multi-period (then don't flatten only `[-1]`).
