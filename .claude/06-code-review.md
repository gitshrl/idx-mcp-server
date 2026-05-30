# Code Review

Senior-Rust review of the server (`src/`, ~1,900 LOC). Companion to `01-architecture.md`. Reviewed by reading, not by compiling — the bundled DuckDB build was not run, so "compiles clean / passes pedantic clippy" is assumed, not verified.

**Overall: 8/10.** Production-leaning, security-first, idiomatic Rust. A few real concurrency and operational issues, plus thin tests on the security-relevant code, keep it out of the 9–10 range.

## What's strong

- **Defense-in-depth on `run_query`** (`src/analytics.rs:1`, `:403`, `:472`). Three independent layers: (1) read-only connection with `enable_external_access=false`, `enable_autoload_extension=false`, then `lock_configuration=true` as the **last** action so external access can't be re-enabled; (2) validation via DuckDB's own parser (`json_serialize_sql`) — the AST is walked for `BASE_TABLE`/`TABLE_FUNCTION` nodes and diffed against an allowlist, not regex-matched; (3) external timeout + interrupt. The layers are redundant on purpose: a scalar `read_text()` the AST walk wouldn't flag is still blocked by the no-external-access connection.
- **Atomic hot-swap of the serving DB** (`src/analytics.rs:147`). `RwLock<Arc<Serving>>` + versioned filenames, relying on Linux unlink-open-file semantics so in-flight queries keep reading the old file. Boot is version 0, rebuilds start at 1 to avoid deleting a file an open connection still holds.
- **Hygiene:** `unsafe_code = "forbid"`, clippy `pedantic`, pinned toolchain, edition 2024, parameterized trusted queries, API keys stored only as SHA-256 (plaintext shown once, `src/keys.rs:42`), poison recovery via `PoisonError::into_inner` throughout. Comments explain *why*.
- **`build_screen`** (`src/server.rs:371`) interpolates only allowlisted field names and a fixed operator set; all values are bound or `CAST(? AS DOUBLE)`. Injection-safe.

## Issues worth fixing

1. **Timeout/interrupt can hit the wrong query** (`src/analytics.rs:177`, `:64`). All queries serialize on a single `Mutex<Connection>`, and `spawn_blocking` acquires the lock *inside* the task. If task A's timeout fires while A is still **waiting** for the lock (B is running), `interrupt.interrupt()` interrupts B — the wrong query — and A later runs un-interrupted, dodging its own deadline. The single connection also serializes every tool call process-wide (acknowledged at `:62`), but the interrupt race is the sharper edge.
2. **Blocking SQLite on the async runtime** (`src/auth.rs:37`, `:49`). `keys.verify()` and `keys.log_usage()` are synchronous rusqlite calls on a tokio worker thread, on every request, holding a mutex; the usage insert sits in the latency-critical path. Wrap in `spawn_blocking` or move usage logging off the request path.
3. **Temp-dir leak across restarts** (`src/analytics.rs:102`). The serving dir is unique per `pid-instance`; nothing removes the directory on shutdown, and `clear_serving_files` only cleans the current (freshly created, empty) dir — never prior processes'. `/tmp/idx-mcp-serving/<pid>-*` accumulates across restarts/crashes.
4. **Thin tests on the riskiest code.** `build_screen` and `ensure_date` are pure, security-relevant functions with **no** unit tests. The one analytics integration test skips when `./data` is absent, so data-less CI exercises almost none of the engine. `keys`/`auth` are untested.

## Minor

- `ensure_date` (`src/server.rs:527`) accepts `2025-02-31` / `2025-04-31` (day range is just `1..=31`); DuckDB then errors on the literal cast with a confusing message.
- `MAX_ROWS` duplicated — `usize` in `src/analytics.rs:28`, `u32` in `src/server.rs:18`. Hoist one into `catalog`.
- Typed tools map failures through `mcp_err` → `internal_error(e.to_string())` (`src/server.rs:489`), forwarding raw DuckDB messages to clients (minor info disclosure).
- `search_tickers` (`src/server.rs:142`) doesn't escape `%`/`_` in the ILIKE pattern, so those chars act as wildcards.
- `Config` derives `Debug` and holds the R2 secret (`src/config.rs:9`); never logged today, but a stray `{cfg:?}` would leak it. Consider a custom `Debug`.

## Highest-value next steps

Fix the interrupt/timeout race (#1) and the async-blocking auth path (#2); add unit tests for `build_screen` and `ensure_date` (#4). With those, this is a 9.

## Resolved (2026-05-31)

All four issues and the minors were addressed on `main`:

- **#1 interrupt/timeout race** — connections are now a fixed pool, checked out **exclusively** (tokio `Semaphore` + idle stack), so a connection serves one query at a time and its interrupt can only hit that query. A timed-out (interrupted) query is reclaimed off-path once it stops; a panicked one forgets its permit to keep `permits == idle.len()`.
- **#2 blocking SQLite on the runtime** — key verify runs in `spawn_blocking`; usage logging is fired off the response path (detached `spawn` + `spawn_blocking`), so neither blocks the reactor nor adds request latency.
- **#3 temp-dir leak** — boot reaps serving dirs of dead processes (`<pid>-*` with no `/proc/<pid>`).
- **#4 thin tests** — unit tests added for `build_screen` (binding, allowlist, injection rejection) and `ensure_date` (calendar validation), plus `escape_like`; these run in data-less CI (11 tests total).
- **Minors** — `ensure_date` does full calendar validation (rejects e.g. `2025-02-31`); `MAX_ROWS` hoisted into `catalog`; typed-tool failures return a generic message (raw DuckDB text only logged); `ILIKE` escapes `%`/`_` via `ESCAPE '\'`; `DataBase` has a redacting `Debug`.

Verified: `clippy` pedantic + `fmt` clean, 11 tests pass, live MCP e2e 9/9.
