# Architecture

System design for the IDX Market-Data MCP server. Status and quickstart live in the root `README.md`; the per-dataset column contract lives in `02-data-contract.md`.

## Goal & boundary

A Rust MCP server that exposes Indonesian market data as MCP tools, querying Parquet via embedded DuckDB. **This repo is the server** (+ a dev ETL loader). The **production** ETL that lands Parquet daily is a separate external system; `02-data-contract.md` is the contract between them.

## System

```
  dev:  Mongo ──(src/bin/etl.rs)──► ./data/<dataset>/date=YYYY-MM-DD/*.parquet
  prod: external ETL ─────────────► R2  s3://idx-data/<dataset>/date=*/...
                                          │
                                          ▼  DuckDB (bundled) reads local dir OR r2:// (httpfs)
   MCP client ──HTTP──►  ┌──────────────────────────────────────────────┐
  Claude/Cursor/web      │  idx-mcp (axum Router)                        │
                         │   public:  /.well-known/*  /oauth/*           │  ← (planned) no auth
                         │   /mcp  .layer(auth: api_key OR oauth token)  │  ← tower, per-request
                         │          .nest_service StreamableHttpService  │  ← rmcp 1.7
                         │   tools → DuckDB → read_parquet(...)           │
                         └──────────────────┬───────────────────────────┘
                                            ▼  SQLite: accounts, api_keys, usage, oauth_*
```

DuckDB reads the local `./data` mirror in dev (`IDX_DATA_DIR`) and R2 directly in prod (`R2_*` env → `CREATE SECRET (TYPE r2…)` + `r2://`). Per-query R2 latency (~50–200ms) is negligible next to an LLM round-trip; a synced local mirror is the documented fallback if needed.

## Datasets

12 source-neutral datasets (see `README.md` for the table with row counts, and `02-data-contract.md` for per-dataset columns). **Canonical keys**: every dataset exposes `ticker` (VARCHAR, UPPER) and, where time-series, `date` (DATE) — the ETL normalizes `stock_code`/`StockCode`/`Code`/`ticker` → `ticker`. Time-series partition daily by `date` (hive `date=YYYY-MM-DD/`); the server reads `<ds>/date=*/*.parquet` with `hive_partitioning=true` and filters uniformly on `ticker`/`date`. Snapshots are a single `<ds>/latest.parquet` (latest row per ticker). Two datasets are ETL-combined (`broker_activity`, `announcements`) and one is exploded+filtered (`ownership`, investors ≥1%).

## MCP tools

Defined with `rmcp`'s `#[tool_router]`/`#[tool]` macros; each takes `Parameters<T>` (`serde::Deserialize` + `schemars::JsonSchema`, auto JSON-Schema). Output is a **curated explicit column projection** per dataset (no `SELECT *`; partition/internal columns stripped). All in `src/server.rs`; `src/store.rs` owns DuckDB access (`query_json` wraps each query in DuckDB `to_json(row)`).

**Live (6):** `search_tickers`, `get_company` (profile+fundamentals+summary), `get_prices` (`source=yf|idx`), `get_broker_activity` (bandarmology), `get_ownership` (KSEI ≥1%), `get_announcements`. **Buildable (datasets ready, tools TODO):** `get_indicators`, `get_analyst`, `get_broker_summary`.

`get_broker_activity` + `get_ownership` are the IDX-specific moat (broker-attributed flow, KSEI local/foreign split — no US/global equivalent) → gate behind paid tier when monetizing.

## Auth & accounts

**Now (built, verified):** a tower middleware on `/mcp` reads `Authorization: Bearer <key>`, SHA-256-hashes it, looks it up in SQLite `api_keys`, 401 on miss, logs `usage`. Works for Claude Code, Cursor, and any HTTP/SDK client. Keys via `idx-mcp keys add`.

**Next milestone — OAuth 2.1 (for Claude.ai web/Desktop).** Those clients won't send a static header; they run the MCP OAuth discovery dance. **rmcp 1.7's `auth` feature is client-side only** (it's an `oauth2`-crate client for connecting *to* servers); it provides zero server-side AS primitives — only `StreamableHttpService` and three reusable serde DTOs (`AuthorizationMetadata`, `ClientRegistrationResponse`, `OAuthClientConfig`). The in-repo `complex_auth_streamhttp.rs` example hand-builds the whole AS on axum — that's our reference.

Decision: **self-host a minimal AS in-process** (don't delegate to an IdP, don't adopt `oxide-auth`/Hydra — they still leave us writing the metadata docs + DCR + validation, and add a dependency). **Opaque tokens** (random string, SHA-256 in SQLite — mirrors the `api_keys` pattern; revocation = one DELETE); no JWT/JWKS (single-instance).

Endpoints to add (all **HTTPS**, behind a reverse proxy):

| Method · path | Purpose |
|---|---|
| GET `/.well-known/oauth-protected-resource` | RFC 9728: `{resource: "<HOST>/mcp", authorization_servers: ["<HOST>"], scopes_supported}` |
| GET `/.well-known/oauth-authorization-server` | RFC 8414: issuer, authorize/token/registration endpoints, `code_challenge_methods_supported:["S256"]`, `grant_types:["authorization_code","refresh_token"]` |
| POST `/oauth/register` | RFC 7591 DCR: validate redirect_uris (https-or-localhost, exact), issue `client_id` (public client, auth method `none`), store |
| GET `/oauth/authorize` | validate client_id + **exact** redirect_uri + `response_type=code` + S256 `code_challenge`; issue single-use short-lived code (MVP: auto-consent to a default account) |
| POST `/oauth/token` | authorization_code grant: single-use code, verify PKCE `base64url-nopad(SHA256(verifier))`, issue opaque access token bound to the `resource` audience |
| (existing) `/mcp` | middleware now accepts api_key **OR** oauth token |

**Hard MUSTs (from the spec research — easy to get wrong):**
- **`WWW-Authenticate` on 401** at `/mcp` pointing at the RFC 9728 URL — the linchpin; without it Claude.ai can't discover the AS. (Current `src/auth.rs` returns a bare 401 — must change.)
- **Route ordering**: `.well-known/*` and `/oauth/*` must be **outside** the auth layer (today the layer wraps the whole Router → would 401 the metadata). Wrap only the nested `/mcp`.
- **Audience binding (RFC 8707)**: store `resource` on the token; reject at `/mcp` any token whose audience ≠ this server's canonical `/mcp` URL. No token pass-through.
- **PKCE S256**: base64url **without** padding. **Codes**: single-use, ~30–60s, exact `redirect_uri` match (whitelist `https://claude.ai/api/mcp/auth_callback` exactly — verify current value).
- **Canonical URL** (`IDX_PUBLIC_URL`): the RFC 9728 `resource`, the token audience, and Claude's `resource` param must be byte-identical.

**Accounts model** — both credential types resolve to one `account_id` (so usage/quota are per account). The single auth middleware tries `verify(key)` then `verify_oauth(token)`; first hit wins. **DB migration is the user's job** (project hard limit — these are emitted, not run):

```sql
CREATE TABLE accounts (id INTEGER PRIMARY KEY, label TEXT, plan TEXT NOT NULL DEFAULT 'free', created_at TEXT NOT NULL);
ALTER TABLE api_keys ADD COLUMN account_id INTEGER REFERENCES accounts(id);
CREATE TABLE oauth_clients (client_id TEXT PRIMARY KEY, redirect_uris TEXT NOT NULL, account_id INTEGER REFERENCES accounts(id), created_at TEXT NOT NULL);
CREATE TABLE oauth_codes  (code_hash TEXT PRIMARY KEY, client_id TEXT, account_id INTEGER, code_challenge TEXT, redirect_uri TEXT, resource TEXT, expires_at TEXT, used INTEGER NOT NULL DEFAULT 0);
CREATE TABLE oauth_tokens (token_hash TEXT PRIMARY KEY, account_id INTEGER, client_id TEXT, audience TEXT, scope TEXT, expires_at TEXT);
```

Implementation tasks (ordered): (1) move `.well-known`/`oauth` routes outside the auth layer, wrap only `/mcp`; (2) apply schema DDL (user) + backfill one account for existing keys; (3) `keys.rs`: `verify()`→account_id, add `verify_oauth()` + DCR/code/token helpers; (4) `IDX_PUBLIC_URL` in `config.rs`; (5) new `src/oauth.rs` (metadata + register + authorize + token); (6) `auth.rs`: try both, emit 401+`WWW-Authenticate`; (7) wire routes, add `base64`/`url` deps; (8) curl the full dance end-to-end. Defer: interactive login/consent (true multi-user), refresh rotation, JWT (only if multi-instance).

**Monetization (later):** `plan` → monthly quota enforced from `usage`; Stripe checkout + webhook flips `plan`; gate moat tools.

## File structure

```
idx-mcp-server/
  Cargo.toml · .env.example · .gitignore · README.md
  src/
    main.rs      # tokio main, `keys add` CLI, rmcp+axum wiring, auth layer, serve
    config.rs    # Config from env (bind, sqlite, DataBase: Local dir | R2)
    store.rs     # DuckDB conn + R2 secret; query_json via DuckDB to_json()
    keys.rs      # SQLite api_keys/usage; key gen/hash, verify, log_usage
    auth.rs      # Bearer tower middleware + usage logging  (+ oauth validation, planned)
    server.rs    # IdxServer: #[tool_router] with the tools + ServerHandler
    oauth.rs     # (planned) AS: metadata + register + authorize + token
    bin/etl.rs   # dev ETL: Mongo JSONL -> contract Parquet under ./data
    bin/q.rs     # ad-hoc DuckDB query tool for ./data
  data/          # local Parquet for dev (gitignored)
```

Single binary crate (one consumer ⇒ no workspace/`idx-core`). Extra binaries (`etl`, `q`) live in `src/bin/` per Cargo convention.

## Key decisions (research-backed)

1. **rmcp 1.7** — official MCP SDK; Streamable HTTP is the current transport (SSE deprecated). `StreamableHttpService` is a `tower::Service` nested in axum behind our auth layer. `auth` feature is **client-only** (see §Auth). `schemars` must match rmcp's version.
2. **DuckDB `bundled`** over Parquet; reads local dir or R2 via httpfs (`CREATE SECRET TYPE r2`). `to_json(row)` avoids per-column type mapping. C++ build is slow first time — cache it.
3. **Cloudflare R2** — **$0 egress** (decisive for a read-heavy API) vs S3's ~$0.09/GB; access via `object_store`/httpfs, region `auto`, path-style, hint `apac`.
4. **Daily `date` partitioning** — matches daily ingestion; date-range pruning; new day = new partition, no rewrite.
5. **Hand-rolled opaque-token AS** — smallest correct design for single-instance; reuses the SHA-256-in-SQLite pattern.
6. **No Docker** — deploy the release binary via systemd behind a TLS reverse proxy.

## Risks

- **`yf*` redistribution licensing** (Yahoo-derived `prices`/`indicators`/`summary`/`analyst`) is restrictive — don't sell redistribution on these; lead paid with IDX/KSEI-owned data.
- **OAuth correctness** — the `WWW-Authenticate` header, route ordering, audience binding, PKCE base64 variant, and exact canonical-URL match are all silent-failure traps (§Auth).
- **HTTPS required** for OAuth — server currently binds plaintext `127.0.0.1:8080`; needs a public TLS endpoint.
- **DB migration is the user's job** — if the `accounts`/`oauth_*` tables aren't applied before deploy, auth breaks.
- **Single-instance** — opaque tokens + in-process rate limit hit one SQLite/DuckDB; move to a shared store before horizontal scaling.
- **KSEI semantics** — central-depository registry (local/foreign), not US 13F/Form-4; label tools accurately.
- **Secrets** — no creds in repo; `.env` gitignored; R2 token read-only.
