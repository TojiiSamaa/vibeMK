//! Port of tests/test_handlers_connection.py (+ AUDIT S4) and a call per remaining connection tool.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::connection::ConnectionHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

fn is_error(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "_error")
}

async fn call(
    server_and_client: &(MockServer, std::sync::Arc<vibemk::client::CheckMkClient>),
    tool: &str,
    args: Value,
) -> Vec<Value> {
    ConnectionHandler::new(server_and_client.1.clone())
        .handle(tool, &args)
        .await
}

/// Python: connection reset while reading the error body falls back to {"error": reason}; here the
/// error body is not JSON, which takes the same fallback.
#[tokio::test]
async fn an_unreadable_error_body_does_not_break_the_direct_url_test() {
    let f = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(format!("{}/version", common::API)))
        .respond_with(ResponseTemplate::new(500).set_body_string("<html>boom</html>"))
        .mount(&f.0)
        .await;
    let out = call(
        &f,
        "vibemk_test_direct_url",
        json!({"test_url": format!("{}/version", f.1.api_base_url)}),
    )
    .await;
    assert!(is_error(&out));
    assert_eq!(
        text(&out),
        format!("❌ **HTTP Error 500**\n\nURL: {}/version\nError: Internal Server Error\nResponse: {{\n  \"error\": \"Internal Server Error\"\n}}", f.1.api_base_url)
    );
}

/// The request carries the CheckMK credentials: a foreign URL is refused without any request.
#[tokio::test]
async fn a_foreign_url_is_refused_without_a_request() {
    let f = common::fake_checkmk().await;
    let other = MockServer::start().await; // same host, other port: a different origin
    let api = f.1.api_base_url.clone();
    let host = api
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let urls = [
        "http://attacker.example/steal".to_string(),
        format!("{}/cmk/check_mk/api/1.0/version", other.uri()),
        format!("https://{host}/cmk/check_mk/api/1.0/version"),
        format!("http://{host}.attacker.example/cmk/check_mk/api/1.0/version"),
        format!("http://user:pw@{host}/cmk/check_mk/api/1.0/version"),
        format!("http://{host}/cmk/check_mk/login.py"),
        format!("http://{host}/cmk/check_mk/api/1.0/../../wato.py"),
        format!("http://{host}/cmk/check_mk/api/1.0/%2e%2e/%2e%2e/wato.py"),
        format!("http://{host}/cmk/check_mk/api/1.0x/version"),
        "file:///etc/passwd".to_string(),
        // AUDIT S4: antislash and double encoding
        format!("http://{host}/cmk/check_mk/api/1.0/x\\..\\..\\wato.py"),
        format!("http://{host}/cmk/check_mk/api/1.0/x%5c..%5cwato.py"),
        format!("http://{host}/cmk/check_mk/api/1.0/x%252f..%252fwato.py"),
        format!("http://{host}/cmk/check_mk/api/1.0/x%2f..%2f..%2fwato.py"),
    ];
    for url in &urls {
        let out = call(&f, "vibemk_test_direct_url", json!({"test_url": url})).await;
        assert!(is_error(&out), "{url}");
        assert!(
            text(&out).starts_with("❌ **URL not allowed**"),
            "{url}: {}",
            text(&out)
        );
    }
    assert!(f.0.received_requests().await.unwrap().is_empty());
    assert!(other.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_url_under_the_api_is_sent_with_credentials_and_a_redirect_is_not_followed() {
    let f = common::fake_checkmk().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{}/version", common::API)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"versions": {}})))
        .mount(&f.0)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{}/moved", common::API)))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/steal", elsewhere.uri())),
        )
        .mount(&f.0)
        .await;

    let ok = call(
        &f,
        "vibemk_test_direct_url",
        json!({"test_url": format!("{}/version", f.1.api_base_url)}),
    )
    .await;
    assert!(!is_error(&ok));
    assert!(
        text(&ok).starts_with("✅ **Direct URL Test Successful**"),
        "{}",
        text(&ok)
    );
    let sent = &f.0.received_requests().await.unwrap()[0];
    assert!(sent
        .headers
        .get("authorization")
        .is_some_and(|v| v.to_str().unwrap().starts_with("Basic ")));

    let moved = call(
        &f,
        "vibemk_test_direct_url",
        json!({"test_url": format!("{}/moved", f.1.api_base_url)}),
    )
    .await;
    assert!(is_error(&moved));
    assert!(
        text(&moved).starts_with("❌ **HTTP Error 302**"),
        "{}",
        text(&moved)
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_direct_url_test_requires_a_url() {
    let f = common::fake_checkmk().await;
    let out = call(&f, "vibemk_test_direct_url", json!({})).await;
    assert_eq!(
        text(&out),
        "❌ **Missing URL**\n\ntest_url parameter is required"
    );
}

#[tokio::test]
async fn debug_connection_reports_the_version_and_edition() {
    let f = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(format!("{}/version", common::API)))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"versions": {"checkmk": "2.4.0"}, "edition": "cre"})),
        )
        .mount(&f.0)
        .await;
    let out = call(&f, "vibemk_debug_checkmk_connection", json!({})).await;
    let t = text(&out);
    assert!(
        t.starts_with("✅ **CheckMK Connection Successful**\n\n🌐 Server: "),
        "{t}"
    );
    assert!(t.contains("🔒 SSL Verify: False\n"), "{t}");
    assert!(t.ends_with("📊 Version: 2.4.0\n📦 Edition: cre"), "{t}");
    assert!(!t.contains("test-secret"));
}

#[tokio::test]
async fn get_version_and_its_failure() {
    let f = common::fake_checkmk().await;
    let down = call(&f, "vibemk_get_checkmk_version", json!({})).await;
    assert!(is_error(&down));
    assert!(
        text(&down).starts_with("❌ **CheckMK API Error**"),
        "{}",
        text(&down)
    );
    Mock::given(method("GET"))
        .and(path(format!("{}/version", common::API)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"edition": "cee"})))
        .mount(&f.0)
        .await;
    let up = call(&f, "vibemk_get_checkmk_version", json!({})).await;
    assert!(
        text(&up).starts_with(
            "📋 **CheckMK Version Information**\n\nVersion: Unknown\nEdition: cee\nSite: cmk\n"
        ),
        "{}",
        text(&up)
    );
}

#[tokio::test]
async fn test_all_endpoints_marks_each_one() {
    let f = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(format!("{}/version", common::API)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&f.0)
        .await;
    let t = text(&call(&f, "vibemk_test_all_endpoints", json!({})).await);
    assert!(t.starts_with("🧪 **API Endpoint Test Results**\n\n✅ version - Version info (HTTP 200)\n❌ domain-types/host_config/collections/all - Host Configs (Error: "), "{t}");
    assert_eq!(
        t.lines()
            .filter(|l| l.starts_with("❌ domain-types/"))
            .count(),
        9
    );
}

#[tokio::test]
async fn debug_url_detection_lists_the_probe_results() {
    let f = common::fake_checkmk().await;
    let t = text(&call(&f, "vibemk_debug_url_detection", json!({})).await);
    assert!(
        t.starts_with("🔍 **URL Detection Debug Results**\n\n🌐 Server URL: "),
        "{t}"
    );
    assert!(
        t.ends_with("**Test Results:**\nNo debug info available"),
        "{t}"
    );
}
