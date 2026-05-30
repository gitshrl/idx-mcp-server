> Docs index: see [llms.txt](llms.txt) to discover all pages.

# Quick Start

> Connect an agent to the IDX market-data MCP server and make your first tool call.

## 1. Run the server

```bash
IDX_DATA_DIR=./data idx-mcp
```

This serves `http://127.0.0.1:8080/mcp`. Install the binary with `cargo install --git https://github.com/gitshrl/idx-mcp-server.git --locked --bin idx-mcp`. For production, set `R2_ACCOUNT_ID` / `R2_KEY_ID` / `R2_SECRET` / `R2_BUCKET` to read Parquet from Cloudflare R2 instead of `./data`.

## 2. Create an API key

```bash
idx-mcp keys add my-agent
```

The key is printed once — copy it.

## 3. Connect a client

```bash
claude mcp add --transport http idx http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer <your-key>"
```

See [MCP Server](mcp-server.md) for Cursor and other clients.

## 4. Call a tool

Every tool takes a `ticker` and (for time series) optional `from` / `to` dates. Raw MCP request:

```json
{
  "method": "tools/call",
  "params": {
    "name": "get_prices",
    "arguments": { "ticker": "BBCA", "from": "2026-01-01", "to": "2026-03-31" }
  }
}
```

Or just ask your agent: *"What were BBCA's daily prices in Q1 2026?"*

Browse every tool in [llms.txt](llms.txt).
