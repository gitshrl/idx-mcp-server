> Docs index: see [llms.txt](../llms.txt) to discover all pages.

# get_announcements

> IDX corporate announcements and news, optionally filtered by ticker and date range.

### Overview

Corporate disclosures and news from IDX — corporate actions, filings, and press items. Filter by ticker and/or date range, or omit the ticker for a market-wide feed. Each row carries a `source` marking whether it came from the official announcement stream or the news stream.

### Parameters

* `ticker` (string, optional) — IDX ticker symbol, e.g. `GOTO`. Omit for all tickers.
* `from` (string, optional) — inclusive start date, `YYYY-MM-DD`.
* `to` (string, optional) — inclusive end date, `YYYY-MM-DD`.
* `limit` (integer, optional, default `50`) — maximum number of results.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "get_announcements", "arguments": { "ticker": "GOTO", "from": "2026-01-01", "to": "2026-03-31" } }
}
```

Market-wide feed for a day (no ticker):

```json
{
  "method": "tools/call",
  "params": { "name": "get_announcements", "arguments": { "from": "2026-05-05", "limit": 20 } }
}
```

### Example Response

```json
[
  {"date": "2026-03-12", "ticker": "GOTO", "source": "announcement", "title": "Penambahan Modal Tanpa HMETD", "subject": "...", "announcement_type": "STOCK", "announcement_no": "102/EDI/III/2026", "published_at": "2026-03-12 18:16:42"}
]
```

### Notes

Combines the official announcement and news streams (`source` = `announcement` or `news`); titles/subjects are in Indonesian. Sorted by `date` descending.
