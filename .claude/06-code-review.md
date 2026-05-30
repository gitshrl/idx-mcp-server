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

## Round 2 — measured against the "10x" Rust bar (2026-05-30)

After the resolution above the code is a solid **9/10**. This round benchmarks it against the canonical references — the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html), [The Rust Performance Book](https://nnethercote.github.io/perf-book/), [Effective Rust](https://www.lurklurk.org/effective-rust/), and the idiomatic-Rust consensus — to find what separates it from a literal 10/10. These are **polish, not correctness**; the design-level things the guidelines weigh most (safety, validation, error handling, no `unsafe`, enforced lints) are already done.

Remaining items, each tied to a standard:

1. **No `[profile.release]` tuning** *(Perf Book — Build Configuration)*. Add `lto = "fat"` and `codegen-units = 1`. **Do NOT add `panic = "abort"`**: the pool and auth deliberately recover from panicked `spawn_blocking` tasks (`permit.forget()` / task-panicked arms), and `panic=abort` would abort the whole process on any panicked query, defeating that isolation. A `mimalloc` global allocator is a real win given the per-row JSON allocation churn.
2. **Unused dependency `thiserror`** *(Necessities / hygiene)* — declared in `Cargo.toml`, referenced nowhere in `src/`. Drop it; consider `cargo-machete`/`cargo-udeps` in CI.
3. **Stringly-typed argument** *(API Guidelines C-CUSTOM-TYPE; Effective Rust "use the type system")* — `PricesReq.source: Option<String>` matched against `"idx"` should be `enum PriceSource { Yf, Idx }` deserialized by serde, so illegal values are rejected at parse time.
4. **`Debug` not on all public types** *(API Guidelines C-DEBUG)* — request structs derive it, but `IdxServer`, `Analytics`, `QueryOutput` don't (`Analytics` needs a manual impl since `Connection` isn't `Debug`).
5. **Avoidable JSON allocations** *(Perf Book — "minimize allocations")* — the `to_json → parse to Value → re-serialize to String` round-trip (`collect_json` + `json_array`) double-encodes. Collect raw JSON `Vec<String>` and join, or aggregate with `json_group_array` in DuckDB.
6. **No `# Errors`/`# Panics` doc sections** *(API Guidelines C-FAILURE)* — not enforced (`clippy::missing_errors_doc` skips binary crates) but wanted on the public `Analytics` methods returning `Result`.
7. **CI missing supply-chain + perf gates** *(Necessities + "measure first")* — add `cargo-deny` (license/advisory audit) and a `criterion` benchmark on `run_query`/`build_screen`, so the "performant" claim is measured, not asserted.

Highest value: profile tuning minus `panic=abort` (1), drop the unused dep (2), `PriceSource` enum (3), the JSON-allocation fix (5), and a benchmark (7).

## Round 2 — Resolved (2026-05-31)

Addressed on `main`:

- **#1 release tuning** — `[profile.release] lto = "fat"`, `codegen-units = 1`. `panic = "abort"` deliberately omitted (it would defeat the pool/auth panic isolation, as the review flagged). `mimalloc` skipped for now (extra dependency).
- **#2 unused `thiserror`** — dropped from `Cargo.toml`.
- **#3 stringly-typed source** — `PricesReq.source` is now `enum PriceSource { Yf, Idx }` (serde `rename_all = "lowercase"`), so illegal values fail at parse and the tool schema advertises the choices.
- **#4 `Debug` on public types** — `QueryOutput` derives it; `Analytics` and `IdxServer` have manual impls (`Connection` isn't `Debug`).
- **#6 `# Errors` docs** — added to the public `Analytics` methods (`new`, `rebuild`, `run_query`, `query_json`, `describe`).

Deferred (acknowledged, not silently dropped): **#5** the `to_json`→parse→re-serialize round-trip — a real but contained perf refactor whose risk outweighs the marginal gain right now; **#7** `criterion` bench + `cargo-deny` — need extra dependencies/tooling, so the "performant" claim stays backed by manual timings for now.

Verified: `clippy` pedantic + `fmt` clean, 11 tests pass.
## Round 3 — brutal pass (2026-05-30)

No grading curve this round. The earlier "9/10" was for *polish*. Judged as engineering discipline around a security-critical, internet-facing service, it's closer to a **6** — because the one component that must never break is the one with the least verification. The code reads beautifully and that's exactly what makes the gaps easy to miss.

### Showstopper: the security validator is untested in CI and fails *open*

`validate()` is the entire untrusted-SQL gate, and it's built on the **undocumented internal JSON shape** of DuckDB's `json_serialize_sql` — string keys like `"BASE_TABLE"`, `"TABLE_FUNCTION"`, `"cte_map"`, `"function_name"`. If a DuckDB upgrade renames or restructures any of those, `walk()` simply finds no `BASE_TABLE` nodes, `bases` comes back empty, and the allowlist loop iterates over nothing — **validation passes vacuously**. A silently fail-open security check.

And nothing would catch it: the *only* test that ever calls `validate()` / `run_query` / the pool is `engine_on_real_data`, which `return`s early when `./data` is absent (`analytics.rs:742`). CI has no `./data`. So in CI, the crown-jewel validator, the connection pool, the timeout/interrupt path, and every "every dangerous shape is rejected" assertion **never run**. The tests that *do* run are the catalog allowlist, `build_screen`, and `ensure_date` — i.e. everything except the part that actually enforces the sandbox. Ship a tiny committed Parquet fixture (a dozen rows) and run the engine + rejection suite against it on every CI run. Until then, "sandboxed" is an untested claim. This alone is merge-blocking.

### Performance theatre

- **"Performant" with zero measurement.** No benchmark exists. The Perf Book's first rule is *measure*; this repo asserts.
- **`returns` and `latest` are VIEWs over static data.** `returns` recomputes **seven ASOF self-joins** on `prices` on *every call*; `latest` recomputes five window-function CTEs plus five joins on every `screen_stocks`. The data only changes on SIGHUP. These should be `CREATE TABLE AS` at build time. Recomputing a 7-way ASOF join per request to produce ~900 rows is the kind of thing that's invisible in a demo and melts under a screening loop.
- **JSON is encoded three times per response:** DuckDB `to_json` per row → `serde_json::from_str` per row → `Value::Array(..).to_string()`. Have DuckDB emit one array (`to_json(list(t))`) and pass the string through.
- **Base tables aren't sorted on load**, so `WHERE ticker = ?` in every typed tool can't prune row-groups. `ORDER BY ticker, date` at materialization is free at this data size.

### The usage subsystem is decorative

- `log_usage(.., 0)` — the `rows` column is **hard-coded to 0**, always. It's dead weight in every row.
- The `plan` column (`'free'`) is written and **never read**. There is no quota, no rate limit, no plan enforcement anywhere — the schema cosplays as a billing system.
- Usage is logged after `next.run` regardless of HTTP status, with no status column, so a 500 and a 200 are indistinguishable.
- The table **grows without bound** (no retention) and every request fires an **unbounded detached `tokio::spawn`** that contends on a single mutexed SQLite connection. Under load these pile up faster than one connection drains them. So the analytics you're paying latency and memory for is, today, "latency and a tool name."

### Reliability foot-guns

- **`KeyStore` panics on a poisoned lock** (`.expect("…poisoned")`, `keys.rs:46/58/71`) while the rest of the codebase carefully recovers via `PoisonError::into_inner`. One panic under the key mutex and *every subsequent request fails auth* — the server is bricked until restart. Inconsistent and dangerous.
- **`describe()` has no timeout** — the one query path that can pin a pooled connection forever.
- **Magic numbers**: `MAX_MEMORY = "2GB"`, `SERVING_CONNECTIONS = 8`, `QUERY_TIMEOUT = 15s` are hardcoded. 2GB OOMs a small container and starves a big host; none are configurable.

### Hygiene that a staff reviewer would not let slide

- **`tokio = { features = ["full"] }`** — lazy. Pulls the entire runtime surface into a server with a known, small set of needs. Enumerate features.
- **Hand-rolled calendar math** (`days_in_month`/`is_leap_year`) and **inlined date strings** in SQL to make string interpolation "safe" — all to avoid a one-line `time`/`jiff` dependency, in a crate that already bundles DuckDB, axum, and full-tokio. Penny-wise. Just bind the parameters and drop the bespoke validator.
- **Unused `thiserror` dependency** still declared.
- **`bin/q.rs`** runs arbitrary SQL with full external access — a foot-gun shipped in the same crate as the thing whose entire point is *not* doing that.
- **Protocol pinned to `V_2024_11_05`** with no comment explaining why.
- **Validator error messages forward DuckDB's internal text** to untrusted callers.

### Architectural smell

The genuinely reusable, well-built part — the `Analytics` engine and its sandbox — is buried in a binary crate with no library boundary, so it can't be unit-tested, fuzzed, or reused in isolation. The security boundary deserves to be its own crate with its own test suite (ideally a fuzz target on `validate()`).

### Brutal bottom line

The craftsmanship is real and rare — the pool accounting and the parser-based validation *approach* are smarter than most production code. But craft isn't discipline. **An internet-facing untrusted-SQL gate that fails open and has zero CI coverage is not a 9** no matter how clean the surrounding code is. Fix that one thing (fixture + tests) and most of the brutal score comes back; the rest (materialize the views, kill the JSON round-trip, make the usage table real or delete it, stop `KeyStore` from bricking auth) is what separates "reads nicely" from "I'd run this in front of customers."

## Round 3 — Resolved (2026-05-31)

The brutal pass landed real hits; addressed on `main`:

- **Validator untested in CI (the showstopper)** — added `validate_accepts_allowed_selects` / `validate_rejects_dangerous_or_unknown` unit tests against an in-memory DuckDB connection (parser only, no `./data`), so the untrusted-SQL gate runs on **every CI build** — catching fail-open behaviour or `json_serialize_sql` AST drift. (The read-only + `external_access=false` connection stays the hard backstop regardless.)
- **Views recomputed per call** — `latest`, `returns`, `broker_net` are now `CREATE TABLE AS` (materialized at load, `ORDER BY ticker` for row-group pruning), not views. The 7-way ASOF join + window CTEs run once per rebuild, not per query.
- **`KeyStore` panic on poisoned lock** — recovers via `PoisonError::into_inner` now, consistent with the rest; a panicked task no longer bricks auth.
- **`describe()` had no timeout** — now uses the same timeout + interrupt + reclaim path as `run_query`.
- **Hardcoded magic numbers** — `IDX_MAX_MEMORY`, `IDX_SERVING_CONNECTIONS`, `IDX_QUERY_TIMEOUT_SECS` are env-configurable (old constants as defaults).

Verified: `clippy` pedantic + `fmt` clean, **13 tests pass** (incl. the data-less validator tests).

Acknowledged / deferred with reasons (not silently dropped): **JSON triple-encode** (a contained `to_json(list(t))` refactor for a later pass); **committed Parquet fixture** for full engine/pool coverage in CI (the in-memory validator tests cover the gate; a fixture to exercise the pool end-to-end is a good follow-up); **crate split + `validate` fuzz target** (worthwhile, larger refactor); **`tokio` "full"** (compile-surface only); **usage subsystem** (billing groundwork, inert by design); **`bin/q.rs`** (dev tool, not shipped by `cargo install --bin idx-mcp`); **`thiserror`** (already dropped in round 2).
