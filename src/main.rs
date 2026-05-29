mod auth;
mod config;
mod keys;
mod server;
mod store;

use std::sync::Arc;

use anyhow::Result;
use axum::{Router, middleware};
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::auth::{AuthState, auth_middleware};
use crate::config::Config;
use crate::keys::KeyStore;
use crate::server::IdxServer;
use crate::store::Store;

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
    let store = Arc::new(Store::open(&cfg)?);
    tracing::info!(base = store.base(), "data source ready");

    let ct = CancellationToken::new();

    let factory_store = store.clone();
    let mcp: StreamableHttpService<IdxServer, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(IdxServer::new(factory_store.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let auth_state = AuthState { keys };
    let app = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(auth_state, auth_middleware));

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
