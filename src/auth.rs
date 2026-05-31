use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION, header::WWW_AUTHENTICATE},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::keys::{KeyStore, UsageEvent, UsageLogger};

/// MCP JSON-RPC request bodies are small; cap what we buffer for tool-name
/// extraction so a large body can't exhaust memory.
const MAX_BODY: usize = 1 << 20; // 1 MiB

#[derive(Clone)]
pub struct AuthState {
    pub keys: Arc<KeyStore>,
    /// Canonical base URL, for the `WWW-Authenticate` discovery hint + audience.
    pub public_url: String,
    /// Off-request, bounded usage writer (no per-request spawn).
    pub usage: UsageLogger,
}

/// Who authenticated. API-key requests carry a key id for usage logging; OAuth
/// tokens don't map to an `api_keys` row, so they skip usage logging for now.
#[derive(Clone, Copy)]
enum Principal {
    ApiKey(i64),
    OAuth,
}

/// Bearer gate on `/mcp`, applied as the outermost layer so it runs on every
/// request (including the MCP `initialize` handshake). Accepts a static API key
/// **or** an OAuth access token (audience-bound). `SQLite` work runs on a
/// blocking thread; usage logging is fired off the response path.
pub async fn auth_middleware(
    State(state): State<AuthState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| {
            // RFC 7235: the auth scheme is case-insensitive ("Bearer" / "bearer").
            let (scheme, tok) = h.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| tok.trim().to_owned())
        })
    else {
        return unauthorized(&state.public_url);
    };

    // Verify off the async runtime — rusqlite is blocking. Try the API key
    // first, then the OAuth token; first hit wins.
    let keys = state.keys.clone();
    let audience = format!("{}/mcp", state.public_url);
    let principal = tokio::task::spawn_blocking(move || match keys.verify(&token) {
        Ok(Some(id)) => Ok(Some(Principal::ApiKey(id))),
        Ok(None) => match keys.verify_oauth(&token, &audience) {
            Ok(true) => Ok(Some(Principal::OAuth)),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    })
    .await;

    let principal = match principal {
        Ok(Ok(Some(p))) => p,
        Ok(Ok(None)) => return unauthorized(&state.public_url),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "credential verification failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "verify task panicked");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Buffer the body to read the JSON-RPC method / tool name for usage
    // logging, then rebuild the request unchanged for the MCP handler.
    let (parts, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let tool = tool_label(&bytes);
    let request = Request::from_parts(parts, Body::from(bytes));

    let start = Instant::now();
    let response = next.run(request).await;
    let latency_ms = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);

    // Hand usage to the bounded background writer — no per-request spawn, no DB
    // contention on the hot path. Only API-key requests carry a key id.
    if let Principal::ApiKey(key_id) = principal {
        state.usage.record(UsageEvent {
            key_id,
            tool,
            latency_ms,
        });
    }

    response
}

/// 401 carrying the RFC 9728 discovery hint so MCP clients can find the AS.
/// Without this header Claude.ai cannot begin the OAuth dance.
fn unauthorized(public_url: &str) -> Response {
    let challenge =
        format!("Bearer resource_metadata=\"{public_url}/.well-known/oauth-protected-resource\"");
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    if let Ok(value) = challenge.parse() {
        resp.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    resp
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
