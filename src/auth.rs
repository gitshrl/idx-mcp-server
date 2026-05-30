use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};

use crate::keys::KeyStore;

#[derive(Clone)]
pub struct AuthState {
    pub keys: Arc<KeyStore>,
}

/// Bearer-API-key gate, applied as the outermost layer so it runs on every
/// request (including the MCP `initialize` handshake) before the MCP handler.
/// On success it logs a coarse usage row; per-tool granularity comes later.
pub async fn auth_middleware(
    State(state): State<AuthState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let key = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let Some(key) = key else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let key_id = match state.keys.verify(key) {
        Ok(Some(id)) => id,
        Ok(None) => return Err(StatusCode::UNAUTHORIZED),
        Err(e) => {
            tracing::error!(error = %e, "api key verification failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let start = Instant::now();
    let response = next.run(request).await;
    let latency_ms = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
    if let Err(e) = state.keys.log_usage(key_id, "mcp", latency_ms, 0) {
        tracing::warn!(error = %e, "usage logging failed");
    }

    Ok(response)
}
