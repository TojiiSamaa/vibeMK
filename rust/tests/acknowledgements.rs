//! Port of tests/test_handlers_acknowledgements.py (entry_type 4, not guessed from comment text),
//! plus the acknowledge and remove paths.

mod common;

use serde_json::{json, Value};
use vibemk::handlers::acknowledgements::AcknowledgementHandler;
use vibemk::handlers::base::Handler;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const COMMENTS: &str = "domain-types/comment/collections/all";
const DELETE: &str = "domain-types/comment/actions/delete/invoke";

async fn handler() -> (MockServer, AcknowledgementHandler) {
    let (server, client) = fake_checkmk().await;
    (server, AcknowledgementHandler::new(client))
}

async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn requests(server: &MockServer, m: &str, p: &str) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == m && r.url.path() == format!("{API}/{p}"))
        .collect()
}

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap_or_default().to_string()
}

fn comment(id: &str, host: &str, is_service: bool) -> Value {
    json!({"id": id, "extensions": {"host_name": host, "comment": "disk replaced, waiting for RMA", "persistent": false, "author": "ops", "is_service": is_service}})
}

#[tokio::test]
async fn asks_checkmk_for_the_acknowledgement_entry_type() {
    let (server, h) = handler().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/{COMMENTS}")))
        .and(query_param(
            "query",
            r#"{"op":"=","left":"entry_type","right":"4"}"#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    h.handle("vibemk_list_acknowledgements", &json!({})).await;
}

#[tokio::test]
async fn does_not_invent_acknowledgements_from_comment_text() {
    let (server, h) = handler().await;
    mount(&server, "GET", COMMENTS, 200, json!({"value": []})).await;
    let out = h.handle("vibemk_list_acknowledgements", &json!({})).await;
    assert!(text(&out).contains("No active acknowledgements"));
}

#[tokio::test]
async fn lists_what_checkmk_returns() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        COMMENTS,
        200,
        json!({"value": [comment("42", "web01", false)]}),
    )
    .await;
    let t = text(&h.handle("vibemk_list_acknowledgements", &json!({})).await);
    assert_eq!(t, "✅ **Current Acknowledgements**\n\n**1. Acknowledgement #42**\nType: Host\nHost: **web01**\nComment: disk replaced, waiting for RMA\nAuthor: ops\nEntry time: Unknown\nPersistent: False\n");
}

#[tokio::test]
async fn a_host_acknowledgement_on_24_sends_expiry() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        "version",
        200,
        json!({"versions": {"checkmk": "2.4.0p1"}}),
    )
    .await;
    mount(
        &server,
        "POST",
        "domain-types/acknowledge/collections/host",
        204,
        json!({}),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_acknowledge_host_problem",
            &json!({"host_name": "web01", "expire_on": "2026-12-01T00:00:00Z"}),
        )
        .await,
    );
    let sent: Value = requests(&server, "POST", "domain-types/acknowledge/collections/host").await
        [0]
    .body_json()
    .unwrap();
    assert_eq!(sent["expire_on"], "2026-12-01T00:00:00Z");
    assert_eq!(sent["comment"], "Problem acknowledged via vibeMK");
    assert_eq!(t, "✅ **Host Acknowledgement Created**\n\nHost: **web01**\nComment: Problem acknowledged via vibeMK\nSticky: False\nPersistent: False\nNotify: False\nExpire on: 2026-12-01T00:00:00Z\n\nCheckMK Version: 2.4+\n\nThe host problem has been acknowledged and will not generate further alerts.");
}

#[tokio::test]
async fn a_service_acknowledgement_on_23_has_no_expiry() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        "version",
        200,
        json!({"versions": {"checkmk": "2.3.0"}}),
    )
    .await;
    mount(
        &server,
        "POST",
        "domain-types/acknowledge/collections/service",
        204,
        json!({}),
    )
    .await;
    let args = json!({"host_name": "web01", "service_description": "CPU", "expire_on": "x"});
    let t = text(&h.handle("vibemk_acknowledge_service_problem", &args).await);
    let sent: Value = requests(
        &server,
        "POST",
        "domain-types/acknowledge/collections/service",
    )
    .await[0]
        .body_json()
        .unwrap();
    assert!(sent.get("expire_on").is_none());
    assert_eq!(sent["service_description"], "CPU");
    assert!(
        t.contains("Service: **CPU**\nComment:") && t.contains("CheckMK Version: 2.3"),
        "{t}"
    );
}

#[tokio::test]
async fn missing_arguments_and_unknown_tools_are_refused() {
    let (_server, h) = handler().await;
    assert_eq!(
        text(
            &h.handle(
                "vibemk_acknowledge_service_problem",
                &json!({"host_name": "a"})
            )
            .await
        ),
        "❌ Error: host_name and service_description are required"
    );
    assert_eq!(
        text(&h.handle("nope", &json!({})).await),
        "❌ Unknown acknowledgement tool: nope"
    );
}

#[tokio::test]
async fn remove_by_host_deletes_only_matching_kind_by_integer_id() {
    let (server, h) = handler().await;
    mount(&server, "GET", COMMENTS, 200, json!({"value": [comment("7", "web01", false), comment("8", "web01", true), comment("9", "db01", false), comment("x", "web01", false)]})).await;
    mount(&server, "POST", DELETE, 204, json!({})).await;
    let t = text(
        &h.handle(
            "vibemk_remove_acknowledgement",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert_eq!(t, "✅ Successfully removed 1 acknowledgements for web01");
    let sent: Value = requests(&server, "POST", DELETE).await[0]
        .body_json()
        .unwrap();
    assert_eq!(
        sent,
        json!({"delete_type": "by_id", "comment_id": 7, "site_id": "cmk"})
    );
}

#[tokio::test]
async fn remove_by_id_and_without_target() {
    let (server, h) = handler().await;
    mount(&server, "DELETE", "objects/comment/42", 204, json!({})).await;
    assert_eq!(
        text(
            &h.handle(
                "vibemk_remove_acknowledgement",
                &json!({"acknowledgement_id": "42"})
            )
            .await
        ),
        "✅ Successfully removed acknowledgement #42"
    );
    let none = h.handle("vibemk_remove_acknowledgement", &json!({})).await;
    assert!(text(&none).contains("acknowledgement_id, host_name, or comment_pattern is required"));
    assert_eq!(none[1]["type"], "_error");
}
