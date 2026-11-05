//! MCP over HTTP (axum): POST JSON-RPC on one path, bearer token on every request.
//! Port of http_auth.py + CheckMKMCPServer.http_app/run_http.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;

use crate::dispatch::Dispatcher;

pub const TOKEN_VARIABLE: &str = "VIBEMK_HTTP_TOKEN";
pub const MINIMUM_TOKEN_LENGTH: usize = 16;

/// The token to require, from a raw value: refuses an absent, blank or short one.
pub fn validate_token(raw: Option<&str>) -> Result<String, String> {
    let token = raw.unwrap_or("").trim();
    if token.is_empty() {
        return Err(format!(
            "{TOKEN_VARIABLE} must be set to serve over HTTP. \
             Generate one with: python -c \"import secrets; print(secrets.token_urlsafe(32))\""
        ));
    }
    if token.chars().count() < MINIMUM_TOKEN_LENGTH {
        return Err(format!(
            "{TOKEN_VARIABLE} must be at least {MINIMUM_TOKEN_LENGTH} characters"
        ));
    }
    Ok(token.to_string())
}

/// `read_token()` of http_auth.py.
pub fn read_token() -> Result<String, String> {
    validate_token(std::env::var(TOKEN_VARIABLE).ok().as_deref())
}

/// Equal length, then every byte folded in: no early exit on the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let (scheme, presented) = value.split_once(' ').unwrap_or((value, ""));
    scheme.eq_ignore_ascii_case("bearer")
        && constant_time_eq(presented.trim().as_bytes(), token.as_bytes())
}

async fn require_bearer(
    State(token): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(request.headers(), &token) {
        return next.run(request).await;
    }
    // Nothing about the presented value is echoed back.
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, r#"Bearer realm="vibeMK""#)],
        Json(serde_json::json!({"error": "unauthorized"})),
    )
        .into_response()
}

async fn post_mcp(State(dispatcher): State<Arc<Dispatcher>>, body: String) -> Response {
    match crate::mcp::handle_line(&dispatcher, &body).await {
        Some(answer) => ([(header::CONTENT_TYPE, "application/json")], answer).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Router on `path`; every request must carry the bearer `token`.
pub fn router_at(path: &str, dispatcher: Arc<Dispatcher>, token: String) -> axum::Router {
    axum::Router::new()
        .route(path, post(post_mcp))
        .with_state(dispatcher)
        .layer(middleware::from_fn_with_state(
            Arc::new(token),
            require_bearer,
        ))
}

/// The default router, served on `/mcp`.
pub fn router(dispatcher: Arc<Dispatcher>, token: String) -> axum::Router {
    router_at("/mcp", dispatcher, token)
}

/// Bind and serve until the process stops.
pub async fn serve(
    addr: SocketAddr,
    path: &str,
    dispatcher: Arc<Dispatcher>,
    token: String,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("vibemk: serving MCP on http://{addr}{path}");
    axum::serve(listener, router_at(path, dispatcher, token)).await?;
    Ok(())
}
