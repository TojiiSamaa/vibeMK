//! tools/call of vibemk_get_audit_log against a fake CheckMK.
mod common;

use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use vibemk::handlers::audit_log::AuditLogHandler;
use vibemk::handlers::base::Handler;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const AUDIT: &str = "/cmk/check_mk/api/1.0/domain-types/audit_log/collections/all";

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn entries_are_listed_with_time_user_action_object_and_title() {
    let (server, client) = common::fake_checkmk().await;
    let ts = 1_700_000_000_i64;
    let body = json!({"value": [
        {"title": "Created host web01", "extensions": {"time": ts, "user_id": "admin", "action": "create-host", "object_type": "host", "object_name": "web01"}},
        {"extensions": {}},
    ]});
    Mock::given(method("GET"))
        .and(path(AUDIT))
        .and(query_param("date", "2026-01-02"))
        .and(query_param("user_id", "admin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let out = AuditLogHandler::new(client)
        .handle(
            "vibemk_get_audit_log",
            &json!({"date": "2026-01-02", "user_id": "admin"}),
        )
        .await;
    let hhmmss = Local.timestamp_opt(ts, 0).unwrap().format("%H:%M:%S");
    assert_eq!(
        text(&out),
        format!("📋 **Audit Log — 2026-01-02 (2 entries)**\n\n• `{hhmmss}` **admin** — create-host [host: web01]\n  Created host web01\n• `?` **?** — ?")
    );
}

#[tokio::test]
async fn limit_cuts_the_list_and_says_how_many_are_left() {
    let (server, client) = common::fake_checkmk().await;
    let entries: Vec<Value> = (0..3)
        .map(|i| json!({"extensions": {"user_id": format!("u{i}"), "action": "a"}}))
        .collect();
    Mock::given(method("GET"))
        .and(path(AUDIT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": entries})))
        .mount(&server)
        .await;
    let out = AuditLogHandler::new(client)
        .handle(
            "vibemk_get_audit_log",
            &json!({"date": "2026-01-02", "limit": 1}),
        )
        .await;
    assert_eq!(text(&out), "📋 **Audit Log — 2026-01-02 (3 entries)**\n\n• `?` **u0** — a\n\n…2 more entries (raise limit to see them).");
}

#[tokio::test]
async fn no_entries_and_default_date_is_today() {
    let (server, client) = common::fake_checkmk().await;
    let today = Local::now().format("%Y-%m-%d").to_string();
    Mock::given(method("GET"))
        .and(path(AUDIT))
        .and(query_param("date", today.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    let out = AuditLogHandler::new(client)
        .handle("vibemk_get_audit_log", &json!({}))
        .await;
    assert_eq!(text(&out), format!("📋 No audit log entries for {today}."));
}

#[tokio::test]
async fn errors_and_unknown_tools_are_reported() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let h = AuditLogHandler::new(client);
    let out = h.handle("vibemk_get_audit_log", &json!({})).await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert_eq!(out[1]["type"], "_error");
    assert_eq!(
        text(
            &h.handle("vibemk_get_audit_log", &json!({"limit": "x"}))
                .await
        ),
        "❌ **Unexpected Error**\n\ninvalid literal for int() with base 10: 'x'"
    );
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
