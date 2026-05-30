> Docs index: see [the index](01-index.md) to discover all pages.

# screen_stocks

> Screen stocks cross-sectionally on the latest per-ticker snapshot.

### Overview

A typed cross-sectional screener over the `latest` view (one row per ticker). Provide a list of filters (all AND-ed) and an optional sort; get back the matching tickers with their snapshot fields. For derived metrics like returns, use [run_query](run-query.md) against the `returns` view instead.

### Parameters

* `filters` (array, required) — each `{ "field": ..., "op": ..., "value": ... }`, AND-ed together.
* `sort` (object, optional) — `{ "field": ..., "desc": true|false }`. Defaults to `market_cap` descending.
* `limit` (integer, optional) — max results, default `50`, hard cap `5000`.

**Numeric fields** (ops `= != < <= > >= between`): `market_cap`, `enterprise_value`, `shares_outstanding`, `free_float_pct`, `trailing_pe`, `forward_pe`, `price_to_book`, `dividend_yield`, `beta`, `return_on_equity`, `profit_margins`, `close`, `volume`, `rsi_14`, `sma_50`, `sma_200`.

**Text field** (ops `=`, `in`): `sector`.

`between` takes a `[low, high]` array; `in` takes a list of strings.

### Example

"Liquid financials yielding over 3% with a low P/B, biggest first."

```json
{
  "method": "tools/call",
  "params": {
    "name": "screen_stocks",
    "arguments": {
      "filters": [
        { "field": "sector", "op": "=", "value": "Financials" },
        { "field": "dividend_yield", "op": ">", "value": 3 },
        { "field": "price_to_book", "op": "between", "value": [0, 1.5] }
      ],
      "sort": { "field": "market_cap", "desc": true },
      "limit": 20
    }
  }
}
```

### Example Response

```json
[
  {"ticker": "BBCA", "company_name": "Bank Central Asia Tbk.", "sector": "Financials", "market_cap": 1.2e15, "dividend_yield": 3.4, "price_to_book": 1.3}
]
```

### Notes

Filters and sort fields are matched against the allowlist above; anything else is rejected. Operates on the **latest** snapshot only — not a time series. Yahoo-derived ratios (`trailing_pe`, `forward_pe`, `price_to_book`) are unreliable for IDX names; prefer `market_cap`/`dividend_yield`/`free_float_pct` from the IDX/fundamentals side for hard filters.
