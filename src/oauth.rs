//! Minimal OAuth 2.1 authorization server for MCP clients (Claude.ai web/Desktop).
//!
//! Self-hosted, in-process, opaque tokens (no JWT/JWKS — single instance). It
//! implements just enough of RFC 9728 / 8414 / 7591 plus the authorization-code
//! + PKCE flow for the MCP connector discovery dance.
//!
//! MVP: `/authorize` **auto-consents** — there is no login, so anyone who can
//! reach the server can obtain a token. Gating + accounts come with
//! monetization; tokens are still audience-bound and revocable.

use std::fmt::Write as _;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Form, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::keys::KeyStore;

/// Access-token lifetime. Long-lived because we don't issue refresh tokens yet;
/// the client re-runs the flow when it expires.
const TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;

#[derive(Clone)]
pub struct OAuthState {
    pub keys: Arc<KeyStore>,
    /// Canonical externally-visible base URL, no trailing slash (`IDX_PUBLIC_URL`).
    pub public_url: String,
}

impl OAuthState {
    fn resource(&self) -> String {
        format!("{}/mcp", self.public_url)
    }
}

/// RFC 9728 — protected-resource metadata. The `WWW-Authenticate` 401 points here.
#[allow(clippy::unused_async)] // axum handlers must be async
pub async fn protected_resource(State(st): State<OAuthState>) -> Json<Value> {
    Json(json!({
        "resource": st.resource(),
        "authorization_servers": [st.public_url],
        "scopes_supported": ["mcp"],
        "bearer_methods_supported": ["header"],
    }))
}

/// RFC 8414 — authorization-server metadata.
#[allow(clippy::unused_async)] // axum handlers must be async
pub async fn as_metadata(State(st): State<OAuthState>) -> Json<Value> {
    Json(json!({
        "issuer": st.public_url,
        "authorization_endpoint": format!("{}/oauth/authorize", st.public_url),
        "token_endpoint": format!("{}/oauth/token", st.public_url),
        "registration_endpoint": format!("{}/oauth/register", st.public_url),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"],
    }))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    #[serde(default)]
    redirect_uris: Vec<String>,
}

/// RFC 7591 — dynamic client registration. Issues a public client (PKCE, no secret).
pub async fn register(State(st): State<OAuthState>, Json(req): Json<RegisterReq>) -> Response {
    if req.redirect_uris.is_empty() || !req.redirect_uris.iter().all(|u| valid_redirect(u)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris must be https or localhost",
        );
    }
    let keys = st.keys.clone();
    let uris = req.redirect_uris.clone();
    let Ok(Ok(client_id)) = tokio::task::spawn_blocking(move || keys.register_client(&uris)).await
    else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "could not register client",
        );
    };
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "redirect_uris": req.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct AuthorizeReq {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

/// Authorization endpoint. Validates the client + PKCE, then AUTO-CONSENTS and
/// redirects back with a single-use code.
pub async fn authorize(State(st): State<OAuthState>, Query(req): Query<AuthorizeReq>) -> Response {
    // Validate the client + redirect_uri BEFORE trusting the redirect target.
    let keys = st.keys.clone();
    let cid = req.client_id.clone();
    let registered =
        match tokio::task::spawn_blocking(move || keys.client_redirect_uris(&cid)).await {
            Ok(Ok(Some(uris))) => uris,
            Ok(Ok(None)) => return (StatusCode::BAD_REQUEST, "unknown client_id").into_response(),
            _ => return (StatusCode::INTERNAL_SERVER_ERROR, "server error").into_response(),
        };
    if !registered.iter().any(|u| u == &req.redirect_uri) {
        return (StatusCode::BAD_REQUEST, "redirect_uri not registered").into_response();
    }
    // From here, OAuth says errors redirect back to the client.
    if req.response_type != "code" {
        return redirect_err(
            &req.redirect_uri,
            "unsupported_response_type",
            req.state.as_deref(),
        );
    }
    let Some(challenge) = req.code_challenge.as_deref() else {
        return redirect_err(&req.redirect_uri, "invalid_request", req.state.as_deref());
    };
    if req.code_challenge_method.as_deref().unwrap_or("plain") != "S256" {
        return redirect_err(&req.redirect_uri, "invalid_request", req.state.as_deref());
    }
    // Auto-consent: issue a code bound to the PKCE challenge + requested audience.
    let keys = st.keys.clone();
    let cid = req.client_id.clone();
    let chal = challenge.to_string();
    let ruri = req.redirect_uri.clone();
    let resource = req.resource.clone();
    let Ok(Ok(code)) = tokio::task::spawn_blocking(move || {
        keys.create_auth_code(&cid, &chal, &ruri, resource.as_deref())
    })
    .await
    else {
        return redirect_err(&req.redirect_uri, "server_error", req.state.as_deref());
    };
    let mut url = format!(
        "{}{}code={}",
        req.redirect_uri,
        sep(&req.redirect_uri),
        code
    );
    if let Some(s) = req.state.as_deref() {
        let _ = write!(url, "&state={}", urlenc(s));
    }
    Redirect::to(&url).into_response()
}

#[derive(Deserialize)]
pub struct TokenReq {
    grant_type: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
}

/// Token endpoint — `authorization_code` grant with PKCE S256.
pub async fn token(State(st): State<OAuthState>, Form(req): Form<TokenReq>) -> Response {
    if req.grant_type != "authorization_code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code is supported",
        );
    }
    let (Some(code), Some(redirect_uri), Some(verifier)) =
        (req.code, req.redirect_uri, req.code_verifier)
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing code / redirect_uri / code_verifier",
        );
    };
    let keys = st.keys.clone();
    let (c, r) = (code.clone(), redirect_uri.clone());
    let consumed = match tokio::task::spawn_blocking(move || keys.consume_auth_code(&c, &r)).await {
        Ok(Ok(Some(ac))) => ac,
        Ok(Ok(None)) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "code invalid, expired, or already used",
            );
        }
        _ => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "token failed",
            );
        }
    };
    // PKCE: base64url-nopad(SHA256(verifier)) must equal the stored challenge.
    if pkce_s256(&verifier) != consumed.code_challenge {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "PKCE verification failed",
        );
    }
    let audience = consumed.resource.unwrap_or_else(|| st.resource());
    let keys = st.keys.clone();
    let cid = consumed.client_id;
    let Ok(Ok(token)) = tokio::task::spawn_blocking(move || {
        keys.issue_token(&cid, &audience, "mcp", TOKEN_TTL_SECS)
    })
    .await
    else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "token failed",
        );
    };
    Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": TOKEN_TTL_SECS,
        "scope": "mcp",
    }))
    .into_response()
}

fn pkce_s256(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize())
}

fn valid_redirect(u: &str) -> bool {
    u.starts_with("https://")
        || u.starts_with("http://localhost")
        || u.starts_with("http://127.0.0.1")
}

fn sep(url: &str) -> char {
    if url.contains('?') { '&' } else { '?' }
}

fn redirect_err(redirect_uri: &str, err: &str, state: Option<&str>) -> Response {
    let mut url = format!("{redirect_uri}{}error={err}", sep(redirect_uri));
    if let Some(s) = state {
        let _ = write!(url, "&state={}", urlenc(s));
    }
    Redirect::to(&url).into_response()
}

fn oauth_error(status: StatusCode, err: &str, desc: &str) -> Response {
    (
        status,
        Json(json!({ "error": err, "error_description": desc })),
    )
        .into_response()
}

/// Percent-encode a query value (`state` is opaque client data).
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
