> Docs index: see [llms.txt](llms.txt) to discover all pages.

# MCP Server

> Connect the remote IDX market-data MCP server to your AI client.

## Overview

The server speaks MCP over **Streamable HTTP** at `/mcp`. Any MCP client can connect, authenticate with an API key, and call the tools listed in [llms.txt](llms.txt). Tools query a loaded, read-only DuckDB serving database (built from the Parquet at startup) — there is no separate database to operate. The flexible `run_query` tool runs read-only SQL; the typed shortcuts cover common lookups.

## Authentication

- **API key (Bearer)** — available now. Send `Authorization: Bearer <key>` on every request. Create a key with `idx-mcp keys add <label>` (printed once; stored as a SHA-256 hash).
- **OAuth 2.1** — planned, for Claude.ai web and Claude Desktop (which don't send a static header). Tracked in [Architecture](01-architecture.md) §Auth.

## Getting Started

### Claude Code

```bash
claude mcp add --transport http idx http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer <your-key>"
claude mcp list   # verify it's connected
```

Then call tools in any session, e.g. *"search IDX tickers for 'bank'"*.

### Cursor / other MCP clients

Point the client at the Streamable HTTP endpoint `http://<host>:8080/mcp` and add the header `Authorization: Bearer <your-key>`.

### Claude.ai web / Claude Desktop

These require OAuth, which is not available yet — it's the next milestone (see [Architecture](01-architecture.md) §Auth). Until then, use Claude Code or a programmatic client with a Bearer key.
