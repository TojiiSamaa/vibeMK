//! A tools/call per debug tool, through the real dispatcher (read-only mode), against a fake CheckMK.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Dispatcher) {
    let (server, client) = fake_checkmk().await;
    (server, Dispatcher::from_client(client, true))
}

async fn mount(server: &MockServer, p: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/{p}")))
        .respond_with(template)
        .mount(server)
        .await;
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn tools_call_debug_api_endpoints_samples_each_endpoint() {
    let (server, d) = setup().await;
    mount(
        &server,
        "domain-types",
        ok(json!({"domain_type": [{"a": 1}, {"b": 2}, {"c": 3}]})),
    )
    .await;
    mount(
        &server,
        "domain-types/host/collections/all",
        ok(json!({"value": [{"id": "web1"}]})),
    )
    .await;
    mount(&server, "objects", ok(json!([1, 2]))).await;
    // The other endpoints are unmounted (404): reported as an exception, not a failed call.
    let r = d
        .call_tool("vibemk_debug_api_endpoints", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], false);
    let t = text(&r);
    assert!(t.starts_with("🔍 **CheckMK API Endpoints Debug**\n\n✅ **domain-types**\n   Keys: ['domain_type']\n   Sample: [{'a': 1}, {'b': 2}]..."), "{t}");
    assert!(t.contains("✅ **domain-types/host/collections/all**\n   Keys: ['value']\n   Sample: [{'id': 'web1'}]..."), "{t}");
    assert!(
        t.contains("✅ **objects**\n   Data type: <class 'list'>\n   Content: [1, 2]..."),
        "{t}"
    );
    assert!(
        t.contains("💥 **domain-types/host**\n   Exception: "),
        "{t}"
    );
}

#[tokio::test]
async fn tools_call_debug_permissions_reports_access_per_endpoint() {
    let (server, d) = setup().await;
    mount(&server, "version", ok(json!({"site": "cmk"}))).await;
    mount(
        &server,
        "domain-types/host/collections/all",
        ok(json!({"value": [1, 2, 3]})),
    )
    .await;
    mount(
        &server,
        "domain-types/service/collections/all",
        ok(json!({"value": "x"})),
    )
    .await;
    mount(
        &server,
        "domain-types/host_config/collections/all",
        ok(json!({"links": []})),
    )
    .await;
    mount(&server, "objects/user_config", ResponseTemplate::new(403)).await;
    let r = d
        .call_tool("vibemk_debug_permissions", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], false);
    let t = text(&r);
    assert!(
        t.starts_with("🔐 **Permissions Debug**\n\n🧪 **API Version**\n   ✅ Access granted\n"),
        "{t}"
    );
    assert!(t.contains("🧪 **Domain Types**\n   💥 Exception: "), "{t}");
    assert!(t.contains("🧪 **User Info**\n   💥 Exception: "), "{t}");
    assert!(
        t.contains("🔒 **Host Monitoring Permissions**\n   ✅ Access granted - 3 items"),
        "{t}"
    );
    assert!(
        t.contains("🔒 **Service Monitoring Permissions**\n   ✅ Access granted - unknown items"),
        "{t}"
    );
    assert!(
        t.contains("🔒 **Host Config Permissions**\n   ✅ Access granted - structure: ['links']"),
        "{t}"
    );
}
