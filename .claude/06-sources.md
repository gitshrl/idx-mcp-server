# Sources

Where the raw data comes from, and the original collection inventory. The **served** datasets and their column contract are in `05-data-contract.md`.

## Upstream sources

- **Vendor feed** (third-party retail trading-platform data) — keystats, stockprofiles, marketdetectors, orderbook, brokerdistribution, tradebook, brokermaster, grwbrokeractivity
- **IDX** — Indonesia Stock Exchange, official — idxstocksummary, idxbrokersummary, idxannouncement, idxnewsannouncement
- **KSEI** — central securities depository — kseiownership, kseishareholdercomposition, ksei5pctownership
- **Yahoo Finance** — yfdaily, yfindicators, yfsummary, yfanalyst

## MongoDB (dev source)

The dev ETL reads a MongoDB. **Real connection details are not stored here** — host, database, and credentials live in `.env` / local only:

```
mongodb://<user>:<pass>@<host>:27017/<db>?authSource=admin
```

A copy of the data also exists in a local `mongod`.

## Raw collection inventory (19 → 11 served)

| Collection | Docs | Source | Served as |
|---|--:|---|---|
| keystats | 87,113 | Vendor | `fundamentals` |
| stockprofiles | 86,156 | Vendor | `companies` |
| marketdetectors | 88,076 | Vendor | `broker_activity` (2026, combined with grwbrokeractivity) |
| orderbook | 87,113 | Vendor | — dropped |
| brokerdistribution | 89,033 | Vendor | `broker_distribution` |
| tradebook | 79,439 | Vendor | — dropped |
| brokermaster | 90 | Vendor | — dropped |
| grwbrokeractivity | 228,723 | Vendor | `broker_activity` (2025, combined with marketdetectors) |
| idxstocksummary | 314,038 | IDX | `eod_summary` |
| idxbrokersummary | 29,773 | IDX | `broker_rankings` |
| idxannouncement | 60,277 | IDX | `announcements` |
| idxnewsannouncement | 28,979 | IDX | `announcements` |
| kseiownership | 956 | KSEI | `ownership` (≥1%) |
| kseishareholdercomposition | 957 | KSEI | — dropped |
| ksei5pctownership | 819 | KSEI | — dropped |
| yfdaily | 505,490 | Yahoo Finance | — dropped (no longer served; `eod_summary` from idxstocksummary is now the price source) |
| yfindicators | 505,488 | Yahoo Finance | `indicators` |
| yfsummary | 955 | Yahoo Finance | `summary` |
| yfanalyst | 957 | Yahoo Finance | `analyst` |

Counts are original estimates; exact `find()` counts at ETL time differ slightly. Dropped collections are redundant for the served API.
