//! Port of tests/test_handlers_monitoring.py (9 tests): problems carry their cause, the generic
//! acknowledge tool is steerable. Against a fake CheckMK (wiremock).
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::monitoring::MonitoringHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The instant the fixture's failing service last changed state.
const BROKE_AT: i64 = 1_757_000_000;

fn service(host: &str, description: &str, state: i64, output: &str, changed: i64) -> Value {
    json!({"extensions": {"host_name": host, "description": description, "state": state, "plugin_output": output, "last_state_change": changed}})
}

async fn mount(server: &MockServer, verb: &str, p: &str, body: Value) {
    Mock::given(method(verb))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn text_of(h: &MonitoringHandler, tool: &str, args: Value) -> String {
    h.handle(tool, &args).await[0]["text"]
        .as_str()
        .expect("text block")
        .to_string()
}

/// A site with every host up and a single failing service.
async fn one_critical_service() -> (MockServer, MonitoringHandler) {
    let (server, client) = fake_checkmk().await;
    mount(
        &server,
        "GET",
        "domain-types/host/collections/all",
        json!({"value": [{"extensions": {"name": "web01", "state": 0}}]}),
    )
    .await;
    let services = json!([
        service("web01", "CPU utilization", 0, "OK - 12% used", 0),
        service(
            "web01",
            "Filesystem /var",
            2,
            "CRIT - 94.1% used (89.2 of 94.8 GiB), trend: +1.2 GiB / 24 hours",
            BROKE_AT
        ),
    ]);
    mount(
        &server,
        "GET",
        "domain-types/service/collections/all",
        json!({"value": services}),
    )
    .await;
    (server, MonitoringHandler::new(client))
}

#[tokio::test]
async fn the_check_output_is_shown() {
    let (_s, h) = one_critical_service().await;
    let t = text_of(&h, "vibemk_get_current_problems", json!({})).await;
    assert!(
        t.contains("94.1% used"),
        "the output that explains the problem is missing: {t}"
    );
}

#[tokio::test]
async fn the_service_is_still_named() {
    let (_s, h) = one_critical_service().await;
    let t = text_of(&h, "vibemk_get_current_problems", json!({})).await;
    assert!(
        t.contains("web01") && t.contains("Filesystem /var") && t.contains("CRITICAL"),
        "{t}"
    );
}

#[tokio::test]
async fn healthy_services_are_not_listed() {
    let (_s, h) = one_critical_service().await;
    assert!(!text_of(&h, "vibemk_get_current_problems", json!({}))
        .await
        .contains("CPU utilization"));
}

#[tokio::test]
async fn since_when_is_answerable() {
    let (_s, h) = one_critical_service().await;
    let expected = chrono::DateTime::from_timestamp(BROKE_AT, 0)
        .expect("ts")
        .format("%Y-%m-%d %H:%M")
        .to_string();
    let t = text_of(&h, "vibemk_get_current_problems", json!({})).await;
    assert!(
        t.contains(&expected),
        "last_state_change is not surfaced: {t}"
    );
}

#[tokio::test]
async fn last_state_change_is_requested_from_checkmk() {
    let (server, h) = one_critical_service().await;
    h.handle("vibemk_get_current_problems", &json!({})).await;
    let reqs = server.received_requests().await.expect("recorded");
    let call = reqs
        .iter()
        .find(|r| r.url.path().ends_with("service/collections/all"))
        .expect("service call");
    assert!(
        call.url
            .query()
            .unwrap_or_default()
            .contains("columns=last_state_change"),
        "{:?}",
        call.url
    );
}

#[tokio::test]
async fn an_all_clear_is_reported() {
    let (server, client) = fake_checkmk().await;
    mount(
        &server,
        "GET",
        "domain-types/host/collections/all",
        json!({"value": []}),
    )
    .await;
    mount(
        &server,
        "GET",
        "domain-types/service/collections/all",
        json!({"value": []}),
    )
    .await;
    let t = text_of(
        &MonitoringHandler::new(client),
        "vibemk_get_current_problems",
        json!({}),
    )
    .await;
    assert!(t.contains("No current problems"), "{t}");
}

async fn ack_body(args: Value) -> Value {
    let (server, client) = fake_checkmk().await;
    mount(
        &server,
        "POST",
        "domain-types/acknowledge/collections/host",
        json!({}),
    )
    .await;
    MonitoringHandler::new(client)
        .handle("vibemk_acknowledge_problem", &args)
        .await;
    let reqs = server.received_requests().await.expect("recorded");
    serde_json::from_slice(&reqs.last().expect("a POST").body).expect("json body")
}

#[tokio::test]
async fn the_acknowledge_defaults_are_checkmks() {
    let body =
        ack_body(json!({"acknowledge_type": "host", "host_name": "web01", "comment": "on it"}))
            .await;
    assert_eq!(
        (body["sticky"].clone(), body["notify"].clone()),
        (json!(true), json!(true))
    );
}

#[tokio::test]
async fn the_caller_can_override_them() {
    let body = ack_body(json!({"acknowledge_type": "host", "host_name": "web01", "comment": "quietly", "sticky": false, "notify": false})).await;
    assert_eq!(
        (body["sticky"].clone(), body["notify"].clone()),
        (json!(false), json!(false))
    );
}

#[test]
fn the_flags_are_declared() {
    let tool = vibemk::tools::tool("vibemk_acknowledge_problem").expect("tool");
    for flag in ["sticky", "notify"] {
        assert!(
            tool["inputSchema"]["properties"].get(flag).is_some(),
            "{flag} is sent but never advertised"
        );
    }
}

#[tokio::test]
async fn comments_and_downtimes_read_tools_answer() {
    let (server, client) = fake_checkmk().await;
    mount(&server, "GET", "domain-types/comment/collections/host", json!({"value": [{"id": 7, "extensions": {"host_name": "web01", "comment": "hi", "author": "bob"}}]})).await;
    mount(
        &server,
        "GET",
        "domain-types/comment/collections/service",
        json!({"value": []}),
    )
    .await;
    mount(&server, "GET", "domain-types/downtime/collections/all", json!({"value": [{"id": "3", "extensions": {"host_name": "web01", "comment": "patch", "start_time": "a", "end_time": "b"}}]})).await;
    let h = MonitoringHandler::new(client);
    assert_eq!(
        text_of(&h, "vibemk_get_comments", json!({})).await,
        "💬 **Comments** (1 total):\n\n💬 HOST:7 - web01 - hi (by bob)"
    );
    assert_eq!(
        text_of(&h, "vibemk_get_downtimes", json!({})).await,
        "⏰ **Scheduled Downtimes** (1 total):\n\n⏰ ID:3 - web01 (a - b) - patch"
    );
}
