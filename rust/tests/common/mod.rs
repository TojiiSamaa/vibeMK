//! Shared test helpers: a fake CheckMK (wiremock) and a client pointed at it.
#![allow(dead_code)]

use std::sync::Arc;

use vibemk::client::CheckMkClient;
use vibemk::config::Config;

/// REST base path the client uses on the fake server.
pub const API: &str = "/cmk/check_mk/api/1.0";

/// Test config on `server_url`, site "cmk", no TLS check, no retries.
pub fn test_config(server_url: &str) -> Config {
    Config {
        server_url: server_url.to_string(),
        site: "cmk".to_string(),
        username: "automation".to_string(),
        password: "test-secret".to_string(),
        verify_ssl: false,
        timeout: 5,
        max_retries: 0,
        debug: false,
        never_activate_changes: false,
    }
}

/// A URL nothing listens on (bind to a free port, then release it).
pub fn dead_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    format!("http://127.0.0.1:{port}")
}

/// Fake CheckMK whose client retries up to `max_retries` times.
pub async fn fake_checkmk_retrying(max_retries: u32) -> (wiremock::MockServer, Arc<CheckMkClient>) {
    let server = wiremock::MockServer::start().await;
    let config = Config {
        max_retries,
        ..test_config(&server.uri())
    };
    let base = format!("{}{API}", server.uri());
    (
        server,
        Arc::new(CheckMkClient::with_api_base_url(config, base)),
    )
}

/// Fake CheckMK + client on `{uri}/cmk/check_mk/api/1.0` (no URL detection).
/// Mount mocks with paths like `format!("{API}/objects/host_config/web1")`.
pub async fn fake_checkmk() -> (wiremock::MockServer, Arc<CheckMkClient>) {
    let server = wiremock::MockServer::start().await;
    let config = test_config(&server.uri());
    let base = format!("{}{API}", server.uri());
    (
        server,
        Arc::new(CheckMkClient::with_api_base_url(config, base)),
    )
}
