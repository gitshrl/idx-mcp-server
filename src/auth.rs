use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};

use crate::keys::KeyStore;

/// MCP JSON-RPC request bodies are small; cap what we buffer for tool-name
/// extraction so a large body can't exhaust memory.
const MAX_BODY: usize = 1 << 20; // 1 MiB

#[derive(Clone)]
pub struct AuthState {
    pub keys: Arc<KeyStore>,
}

/// Bearer-API-key gate, applied as the outermost layer so it runs on every
/// request (including the MCP `initialize` handshake) before the MCP handler.
/// `SQLite` work (key verify, usage logging) runs on a blocking thread, and the
/// usage write is fired off the response path so it never adds request latency.
pub async fn auth_middleware(
    State(state): State<AuthState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(key) = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned)
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    // Verify off the async runtime — rusqlite is blocking.
    let keys = state.keys.clone();
    let key_id = match tokio::task::spawn_blocking(move || keys.verify(&key)).await {
        Ok(Ok(Some(id))) => id,
        Ok(Ok(None)) => return Err(StatusCode::UNAUTHORIZED),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "api key verification failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(e) => {
            tracing::error!(error = %e, "verify task panicked");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Buffer the body to read the JSON-RPC method / tool name for usage
    // logging, then rebuild the request unchanged for the MCP handler.
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let tool = tool_label(&bytes);
    let request = Request::from_parts(parts, Body::from(bytes));

    let start = Instant::now();
    let response = next.run(request).await;
    let latency_ms = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);

    // Log usage off the request path so the SQLite write never delays the reply.
    let keys = state.keys.clone();
    tokio::spawn(async move {
        match tokio::task::spawn_blocking(move || keys.log_usage(key_id, &tool, latency_ms, 0))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "usage logging failed"),
            Err(e) => tracing::warn!(error = %e, "usage logging task panicked"),
        }
    });

    Ok(response)
}

/// The tool name for a `tools/call`, else the JSON-RPC method, else `mcp` for
/// non-JSON or bodyless requests (e.g. the SSE GET stream).
fn tool_label(bytes: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return "mcp".to_string();
    };
    let method = v.get("method").and_then(serde_json::Value::as_str);
    match method {
        Some("tools/call") => v
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tools/call")
            .to_string(),
        Some(other) => other.to_string(),
        None => "mcp".to_string(),
    }
}
