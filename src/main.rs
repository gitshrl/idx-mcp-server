mod analytics;
mod auth;
mod catalog;
mod config;
mod filings;
mod keys;
mod oauth;
mod server;

use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::analytics::Analytics;
use crate::auth::{AuthState, auth_middleware};
use crate::config::Config;
use crate::filings::Filings;
use crate::keys::{KeyStore, UsageLogger};
use crate::oauth::OAuthState;
use crate::server::IdxServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env();
    let args: Vec<String> = std::env::args().collect();

    // `idx-mcp keys add [label]` — create an API key and print it once.
    if args.get(1).map(String::as_str) == Some("keys")
        && args.get(2).map(String::as_str) == Some("add")
    {
        let label = args
            .get(3)
            .cloned()
            .unwrap_or_else(|| "unnamed".to_string());
        let keys = KeyStore::open(&cfg.sqlite_path)?;
        println!("{}", keys.add_key(&label)?);
        return Ok(());
    }

    let keys = Arc::new(KeyStore::open(&cfg.sqlite_path)?);
    // Single bounded background writer for usage telemetry (batched + pruned).
    let usage = UsageLogger::spawn(keys.clone());

    // Build the loaded, locked, read-only serving database. Fails fast if no
    // data could be loaded.
    let analytics = Arc::new(Analytics::new(&cfg)?);
    tracing::info!(
        tables = ?analytics.loaded_tables(),
        views = ?analytics.loaded_views(),
        "serving database ready"
    );

    // SIGHUP rebuilds the serving database in place (manual refresh).
    spawn_sighup_refresh(analytics.clone());

    let ct = CancellationToken::new();

    // On-demand filing fetcher (Chrome-emulating HTTP client; egress path kept
    // separate from the locked, egress-free run_query engine).
    let filings = Arc::new(Filings::new(keys.clone())?);

    let factory_analytics = analytics.clone();
    let factory_filings = filings.clone();
    let mcp: StreamableHttpService<IdxServer, LocalSessionManager> = StreamableHttpService::new(
        move || {
            Ok(IdxServer::new(
                factory_analytics.clone(),
                factory_filings.clone(),
            ))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let auth_state = AuthState {
        keys: keys.clone(),
        public_url: cfg.public_url.clone(),
        usage,
    };
    let oauth_state = OAuthState {
        keys: keys.clone(),
        public_url: cfg.public_url.clone(),
    };

    // `/mcp` is gated by the auth layer; the `.well-known` discovery docs and
    // `/oauth/*` endpoints MUST stay OUTSIDE it so a client can find the AS and
    // complete the flow before it has a token.
    let mcp_app = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(auth_state, auth_middleware));
    let oauth_app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::as_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(oauth::as_metadata),
        )
        .route("/oauth/register", post(oauth::register))
        .route("/oauth/authorize", get(oauth::authorize))
        .route("/oauth/token", post(oauth::token))
        .with_state(oauth_state);
    let app = oauth_app.merge(mcp_app);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "idx-mcp listening on /mcp");

    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        ct.cancel();
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    Ok(())
}

/// Rebuild the serving database whenever the process receives SIGHUP. The
/// rebuild runs on a blocking thread; on failure the previous data is kept.
fn spawn_sighup_refresh(analytics: Arc<Analytics>) {
    tokio::spawn(async move {
        let mut hup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot install SIGHUP handler");
                return;
            }
        };
        while hup.recv().await.is_some() {
            tracing::info!("SIGHUP received: rebuilding serving database");
            let a = analytics.clone();
            match tokio::task::spawn_blocking(move || a.rebuild()).await {
                Ok(Ok(())) => tracing::info!("serving database rebuilt"),
                Ok(Err(e)) => tracing::error!(error = %e, "rebuild failed; keeping previous data"),
                Err(e) => tracing::error!(error = %e, "rebuild task panicked"),
            }
        }
    });
}
