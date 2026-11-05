//! Port of tests/test_handlers_services.py (1 test: the service-status fallback chain) plus a
//! tools/call per read tool of the services handler, through the real dispatcher (read-only mode).
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COLLECTION: &str = "/domain-types/service/collections/all";

async fn setup() -> (MockServer, Dispatcher) {
    let (server, client) = fake_checkmk().await;
    (server, Dispatcher::from_client(client, true))
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

async fn mount(server: &MockServer, p: &str, template: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{API}{p}")))
        .respond_with(template)
        .mount(server)
        .await;
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn status(d: &Dispatcher) -> Value {
    d.call_tool(
        "vibemk_get_service_status",
        Some(&json!({"host_name": "example.com", "service_description": "CPU utilization"})),
    )
    .await
}

#[tokio::test]
async fn an_exception_during_processing_falls_through_to_the_next_method() {
    let (server, d) = setup().await;
    // Method 1 (collection filtered on the host): HTTP-level success, but the entry has no extensions.
    Mock::given(method("GET"))
        .and(path(format!("{API}{COLLECTION}")))
        .and(query_param("host_name", "example.com"))
        .respond_with(ok(json!({"value": [{"extensions": null}]})))
        .mount(&server)
        .await;
    // Method 2 (show_service): a clean miss.
    mount(
        &server,
        "/objects/host/example.com/actions/show_service/invoke",
        ResponseTemplate::new(404),
    )
    .await;
    // Method 3 (query API): the answer the test expects.
    Mock::given(method("GET")).and(path(format!("{API}{COLLECTION}"))).and(query_param_is_missing("host_name"))
        .respond_with(ok(json!({"value": [{"extensions": {"state": 0, "plugin_output": "OK", "last_check": 0}}]}))).mount(&server).await;

    let t = text(&status(&d).await);
    assert!(t.contains("Dict Format"), "{t}");
    assert!(
        !t.contains("Unexpected Error") && !t.contains("Service Status Retrieval Failed"),
        "{t}"
    );
}

#[tokio::test]
async fn tools_call_services_lists_state_per_service() {
    let (server, d) = setup().await;
    let value = json!([
        {"extensions": {"host_name": "web1", "description": "CPU", "state": 0}},
        {"extensions": {"host_name": "web1", "description": "Disk", "state": 2}},
        {"extensions": {"host_name": "web2", "description": "Odd", "state": 9}},
        {"extensions": {"description": "Nameless"}},
    ]);
    mount(&server, COLLECTION, ok(json!({"value": value}))).await;
    let r = d
        .call_tool("vibemk_get_checkmk_services", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        text(&r),
        "🔧 **CheckMK Services** (4 total, showing first 4):\n\n🔧 web1/CPU (Status: OK)\n🔧 web1/Disk (Status: CRITICAL)\n🔧 web2/Odd (Status: UNKNOWN(9))\n🔧 Unknown/Nameless (Status: UNKNOWN(None))"
    );
}

#[tokio::test]
async fn tools_call_services_filters_by_host_and_says_when_empty() {
    let (server, d) = setup().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}{COLLECTION}")))
        .and(query_param("host_name", "web1"))
        .respond_with(ok(json!({"value": []})))
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_get_checkmk_services",
            Some(&json!({"host_name": "web1"})),
        )
        .await;
    assert_eq!(
        (r["isError"].clone(), text(&r)),
        (json!(false), "📭 No services found".to_string())
    );
}

#[tokio::test]
async fn tools_call_services_reports_a_failed_retrieval() {
    let (server, d) = setup().await;
    mount(&server, COLLECTION, ResponseTemplate::new(500)).await;
    let r = d
        .call_tool("vibemk_get_checkmk_services", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "❌ **Service retrieval failed**\n\nCould not retrieve services using any available method"
    );
}

#[tokio::test]
async fn tools_call_service_status_uses_the_collection_with_output_and_structured_data() {
    let (server, d) = setup().await;
    let ext = json!({"host_name": "example.com", "description": "CPU utilization", "state": 2, "state_type": 1,
        "plugin_output": " disk full ", "last_check": 1700000000, "last_state_change": 1700000000});
    mount(
        &server,
        COLLECTION,
        ok(json!({"value": [{"extensions": {"description": "Other"}}, {"extensions": ext}]})),
    )
    .await;
    let r = status(&d).await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        text(&r),
        "🔴 **Service Status: example.com/CPU utilization**\n\n**Status:** CRITICAL\n**State Code:** 2\n**Output:** disk full\n**Since:** 2023-11-14 22:13 UTC"
    );
    assert_eq!(
        r["structuredContent"],
        json!({"host_name": "example.com", "service_description": "CPU utilization", "state": "CRITICAL", "state_code": 2,
            "plugin_output": "disk full", "last_check": 1700000000, "last_state_change": 1700000000, "is_hard_state": true})
    );
}

#[tokio::test]
async fn tools_call_service_status_falls_back_to_show_service() {
    let (server, d) = setup().await;
    mount(&server, COLLECTION, ResponseTemplate::new(404)).await;
    mount(
        &server,
        "/objects/host/example.com/actions/show_service/invoke",
        ok(json!({"extensions": {"state": 1, "state_type": 0, "last_check": 0}})),
    )
    .await;
    let t = text(&status(&d).await);
    assert!(t.starts_with("⚠️ **Service Status: example.com/CPU utilization**\n\n**Status:** WARNING\n**State Code:** 1\n**Last Check:** Unknown\n**State Type:** Soft"), "{t}");
}

#[tokio::test]
async fn show_service_without_extensions_is_terminal() {
    let (server, d) = setup().await;
    mount(&server, COLLECTION, ResponseTemplate::new(404)).await;
    mount(
        &server,
        "/objects/host/example.com/actions/show_service/invoke",
        ok(json!({"foo": 1})),
    )
    .await;
    let r = status(&d).await;
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "❌ **Unexpected response format**\n\nExpected extensions in response, got: ['foo']"
    );
}

#[tokio::test]
async fn tools_call_service_status_summarises_when_every_method_fails() {
    let (server, d) = setup().await;
    mount(&server, COLLECTION, ResponseTemplate::new(404)).await;
    mount(
        &server,
        "/objects/host/example.com/actions/show_service/invoke",
        ResponseTemplate::new(404),
    )
    .await;
    let r = status(&d).await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).starts_with("❌ **Service Status Retrieval Failed**\n\nService: example.com/CPU utilization\n\n**Tried Methods:**\n1️⃣"), "{}", text(&r));
}

#[tokio::test]
async fn service_status_needs_both_arguments() {
    let (_server, d) = setup().await;
    let r = d
        .call_tool(
            "vibemk_get_service_status",
            Some(&json!({"host_name": "web1", "service_description": ""})),
        )
        .await;
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "❌ **Missing parameters**\n\nhost_name and service_description are required"
    );
}
