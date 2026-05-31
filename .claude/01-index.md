# IDX Market-Data MCP

> A remote MCP server that gives AI agents Indonesian (IDX) market data — prices, fundamentals, broker flow (bandarmology), KSEI ownership, and corporate announcements — served from Parquet via an embedded DuckDB.

## Guides

- [Quick Start](02-quickstart.md): Connect a client and make your first tool call.
- [MCP Server](03-mcp-server.md): Connect the remote MCP server — endpoint, auth, clients.

## Design

- [Architecture](04-architecture.md): System design, datasets, the query-engine sandbox, auth/OAuth plan, decisions, risks.
- [Data Contract](05-data-contract.md): Source field → canonical column, per dataset.
- [Sources](06-sources.md): Upstream sources and the raw collection inventory.
- [Implementation Plan](07-implementation-plan.md): milestones for the complex-Q&A engine.

## Tools

- [run_query](10-tool-run-query.md): Run read-only SQL over the data plus the `latest`/`returns`/`broker_net` views — the flexible tool for any derived or analytical question.
- [describe_schema](11-tool-describe-schema.md): List the queryable tables and views with their columns and types.
- [screen_stocks](12-tool-screen-stocks.md): Cross-sectional screen (filter + sort) over the per-ticker latest snapshot.
- [search_tickers](13-tool-search-tickers.md): Find IDX tickers by symbol or company name.
- [get_company](14-tool-get-company.md): Company profile, key statistics, and market summary for a ticker.
- [get_prices](15-tool-get-prices.md): Daily official IDX end-of-day prices (raw close), with volume, traded value, and foreign buy/sell flow.
- [get_broker_activity](16-tool-get-broker-activity.md): Per-broker buy/sell flow (bandarmology) for a ticker over a date range.
- [get_ownership](17-tool-get-ownership.md): KSEI depository holders ≥1%, with local/foreign split.
- [get_announcements](18-tool-get-announcements.md): IDX corporate announcements and news.
- [get_filing](19-tool-get-filing.md): on-demand — fetch + text-extract an announcement PDF past the Cloudflare wall via Chrome-TLS emulation (headless-browser fallback), cached.

## Reviews & testing

- [Code Review](08-code-review.md): three rounds of senior-Rust review and how each was resolved.
- [Scenario Q&A corpus](09-scenario-qa.md): 100 complex investment-scenario questions answered with reasoning + tool queries + real-ticker picks (a reasoning test corpus).

## Optional

- [Repository](https://github.com/gitshrl/idx-mcp-server)
