//! Port of tests/test_http_auth.py (11 tests) against the real axum router on a real socket.
mod common;

use std::sync::Arc;

use vibemk::dispatch::Dispatcher;
use vibemk::http::{router_at, validate_token};
use vibemk::registry::ToolRegistry;

const TOKEN: &str = "s3cr3t-token-value";

async fn serve() -> (String, wiremock::MockServer) {
    let (fake, client) = common::fake_checkmk().await;
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(ToolRegistry::from_client(client))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router_at("/mcp", dispatcher, TOKEN.to_string())).await
    });
    (url, fake)
}

const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

async fn post(url: &str, authorization: Option<&str>) -> reqwest::Response {
    let mut request = reqwest::Client::new().post(url).body(PING);
    if let Some(value) = authorization {
        request = request.header("Authorization", value);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn a_request_without_authorization_is_refused() {
    let (url, _fake) = serve().await;
    assert_eq!(post(&url, None).await.status(), 401);
}

#[tokio::test]
async fn a_wrong_token_is_refused() {
    let (url, _fake) = serve().await;
    assert_eq!(post(&url, Some("Bearer not-the-token")).await.status(), 401);
}

#[tokio::test]
async fn a_token_without_the_bearer_scheme_is_refused() {
    let (url, _fake) = serve().await;
    assert_eq!(post(&url, Some(TOKEN)).await.status(), 401);
}

#[tokio::test]
async fn the_right_token_reaches_the_application() {
    let (url, _fake) = serve().await;
    let response = post(&url, Some(&format!("Bearer {TOKEN}"))).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["result"], serde_json::json!({}));
}

#[tokio::test]
async fn the_scheme_is_matched_case_insensitively() {
    let (url, _fake) = serve().await;
    assert_eq!(
        post(&url, Some(&format!("bearer {TOKEN}"))).await.status(),
        200
    );
}

#[tokio::test]
async fn the_body_does_not_echo_the_token() {
    let (url, _fake) = serve().await;
    let text = post(&url, Some("Bearer guess-me"))
        .await
        .text()
        .await
        .unwrap();
    assert!(!text.contains("guess-me"));
}

#[tokio::test]
async fn the_refusal_names_the_scheme_it_wants() {
    let (url, _fake) = serve().await;
    let response = post(&url, None).await;
    let challenge = response
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(challenge.contains("Bearer"));
}

#[test]
fn a_configured_token_is_returned() {
    assert_eq!(
        validate_token(Some("from-the-environment")).unwrap(),
        "from-the-environment"
    );
}

#[test]
fn an_absent_token_refuses_to_start() {
    assert!(validate_token(None)
        .unwrap_err()
        .contains("VIBEMK_HTTP_TOKEN"));
}

#[test]
fn a_blank_token_refuses_to_start() {
    assert!(validate_token(Some("   "))
        .unwrap_err()
        .contains("VIBEMK_HTTP_TOKEN"));
}

#[test]
fn a_token_that_is_too_short_refuses_to_start() {
    assert!(validate_token(Some("short"))
        .unwrap_err()
        .contains("at least"));
}
