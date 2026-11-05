//! Port of tests/test_handlers_rules.py: move vocabulary, create honouring `position`, value_raw quoting.

mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::rules::RulesHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const RULE_ID: &str = "f8b74720-a454-4242-99c4-62994ef0f2bf";
const TARGET_ID: &str = "0e3b1a44-9a2e-4c17-8f5d-2b6c1d0e7a93";
const RULE: &str = "objects/rule/f8b74720-a454-4242-99c4-62994ef0f2bf";
const MOVE: &str = "objects/rule/f8b74720-a454-4242-99c4-62994ef0f2bf/actions/move/invoke";
const CREATE: &str = "domain-types/rule/collections/all";

async fn handler() -> (MockServer, RulesHandler) {
    let (server, client) = fake_checkmk().await;
    (server, RulesHandler::new(client))
}

async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_json(body)
                .insert_header("ETag", "\"abc\""),
        )
        .mount(server)
        .await;
}

async fn mount_rule(server: &MockServer, folder: &str) {
    mount(
        server,
        "GET",
        RULE,
        200,
        json!({"id": RULE_ID, "extensions": {"folder": folder}}),
    )
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

/// The body of the one move call, or fail loudly.
async fn move_payload(server: &MockServer) -> Value {
    let calls = requests(server, "POST", MOVE).await;
    assert_eq!(calls.len(), 1, "expected exactly one move call");
    calls[0].body_json().unwrap()
}

fn text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_folder_position_is_translated() {
    for (given, expected) in [
        ("top", "top_of_folder"),
        ("bottom", "bottom_of_folder"),
        ("top_of_folder", "top_of_folder"),
        ("bottom_of_folder", "bottom_of_folder"),
    ] {
        let (server, h) = handler().await;
        mount_rule(&server, "~servers~linux").await;
        mount(&server, "POST", MOVE, 200, json!({})).await;
        h.handle(
            "vibemk_move_rule",
            &json!({"rule_id": RULE_ID, "position": given}),
        )
        .await;
        assert_eq!(move_payload(&server).await["position"], expected);
    }
}

#[tokio::test]
async fn a_folder_position_carries_the_rules_own_folder() {
    let (server, h) = handler().await;
    mount_rule(&server, "~servers~linux").await;
    mount(&server, "POST", MOVE, 200, json!({})).await;
    h.handle(
        "vibemk_move_rule",
        &json!({"rule_id": RULE_ID, "position": "top"}),
    )
    .await;
    assert_eq!(move_payload(&server).await["folder"], "~servers~linux");
}

#[tokio::test]
async fn a_relative_position_names_its_target_rule_id() {
    for (given, expected) in [
        ("before", "before_specific_rule"),
        ("after", "after_specific_rule"),
    ] {
        let (server, h) = handler().await;
        mount_rule(&server, "~").await;
        mount(&server, "POST", MOVE, 200, json!({})).await;
        h.handle(
            "vibemk_move_rule",
            &json!({"rule_id": RULE_ID, "position": given, "target_rule_id": TARGET_ID}),
        )
        .await;
        assert_eq!(
            move_payload(&server).await,
            json!({"position": expected, "rule_id": TARGET_ID})
        );
    }
}

#[tokio::test]
async fn an_unknown_position_never_reaches_checkmk() {
    let (server, h) = handler().await;
    mount_rule(&server, "~").await;
    let out = h
        .handle(
            "vibemk_move_rule",
            &json!({"rule_id": RULE_ID, "position": "sideways"}),
        )
        .await;
    assert!(requests(&server, "POST", MOVE).await.is_empty());
    let t = text(&out);
    assert!(t.contains("sideways") && t.contains("top_of_folder"), "{t}");
    assert_eq!(out[1]["type"], "_error");
}

#[tokio::test]
async fn a_relative_position_without_a_target_is_refused() {
    let (server, h) = handler().await;
    mount_rule(&server, "~").await;
    let out = h
        .handle(
            "vibemk_move_rule",
            &json!({"rule_id": RULE_ID, "position": "after"}),
        )
        .await;
    assert!(requests(&server, "POST", MOVE).await.is_empty());
    assert!(text(&out).contains("target_rule_id"));
}

#[tokio::test]
async fn a_move_uses_the_etag_of_the_read() {
    let (server, h) = handler().await;
    mount_rule(&server, "~").await;
    mount(&server, "POST", MOVE, 200, json!({})).await;
    let out = h
        .handle("vibemk_move_rule", &json!({"rule_id": RULE_ID}))
        .await;
    let sent = &requests(&server, "POST", MOVE).await[0];
    assert_eq!(
        sent.headers.get("if-match").unwrap().to_str().unwrap(),
        "\"abc\""
    );
    assert!(text(&out).starts_with("✅ **Rule Moved Successfully**\n\nRule ID: f8b7"));
}

async fn created(position: Option<&str>, move_status: u16) -> (MockServer, Vec<Value>) {
    let (server, h) = handler().await;
    mount_rule(&server, "~servers~linux").await;
    mount(&server, "POST", CREATE, 200, json!({"id": RULE_ID})).await;
    mount(&server, "POST", MOVE, move_status, json!({})).await;
    let mut args = json!({"ruleset_name": "checkgroup_parameters:filesystem", "rule_config": {"levels": [80.0, 90.0]}, "folder": "/servers/linux"});
    if let Some(p) = position {
        args["position"] = json!(p);
    }
    let out = h.handle("vibemk_create_rule", &args).await;
    (server, out)
}

#[tokio::test]
async fn a_requested_position_is_applied_on_create() {
    let (server, out) = created(Some("bottom"), 200).await;
    assert_eq!(move_payload(&server).await["position"], "bottom_of_folder");
    assert!(text(&out).contains("Position: bottom_of_folder"));
}

#[tokio::test]
async fn no_position_means_no_move() {
    let (server, _) = created(None, 200).await;
    assert!(requests(&server, "POST", MOVE).await.is_empty());
}

#[tokio::test]
async fn a_failed_move_is_reported_with_the_rule_id() {
    let (_server, out) = created(Some("bottom"), 400).await;
    let t = text(&out);
    assert!(t.contains(RULE_ID), "{t}");
    assert!(t.to_lowercase().contains("position"));
}

/// The literal sent as value_raw is the Python repr (what ast.literal_eval reads back).
#[tokio::test]
async fn the_literal_is_a_valid_python_repr() {
    for (config, expected) in [
        (json!({"levels": [80.0, 90.0]}), "{'levels': [80.0, 90.0]}"),
        (
            json!({"comment": "He said \"no\""}),
            "{'comment': 'He said \"no\"'}",
        ),
        (json!({"path": "it's here"}), "{'path': \"it's here\"}"),
        (
            json!({"mixed": ["say \"what\"", "plain"]}),
            "{'mixed': ['say \"what\"', 'plain']}",
        ),
        (
            json!({"apostrophe": "o'clock"}),
            "{'apostrophe': \"o'clock\"}",
        ),
        (json!(["only"]), "'only'"),
    ] {
        let (server, h) = handler().await;
        mount(&server, "POST", CREATE, 200, json!({"id": RULE_ID})).await;
        h.handle(
            "vibemk_create_rule",
            &json!({"ruleset_name": "checkgroup_parameters:filesystem", "rule_config": config}),
        )
        .await;
        let sent: Value = requests(&server, "POST", CREATE).await[0]
            .body_json()
            .unwrap();
        assert_eq!(sent["value_raw"], expected);
        assert_eq!(sent["folder"], "~");
    }
}

#[tokio::test]
async fn get_ruleset_renders_rules_and_conditions() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        CREATE,
        200,
        json!({"value": [{"id": "r1", "extensions": {
            "properties": {"comment": "c", "disabled": true}, "value_raw": "{}", "folder": "~a",
            "conditions": {"host_name": {"match_on": ["web"], "operator": "one_of"}, "host_tags": {"a": 1}}}}]}),
    )
    .await;
    let t = text(
        &h.handle("vibemk_get_ruleset", &json!({"ruleset_name": "x"}))
            .await,
    );
    assert_eq!(t, "📋 **Ruleset: x**\n\nRules (1 total):\n\n🔧 **Rule 1** (ID: r1)\n   Status: 🔒 Disabled\n   Value: {}\n   Folder: ~a\n   Conditions: Hosts: ['web'] (one_of), Tags: 1 conditions\n   Comment: c");
}

#[tokio::test]
async fn update_and_delete_use_the_rule_endpoint() {
    let (server, h) = handler().await;
    mount_rule(&server, "~").await;
    mount(&server, "PUT", RULE, 200, json!({})).await;
    mount(&server, "DELETE", RULE, 204, json!({})).await;
    let t = text(
        &h.handle(
            "vibemk_update_rule",
            &json!({"rule_id": RULE_ID, "comment": "n", "conditions": {"a": 1}}),
        )
        .await,
    );
    assert!(t.contains("Updated fields: conditions, properties"), "{t}");
    assert!(text(
        &h.handle("vibemk_delete_rule", &json!({"rule_id": RULE_ID}))
            .await
    )
    .contains("Rule Deleted Successfully"));
    let none = h
        .handle("vibemk_update_rule", &json!({"rule_id": RULE_ID}))
        .await;
    assert!(text(&none).contains("At least one field must be provided"));
}
