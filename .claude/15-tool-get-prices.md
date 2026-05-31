> Docs index: see [the index](01-index.md) to discover all pages.

# get_prices

> Daily OHLCV price history for an IDX ticker.

### Overview

End-of-day prices for one ticker, optionally bounded by a date range. Served from `eod_summary` (the official IDX end-of-day summary): raw/unadjusted OHLC close, volume, traded value, plus foreign buy/sell flow. Coverage spans `2025-01-02`..`2026-05-29`.

### Parameters

* `ticker` (string, required) — IDX ticker symbol, e.g. `BBCA`.
* `from` (string, optional) — inclusive start date, `YYYY-MM-DD`.
* `to` (string, optional) — inclusive end date, `YYYY-MM-DD`.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_prices", "arguments": { "ticker": "BBCA", "from": "2026-01-01", "to": "2026-03-31" } }
}
```

Open-ended (from a date to latest):

```json
{
  "method": "tools/call",
  "params": { "name": "get_prices", "arguments": { "ticker": "BBRI", "from": "2026-05-01" } }
}
```

### Example Response

```json
[
  {"ticker": "BBCA", "date": "2026-01-02", "open": 7750, "high": 7775, "low": 7700, "close": 7725, "previous": 7700, "change": 25, "volume": 68612400, "value": 530000000000, "frequency": 31000, "foreign_buy": 210000000000, "foreign_sell": 185000000000},
  {"ticker": "BBCA", "date": "2026-01-05", "open": 7725, "high": 7800, "low": 7700, "close": 7775, "previous": 7725, "change": 50, "volume": 73408800, "value": 571000000000, "frequency": 34000, "foreign_buy": 240000000000, "foreign_sell": 198000000000}
]
```

### Notes

`close` is the official IDX unadjusted (raw) exchange price — not split/dividend-adjusted. Sorted ascending by `date`; capped at 5,000 rows.
