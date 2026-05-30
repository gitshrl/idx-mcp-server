> Docs index: see [the index](01-index.md) to discover all pages.

# search_tickers

> Find IDX tickers by symbol or company name.

### Overview

Resolve a free-text query — a symbol fragment or part of a company name — to IDX tickers. Use it to discover symbols before calling the other tools.

### Parameters

* `query` (string, required) — substring matched against the ticker symbol or company name (case-insensitive).
* `limit` (integer, optional, default `20`) — maximum number of results.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "search_tickers", "arguments": { "query": "bank", "limit": 5 } }
}
```

### Example Response

```json
[
  {"ticker": "BBCA", "company_name": "Bank Central Asia Tbk.", "sector": "Keuangan", "exchange": "IDX"},
  {"ticker": "BBRI", "company_name": "Bank Rakyat Indonesia (Persero) Tbk.", "sector": "Keuangan", "exchange": "IDX"},
  {"ticker": "BMRI", "company_name": "Bank Mandiri (Persero) Tbk.", "sector": "Keuangan", "exchange": "IDX"},
  {"ticker": "ARTO", "company_name": "Bank Jago Tbk.", "sector": "Keuangan", "exchange": "IDX"},
  {"ticker": "BBNI", "company_name": "Bank Negara Indonesia (Persero) Tbk.", "sector": "Keuangan", "exchange": "IDX"}
]
```

### Notes

Backed by the `companies` dataset (latest profile per ticker). Returns an empty array when nothing matches.
