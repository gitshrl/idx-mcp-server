> Docs index: see [llms.txt](../llms.txt) to discover all pages.

# get_prices

> Daily OHLCV price history for an IDX ticker.

### Overview

End-of-day prices for one ticker, optionally bounded by a date range. Two sources: `yf` (Yahoo Finance, split/dividend-**adjusted** — the default, good for charts and returns) and `idx` (the official IDX end-of-day summary — raw close, plus foreign buy/sell flow).

### Parameters

* `ticker` (string, required) — IDX ticker symbol, e.g. `BBCA`.
* `from` (string, optional) — inclusive start date, `YYYY-MM-DD`.
* `to` (string, optional) — inclusive end date, `YYYY-MM-DD`.
* `source` (string, optional, default `yf`) — `yf` (adjusted) or `idx` (official, with foreign flow).

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_prices", "arguments": { "ticker": "BBCA", "from": "2026-01-01", "to": "2026-03-31" } }
}
```

Official IDX close with foreign flow:

```json
{
  "method": "tools/call",
  "params": { "name": "get_prices", "arguments": { "ticker": "BBRI", "source": "idx", "from": "2026-05-01" } }
}
```

### Example Response

```json
[
  {"ticker": "BBCA", "date": "2026-01-02", "open": 7736.3, "high": 7736.3, "low": 7664.5, "close": 7688.4, "volume": 68612400, "dividends": 0, "splits": 0},
  {"ticker": "BBCA", "date": "2026-01-05", "open": 7664.5, "high": 7760.3, "low": 7664.5, "close": 7736.3, "volume": 73408800, "dividends": 0, "splits": 0}
]
```

With `source=idx`, rows additionally include `previous`, `change`, `value`, `frequency`, `foreign_buy`, and `foreign_sell`.

### Notes

`yf` is split/dividend-adjusted, so its `close` differs from the raw exchange price; `idx` is the official unadjusted close. Pick one consistently. Sorted ascending by `date`; capped at 5,000 rows.
