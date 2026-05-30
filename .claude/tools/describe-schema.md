> Docs index: see [llms.txt](../llms.txt) to discover all pages.

# describe_schema

> List the queryable tables and views with their columns and types.

### Overview

Discover the schema before writing [run_query](run-query.md) SQL. Returns each base table and analytical view with a human description and its live column list (name + type, introspected from the loaded serving database, so it never drifts from the data). Pass a `dataset` name to focus on one relation.

### Parameters

* `dataset` (string, optional) — a single table or view name (e.g. `returns`). Omit to describe everything.

### Example

```json
{
  "method": "tools/call",
  "params": { "name": "describe_schema", "arguments": { "dataset": "returns" } }
}
```

### Example Response

```json
[
  {
    "name": "returns",
    "relation": "view",
    "description": "One row per ticker: trailing % returns (ret_1w/1m/3m/6m/ytd/1y/3y) and annualized cagr_1y/cagr_3y from close prices.",
    "columns": [
      {"name": "ticker", "type": "VARCHAR"},
      {"name": "as_of", "type": "DATE"},
      {"name": "close", "type": "DOUBLE"},
      {"name": "ret_1y", "type": "DOUBLE"},
      {"name": "cagr_3y", "type": "DOUBLE"}
    ]
  }
]
```

### Notes

The `relation` field is `table` (loaded from Parquet) or `view` (`latest`, `returns`, `broker_net`). Descriptions carry the per-dataset semantics and landmines (e.g. Yahoo ratios being unreliable, `broker_rankings` having no `ticker`). Use the names exactly as returned in `run_query`.
