//! Port of tests/test_handlers_notifications.py (11 tests), against a fake CheckMK.
//! CheckMK 2.4 shapes: deletion is a POST action, no endpoint declares an ETag, so no If-Match is sent.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::notifications::NotificationHandler;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COLLECTION: &str = "domain-types/notification_rule/collections/all";

fn rule() -> Value {
    json!({"id": "1", "extensions": {"rule_config": {
        "rule_properties": {"description": "Notify admins by email"},
        "disabled": false,
        "notification_method": {"notify_plugin": {"option": "mail"}}
    }}})
}

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

async fn setup() -> (MockServer, NotificationHandler) {
    let (server, client) = common::fake_checkmk().await;
    (server, NotificationHandler::new(client))
}

fn url(p: &str) -> String {
    format!("{}/{p}", common::API)
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

// ---------- listing ----------

#[tokio::test]
async fn lists_rules_from_the_collection_endpoint() {
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(url(COLLECTION)))
        .respond_with(ok(json!({"value": [rule()]})))
        .expect(1)
        .mount(&server)
        .await;
    let out = h.handle("vibemk_get_notification_rules", &json!({})).await;
    assert_eq!(text(&out), "📢 **Notification Rules** (1)\n\n• **Notify admins by email** — id `1`, enabled, via `mail`");
}

#[tokio::test]
async fn reports_an_empty_configuration() {
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .respond_with(ok(json!({"value": []})))
        .mount(&server)
        .await;
    let out = h.handle("vibemk_get_notification_rules", &json!({})).await;
    assert_eq!(text(&out), "ℹ️ **No notification rules configured**\n\n**hint**: CheckMK falls back to its built-in notification behaviour");
}

#[tokio::test]
async fn shows_a_single_rule_by_id() {
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(url("objects/notification_rule/1")))
        .respond_with(ok(rule()))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle("vibemk_get_notification_rule", &json!({"rule_id": "1"}))
        .await;
    let t = text(&out);
    assert!(t.starts_with("📢 **Notification Rule `1`**\n\nDescription: Notify admins by email\n\nFull rule_config"), "{t}");
    assert!(
        t.contains("\"rule_properties\": {\n    \"description\": \"Notify admins by email\"\n  }"),
        "{t}"
    );
}

#[tokio::test]
async fn show_requires_a_rule_id() {
    let (server, h) = setup().await;
    let out = h.handle("vibemk_get_notification_rule", &json!({})).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nrule_id is required"
    );
}

// ---------- deletion ----------

#[tokio::test]
async fn delete_posts_to_the_delete_action() {
    let (server, h) = setup().await;
    Mock::given(method("POST"))
        .and(path(url(
            "objects/notification_rule/7/actions/delete/invoke",
        )))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle("vibemk_delete_notification_rule", &json!({"rule_id": "7"}))
        .await;
    assert_eq!(
        text(&out),
        "✅ **Notification rule '7' deleted**\n\n**next step**: activate changes to apply it"
    );
}

#[tokio::test]
async fn delete_requires_a_rule_id() {
    let (server, h) = setup().await;
    let out = h
        .handle("vibemk_delete_notification_rule", &json!({}))
        .await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(text(&out).contains("rule_id"));
}

// ---------- create and update ----------

#[tokio::test]
async fn create_posts_the_rule_config_to_the_collection() {
    let (server, h) = setup().await;
    let config = json!({"properties": {"description": "New rule"}});
    Mock::given(method("POST"))
        .and(path(url(COLLECTION)))
        .and(body_json(json!({"rule_config": config})))
        .respond_with(ok(rule()))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_create_notification_rule",
            &json!({"rule_config": config}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Notification rule created**\n\n**rule_id**: 1\n**next step**: activate changes to apply it");
}

#[tokio::test]
async fn update_puts_the_rule_config_to_the_object() {
    let (server, h) = setup().await;
    let config = json!({"properties": {"description": "Changed"}});
    Mock::given(method("PUT"))
        .and(path(url("objects/notification_rule/3")))
        .and(body_json(json!({"rule_config": config})))
        .respond_with(ok(rule()))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_update_notification_rule",
            &json!({"rule_id": "3", "rule_config": config}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Notification rule '3' updated**\n\n**next step**: activate changes to apply it"
    );
}

#[tokio::test]
async fn create_requires_a_rule_config() {
    let (server, h) = setup().await;
    let out = h
        .handle("vibemk_create_notification_rule", &json!({}))
        .await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(text(&out).contains("rule_config"));
}

#[tokio::test]
async fn update_requires_a_rule_id() {
    let (server, h) = setup().await;
    let out = h
        .handle(
            "vibemk_update_notification_rule",
            &json!({"rule_config": {}}),
        )
        .await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(text(&out).contains("rule_id"));
}

// ---------- unknown tool ----------

#[tokio::test]
async fn reports_an_unknown_tool() {
    let (_server, h) = setup().await;
    let out = h.handle("vibemk_not_a_notification_tool", &json!({})).await;
    assert!(text(&out).to_lowercase().contains("not supported"));
}
