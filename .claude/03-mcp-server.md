> Docs index: see [the index](01-index.md) to discover all pages.

# MCP Server

> Connect the remote IDX market-data MCP server to your AI client.

## Overview

The server speaks MCP over **Streamable HTTP** at `/mcp`. Any MCP client can connect, authenticate with an API key, and call the tools listed in [the index](01-index.md). Tools query a loaded, read-only DuckDB serving database (built from the Parquet at startup) — there is no separate database to operate. The flexible `run_query` tool runs read-only SQL; the typed shortcuts cover common lookups.

## Authentication

- **API key (Bearer)** — available now. Send `Authorization: Bearer <key>` on every request. Create a key with `idx-mcp keys add <label>` (printed once; stored as a SHA-256 hash).
- **OAuth 2.1** — available now, for Claude.ai web and Claude Desktop (which don't send a static header). The server runs an OAuth 2.1 authorization server (Dynamic Client Registration, authorization_code + PKCE S256, opaque audience-bound tokens); `/mcp` returns 401 with a `WWW-Authenticate` header pointing at the protected-resource metadata. The auth middleware accepts a static API key OR an OAuth token. See [Architecture](04-architecture.md) §Auth.

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

These require OAuth, which is now available (see [Architecture](04-architecture.md) §Auth). Point the client at the server URL and it will register dynamically and complete the authorization_code + PKCE flow. You can also use Claude Code or a programmatic client with a Bearer key.
