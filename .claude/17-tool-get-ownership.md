> Docs index: see [the index](01-index.md) to discover all pages.

# get_ownership

> KSEI depository holders (≥1%) for an IDX ticker, with local/foreign split.

### Overview

The significant shareholders of one ticker, from KSEI (Indonesia's central securities depository). Returns one row per holder with at least 1% of shares — name, holder type, classification, local/foreign, share count, and percentage. Use it for ownership concentration, free-float context, and "who controls X".

### Parameters

* `ticker` (string, required) — IDX ticker symbol, e.g. `BBCA`.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_ownership", "arguments": { "ticker": "BBCA" } }
}
```

### Example Response

```json
[
  {"date": "2026-02-27", "name": "PT DWIMURIA INVESTAMA ANDALAN", "type": "CP", "classification": null, "local_foreign": "LOCAL", "total_shares": 67729950000, "percentage": 54.94},
  {"date": "2026-02-27", "name": "ANTHONI SALIM", "type": "ID", "classification": null, "local_foreign": "LOCAL", "total_shares": 1416306835, "percentage": 1.15}
]
```

### Notes

Holders below 1% are excluded. KSEI is a depository registry (actual local/foreign and institution/individual composition) — it is **not** US-style 13F/Form-4 insider data, so coverage and cadence differ (monthly snapshot, latest per ticker). Sorted by `percentage` descending.
