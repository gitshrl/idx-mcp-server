# get_filing (on-demand)

> Fetch an IDX announcement/disclosure PDF on demand, past the Cloudflare wall,
> and return the extracted text. An MCP tool **in the server** — not a separate
> service.

## Two ways past Cloudflare (primary + fallback)

Announcement PDFs live at `https://www.idx.co.id/StaticData/NewsAndAnnouncement/...`,
behind **Cloudflare**. Probed 2026-05-31: plain HTTP, `+User-Agent`,
`+Referer+Accept` all return **HTTP 403** (the `Attention Required! | Cloudflare`
page). The gate is a **JA3 / HTTP-2 fingerprint**, not headers or a JS challenge.
Two fetch paths clear it, tried in order:

1. **`wreq`, Chrome TLS/HTTP-2 emulation (primary).** A single HTTPS GET with a
   Chrome fingerprint passes the WAF — **no browser, ~1 RTT**. The fast, clean
   default. (Verified: `application/pdf`, 200.)
2. **Headless browser (`chromiumoxide`, the "playwright" approach — fallback).**
   Navigate `idx.co.id` to obtain the `cf_clearance` cookie, then `fetch()` the
   PDF from the page context. Used only if (1) fails — e.g. Cloudflare tightens
   and the emulation profile goes stale. Resilience without paying browser cost
   on the hot path.

## Design (in-server, Rust)

```
get_announcements        → metadata + PDF url pointer (already serves attachments)
get_filing { url }
        │  cache hit? → return cached text (instant)
        ▼  miss:  verify https + idx.co.id host (no open SSRF)
   fetch_wreq(url)                         ← primary: 1 HTTPS GET, Chrome JA3
        │ ok → bytes
        │ err → fetch_via_browser(url)     ← fallback: chromiumoxide clears CF, page fetch
        ▼
   verify %PDF magic → pdf_extract::extract_text_from_mem (blocking thread)
        → cache (L1 in-memory + L2 SQLite, by url) → return
```

**Security boundary held:** its own tool handler. `run_query` stays a locked,
read-only, **egress-free** `DuckDB` (no `read_parquet`/httpfs, parser allowlist).
Only `get_filing` egresses, to one allowlisted host.

**Verified end-to-end (2026-05-31):** `get_filing` over MCP on the WSKT RUPO
notice → 6039-byte PDF → 2476 chars of extracted text. Both paths covered by
`#[ignore]` live tests (`live_fetch_wreq`, `live_fetch_via_browser`).

## Tool spec

`get_filing`:
- `url` (string, required) — the PDF `FullSavePath` from `get_announcements`;
  https + on `idx.co.id`.
- returns `{ url, bytes, chars, truncated, text }` (text capped at 200k chars).

## Implementation (`src/filings.rs`)

- **Crates:** `wreq` (Chrome TLS/HTTP-2 emulation) + `wreq-util`
  (`Emulation::Chrome137`) + `chromiumoxide` (headless-browser fallback) +
  `pdf-extract` (pure-Rust text).
- **Caching (two-level):** L1 in-memory `HashMap<url, Arc<Filing>>` backed by an
  L2 `SQLite` table (`filings`, in `idx.sqlite`) — repeat reads never re-fetch,
  and extracted text survives a restart (an L2 hit is promoted back into L1).
- One reused `wreq::Client`; the browser session is **lazily launched only if the
  fallback is hit** (most requests never start a browser).

### Build-time toolchain

`wreq` builds BoringSSL from source, which at **build time** needs `Go` (+`PATH`),
`cmake`, a C/C++ compiler, and `libclang` for bindgen; `chromiumoxide` needs a
Chrome/Chromium binary at **runtime** for the fallback only. CI (`.github/workflows/ci.yml`)
installs Go + cmake + clang/libclang-dev. Local build env:

```
export PATH=/usr/local/go/bin:$PATH
export LIBCLANG_PATH=/home/dev/.local/lib/python3.10/site-packages/clang/native
export BINDGEN_EXTRA_CLANG_ARGS="-isystem /usr/lib/gcc/x86_64-linux-gnu/12/include -isystem /usr/include -isystem /usr/include/x86_64-linux-gnu"
```

## Tradeoffs (honest)

- **Build** pulls a BoringSSL toolchain (Go/cmake/libclang) → slower first build.
- The **fallback** needs a Chrome binary present at runtime (`IDX_CHROME` or
  `/usr/bin/google-chrome`); it is only invoked if the primary fails, so the hot
  path stays browser-free.
- The Chrome **emulation profile** (`Chrome137`) may need bumping if Cloudflare
  tightens; that's exactly what the browser fallback covers.
- L2 cache is unbounded for now — no TTL/size eviction (filings are small text;
  add pruning if the table grows large).

## Deferred

- `ticker` + `announcement_no` → url lookup (so the agent needn't pass the url).
- L2 cache eviction (TTL / max rows) if the `filings` table grows large.
- OCR for scanned/image-only PDFs.
- Pre-warm a hot subset (lapkeu / keterbukaan informasi material / dividen / RUPS).
- Full-corpus extract → search/RAG (`filings_text`) if it becomes a product feature.
