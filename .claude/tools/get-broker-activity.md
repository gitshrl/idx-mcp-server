> Docs index: see [llms.txt](../llms.txt) to discover all pages.

# get_broker_activity

> Per-broker buy/sell activity (bandarmology) for an IDX ticker over a date range.

### Overview

Daily broker-attributed order flow for one ticker — one row per broker per side (buy/sell) per day, with volume, value, average price, and local/foreign domicile. This is the data US markets anonymize; it powers "bandarmology" (which brokers are accumulating or distributing).

### Parameters

* `ticker` (string, required) — IDX ticker symbol, e.g. `BBCA`.
* `from` (string, optional) — inclusive start date, `YYYY-MM-DD`.
* `to` (string, optional) — inclusive end date, `YYYY-MM-DD`.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_broker_activity", "arguments": { "ticker": "BBCA", "from": "2025-06-16", "to": "2025-06-16" } }
}
```

### Example Response

```json
[
  {"date": "2025-06-16", "broker_code": "ZP", "side": "B", "volume_lot": 160831, "value": 143958939289, "avg_price": 8950.9, "domicile": "FOREIGN"},
  {"date": "2025-06-16", "broker_code": "ZP", "side": "S", "volume_lot": 141479, "value": 126665629289, "avg_price": 8953.0, "domicile": "FOREIGN"},
  {"date": "2025-06-16", "broker_code": "KZ", "side": "B", "volume_lot": 98220,  "value": 87930000000,  "avg_price": 8951.3, "domicile": "FOREIGN"}
]
```

### Notes

To rank accumulators/distributors, group by `broker_code` and compute `net = Σ(buy) − Σ(sell)`; split by `domicile` (`LOCAL` / `FOREIGN`) for foreign-vs-local flow. Sorted by `date`, then `value` descending; capped at 5,000 rows — narrow the date range for busy tickers. IDX-specific; no US/global equivalent.
