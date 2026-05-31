//! On-demand IDX filing (announcement PDF) fetch + text extraction.
//!
//! IDX serves announcement PDFs behind Cloudflare, which 403s a standard TLS
//! stack (the gate is a JA3/HTTP-2 browser fingerprint). Two ways past it,
//! primary then fallback:
//!   1. `wreq` with a Chrome TLS/HTTP-2 emulation profile — one HTTPS request,
//!      no browser. Fast, clean; the default hot path.
//!   2. a headless browser (`chromiumoxide`, the "playwright" approach) that
//!      navigates `idx.co.id` to clear Cloudflare, then fetches the PDF from the
//!      page context — the resilient fallback if the emulation profile goes
//!      stale against a tightened WAF.
//!
//! Extracted text is cached in memory so a repeat read never re-fetches. This is
//! a deliberately SEPARATE path from `run_query`: that `DuckDB` serving
//! connection stays locked, read-only, and egress-free. Only this tool egresses,
//! to one allowlisted host.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde::Serialize;
use tokio::sync::Mutex;
use wreq::Client;
use wreq_util::Emulation;

/// Cap extracted text so a huge filing can't blow up a tool response.
const MAX_TEXT_CHARS: usize = 200_000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Navigated once (browser fallback) to obtain the Cloudflare `cf_clearance` cookie.
const CLEARANCE_URL: &str = "https://www.idx.co.id/en/";
const CLEARANCE_WAIT: Duration = Duration::from_secs(6);

/// Monotonic counter so each browser launch gets its own profile dir (no shared
/// `SingletonLock` collision with leftover chrome processes).
static LAUNCH: AtomicU64 = AtomicU64::new(0);

/// One fetched + extracted filing.
#[derive(Clone, Serialize)]
pub struct Filing {
    pub url: String,
    pub bytes: usize,
    pub chars: usize,
    pub truncated: bool,
    pub text: String,
}

/// A live headless-browser session holding the cleared origin page (fallback).
struct Session {
    page: Page,
    _browser: Browser,
    _drive: tokio::task::JoinHandle<()>,
}

/// On-demand filing fetcher: a Chrome-emulating HTTP client (primary), a lazily
/// launched headless browser (fallback), and an in-memory text cache.
pub struct Filings {
    client: Client,
    chrome: Option<String>,
    session: Mutex<Option<Session>>,
    cache: Mutex<HashMap<String, Arc<Filing>>>,
}

impl std::fmt::Debug for Filings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filings").finish_non_exhaustive()
    }
}

impl Filings {
    /// Build the Chrome-emulating client.
    ///
    /// # Errors
    /// Fails if the TLS client cannot be constructed.
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .emulation(Emulation::Chrome137)
            .timeout(FETCH_TIMEOUT)
            .build()
            .context("build wreq client")?;
        let chrome = std::env::var("IDX_CHROME").ok().or_else(|| {
            let p = "/usr/bin/google-chrome";
            std::path::Path::new(p).exists().then(|| p.to_string())
        });
        Ok(Self {
            client,
            chrome,
            session: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Fetch a filing PDF and return its extracted text (cached after first hit).
    /// Tries the fast emulated-client path; on failure falls back to the
    /// headless browser.
    ///
    /// # Errors
    /// Fails if the url is off-host, both fetch paths fail, the body is not a
    /// PDF, or text extraction fails.
    pub async fn fetch(&self, url: &str) -> Result<Arc<Filing>> {
        validate_url(url)?;
        if let Some(hit) = self.cache.lock().await.get(url).cloned() {
            return Ok(hit);
        }
        let body = match self.fetch_wreq(url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "wreq fetch failed; falling back to headless browser");
                self.fetch_via_browser(url)
                    .await
                    .context("browser fallback fetch")?
            }
        };
        if !body.starts_with(b"%PDF") {
            bail!("fetched body is not a PDF ({} bytes)", body.len());
        }
        let nbytes = body.len();
        let full = tokio::task::spawn_blocking(move || pdf_extract::extract_text_from_mem(&body))
            .await
            .context("pdf-extract task")?
            .context("extract pdf text")?;
        let truncated = full.chars().count() > MAX_TEXT_CHARS;
        let text: String = if truncated {
            full.chars().take(MAX_TEXT_CHARS).collect()
        } else {
            full
        };
        let filing = Arc::new(Filing {
            url: url.to_string(),
            bytes: nbytes,
            chars: text.chars().count(),
            truncated,
            text,
        });
        self.cache
            .lock()
            .await
            .insert(url.to_string(), filing.clone());
        Ok(filing)
    }

    /// Primary path: one HTTPS GET with a Chrome TLS/HTTP-2 fingerprint.
    async fn fetch_wreq(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.client.get(url).send().await.context("wreq request")?;
        let status = resp.status();
        if !status.is_success() {
            bail!("HTTP {}", status.as_u16());
        }
        Ok(resp.bytes().await.context("read body")?.to_vec())
    }

    /// Fallback path ("playwright"): drive a headless browser to clear Cloudflare,
    /// then fetch the PDF from the page context. Relaunches + retries once.
    async fn fetch_via_browser(&self, url: &str) -> Result<Vec<u8>> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            *guard = Some(self.launch().await?);
        }
        let page = guard
            .as_ref()
            .map(|s| s.page.clone())
            .ok_or_else(|| anyhow!("no browser session"))?;
        // Cloudflare clearance can take a few seconds to settle after navigation;
        // retry the page-context fetch on the same session before giving up.
        let mut last = anyhow!("browser fetch not attempted");
        for _ in 0..4 {
            match browser_fetch(&page, url).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    last = e;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
        // clearance never settled on this session → relaunch once and try again
        *guard = Some(self.launch().await?);
        let page = guard
            .as_ref()
            .map(|s| s.page.clone())
            .ok_or_else(|| anyhow!("no browser session"))?;
        browser_fetch(&page, url)
            .await
            .map_err(|e| anyhow!("{e} (after relaunch; prior: {last})"))
    }

    /// Launch a headless browser and clear Cloudflare for the IDX origin.
    async fn launch(&self) -> Result<Session> {
        let profile = std::env::temp_dir().join(format!(
            "idx-mcp-chrome-{}-{}",
            std::process::id(),
            LAUNCH.fetch_add(1, Ordering::Relaxed)
        ));
        // Default headless UA is "HeadlessChrome/..." — an instant Cloudflare
        // tell. Override with a real Chrome UA (what the working probe used).
        let mut builder = BrowserConfig::builder()
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--no-sandbox")
            .arg("--user-agent=Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
            .user_data_dir(&profile);
        if let Some(path) = &self.chrome {
            builder = builder.chrome_executable(path);
        }
        let cfg = builder.build().map_err(|e| anyhow!(e))?;
        let (browser, mut handler) = Browser::launch(cfg).await?;
        let drive = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let page = browser.new_page(CLEARANCE_URL).await?;
        page.wait_for_navigation().await?;
        tokio::time::sleep(CLEARANCE_WAIT).await;
        Ok(Session {
            page,
            _browser: browser,
            _drive: drive,
        })
    }
}

/// Fetch the url from a cleared page context (carries `cf_clearance`); returns raw bytes.
async fn browser_fetch(page: &Page, url: &str) -> Result<Vec<u8>> {
    let js = format!(
        r#"(async () => {{
            const r = await fetch({url:?}, {{credentials:"include"}});
            if (!r.ok) return "ERR:" + r.status;
            const a = await r.arrayBuffer();
            const u = new Uint8Array(a); let s = ""; const C = 8192;
            for (let i = 0; i < u.length; i += C) s += String.fromCharCode.apply(null, u.subarray(i, i + C));
            return "OK:" + btoa(s);
        }})()"#
    );
    let ev = EvaluateParams::builder()
        .expression(js)
        .await_promise(true)
        .return_by_value(true)
        .build()
        .map_err(|e| anyhow!(e))?;
    let payload: String = page.evaluate(ev).await?.into_value()?;
    if let Some(code) = payload.strip_prefix("ERR:") {
        bail!("browser fetch blocked: HTTP {code}");
    }
    let b64 = payload
        .strip_prefix("OK:")
        .ok_or_else(|| anyhow!("bad browser fetch payload"))?;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("decode pdf base64")
}

/// Only allow `idx.co.id` hosts over https (no open SSRF via the tool).
fn validate_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("url must be https"))?;
    let host = rest.split('/').next().unwrap_or_default();
    if host == "idx.co.id" || host.ends_with(".idx.co.id") {
        Ok(())
    } else {
        bail!("url host not allowed (must be idx.co.id): {host}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "https://www.idx.co.id/StaticData/NewsAndAnnouncement/ANNOUNCEMENTSTOCK/From_EREP/202605/7228d2fba8_6e12f20f4a.pdf";

    #[test]
    fn validate_rejects_off_host() {
        assert!(validate_url("https://evil.com/x.pdf").is_err());
        assert!(validate_url("http://www.idx.co.id/x.pdf").is_err()); // not https
        assert!(validate_url("https://www.idx.co.id/StaticData/x.pdf").is_ok());
    }

    /// Live: primary (wreq) path. Needs network.
    #[tokio::test]
    #[ignore = "needs network; hits live idx.co.id past Cloudflare"]
    async fn live_fetch_wreq() {
        let url = std::env::var("IDX_TEST_PDF").unwrap_or_else(|_| SAMPLE.to_string());
        let f = Filings::new().expect("build client");
        let body = f.fetch_wreq(&url).await.expect("wreq fetch");
        eprintln!("wreq bytes={}", body.len());
        assert!(body.starts_with(b"%PDF"), "not a pdf");
    }

    /// Live: fallback (headless-browser / "playwright") path. Needs network + chrome.
    #[tokio::test]
    #[ignore = "needs network + chrome; drives a headless browser"]
    async fn live_fetch_via_browser() {
        let url = std::env::var("IDX_TEST_PDF").unwrap_or_else(|_| SAMPLE.to_string());
        let f = Filings::new().expect("build client");
        let body = f.fetch_via_browser(&url).await.expect("browser fetch");
        eprintln!("browser bytes={}", body.len());
        assert!(body.starts_with(b"%PDF"), "not a pdf");
    }

    /// Live: full fetch + extract + cache (uses whichever path works).
    #[tokio::test]
    #[ignore = "needs network; hits live idx.co.id"]
    async fn live_fetch_and_extract() {
        let url = std::env::var("IDX_TEST_PDF").unwrap_or_else(|_| SAMPLE.to_string());
        let f = Filings::new().expect("build client");
        let r = f.fetch(&url).await.expect("fetch filing");
        eprintln!("bytes={} chars={}", r.bytes, r.chars);
        assert!(r.bytes > 1000);
        let r2 = f.fetch(&url).await.expect("cached");
        assert!(Arc::ptr_eq(&r, &r2), "second fetch must hit cache");
    }
}
