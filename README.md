# idx-mcp-server

A remote MCP server that gives AI agents Indonesian (IDX) market data — prices, fundamentals, broker flow (bandarmology), KSEI ownership, and corporate announcements. Agents query it with read-only SQL (`run_query`) or typed shortcuts; the server loads columnar Parquet into a locked, read-only embedded DuckDB at startup, so there is no database to run. Data lives in object storage (Cloudflare R2) or a local directory.

## Install

Install the `idx-mcp` binary from source with Cargo:

```bash
cargo install --git https://github.com/gitshrl/idx-mcp-server.git --locked --bin idx-mcp
```

For local development from this checkout:

```bash
cargo build --release
```

The first build compiles a bundled DuckDB from source (a few minutes). Pinned to Rust 1.96.0 through `rust-toolchain.toml`.

## Run

Point the server at a local Parquet directory and serve:

```bash
IDX_DATA_DIR=./data idx-mcp
```

This serves `http://127.0.0.1:8080/mcp`. For Cloudflare R2, set `R2_ACCOUNT_ID` / `R2_KEY_ID` / `R2_SECRET` / `R2_BUCKET` instead of `IDX_DATA_DIR`; DuckDB reads the Parquet directly over httpfs.

Mint an API key (printed once):

```bash
idx-mcp keys add my-agent
```

## Connect an agent

Claude Code:

```bash
claude mcp add --transport http idx http://127.0.0.1:8080/mcp \
  --header "Authorization: Bearer <your-key>"
```

Any MCP client: point it at the Streamable HTTP endpoint `http://<host>:8080/mcp` and send the header `Authorization: Bearer <your-key>`.

## Tools

`run_query` · `describe_schema` · `screen_stocks` · `search_tickers` · `get_company` · `get_prices` · `get_broker_activity` · `get_ownership` · `get_announcements`

`run_query` is the flexible core — read-only SQL over the tables plus the `latest`, `returns`, and `broker_net` views, sandboxed (single SELECT, allowlisted tables, no file access). The rest are typed shortcuts.

## Docs

Index: [`.claude/01-index.md`](.claude/01-index.md). Highlights:

- [`.claude/02-quickstart.md`](.claude/02-quickstart.md) — connect and make your first tool call.
- [`.claude/03-mcp-server.md`](.claude/03-mcp-server.md) — endpoint, auth, clients.
- [`.claude/04-architecture.md`](.claude/04-architecture.md) — design, datasets, sandbox, auth/OAuth plan.
- [`.claude/05-data-contract.md`](.claude/05-data-contract.md) — data contract (source field → column).
- [`.claude/06-sources.md`](.claude/06-sources.md) — upstream sources and collection inventory.
- per-tool reference: files `10`–`18` (`run_query`, `screen_stocks`, …), all listed in the index.
