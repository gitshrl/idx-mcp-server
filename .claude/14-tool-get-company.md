> Docs index: see [the index](01-index.md) to discover all pages.

# get_company

> Company profile, key statistics, and market summary for an IDX ticker.

### Overview

Returns a single object combining three sources for one ticker: the company `profile` (sector, listing board, IPO, background), `key_stats` (market cap, free float, latest dividend), and a Yahoo-derived `summary` (valuation ratios, analyst target). Use it for a one-call overview of a company.

### Parameters

* `ticker` (string, required) — IDX ticker symbol, e.g. `BBCA`.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_company", "arguments": { "ticker": "BBCA" } }
}
```

### Example Response

```json
{
  "profile": {
    "ticker": "BBCA", "company_name": "Bank Central Asia Tbk.", "sector": "Keuangan",
    "listing_board": "Utama", "ipo_date": "2000-05-31", "company_background": "..."
  },
  "key_stats": {
    "ticker": "BBCA", "date": "2026-05-05", "market_cap": 733487000000000,
    "free_float_pct": 42.45, "latest_dividend": 432.0, "latest_dividend_year": 2020
  },
  "summary": {
    "ticker": "BBCA", "name": "Bank Central Asia Tbk", "dividend_yield": 0.024,
    "return_on_equity": 21.0, "target_mean_price": 11200, "recommendation_key": "buy"
  }
}
```

### Notes

The `summary` block is Yahoo-derived; its valuation ratios (PE, price/book) are unreliable for IDX names because Yahoo mixes IDR and USD scales. Prefer `key_stats` (market cap, free float, dividend) and [get_prices](get-prices.md). Any block may be `null` if the ticker is missing from that source.
