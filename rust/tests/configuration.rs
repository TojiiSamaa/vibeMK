//! Tests of the configuration handler (activation, pending changes, rules guide) against a fake CheckMK.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::configuration::{ConfigurationHandler, RULES_GUIDE};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

const PENDING: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/collections/pending_changes";
const ACTIVATE: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/actions/activate-changes/invoke";

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().expect("text block").to_string()
}

fn is_error(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "_error")
}

#[tokio::test]
async fn activation_sends_the_pending_etag_and_reports_the_run() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"etag-7\"")
                .set_body_json(json!({"value": [{"id": "c1"}, {"id": "c2"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "\"etag-7\""))
        .and(body_json(
            json!({"redirect": false, "sites": ["cmk"], "force_foreign_changes": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "act-1"})))
        .expect(1)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(!is_error(&r));
    assert_eq!(
        text(&r),
        "✅ **Changes Activation Started Successfully**\n\nActivation ID: act-1\nSites: cmk\nForce foreign changes: False\nChanges to activate: 2\n\n⏳ Activation is now running...\n💡 Check the CheckMK GUI for progress details."
    );
}

#[tokio::test]
async fn explicit_sites_and_force_are_sent_and_echoed() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"id": "c1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "*"))
        .and(body_json(
            json!({"redirect": false, "sites": ["a", "b"], "force_foreign_changes": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle(
            "vibemk_activate_changes",
            &json!({"sites": ["a", "b"], "force_foreign_changes": true}),
        )
        .await;
    assert!(
        text(&r).contains("Activation ID: unknown\nSites: a, b\nForce foreign changes: True\n"),
        "{}",
        text(&r)
    );
}

#[tokio::test]
async fn no_pending_changes_means_no_activation_call() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let h = ConfigurationHandler::new(client);
    let r = h.handle("vibemk_activate_changes", &json!({})).await;
    assert_eq!(
        text(&r),
        "ℹ️ **No Pending Changes**\n\nThere are no configuration changes to activate."
    );
    assert!(!is_error(&r));
}

#[tokio::test]
async fn a_null_body_counts_as_no_changes_instead_of_crashing() {
    // AUDIT E2/E11: data null must not blow up.
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(text(&r).starts_with("ℹ️ **No Pending Changes**"));
}

#[tokio::test]
async fn a_refused_activation_is_reported_with_the_server_detail() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"id": "c1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(
            ResponseTemplate::new(412)
                .set_body_json(json!({"title": "Precondition failed", "detail": "ETag mismatch"})),
        )
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(is_error(&r));
    assert!(
        text(&r).starts_with("❌ **Activation failed**\n\n"),
        "{}",
        text(&r)
    );
    assert!(text(&r).contains("ETag mismatch"));
}

#[tokio::test]
async fn never_activate_changes_blocks_before_any_request() {
    let (server, _) = common::fake_checkmk().await;
    let mut config = common::test_config(&server.uri());
    config.never_activate_changes = true;
    let base = format!("{}{}", server.uri(), common::API);
    let client = std::sync::Arc::new(vibemk::client::CheckMkClient::with_api_base_url(
        config, base,
    ));
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(text(&r).starts_with("🚫 **Change Activation Disabled**"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn pending_changes_are_summarised_by_kind_and_capped_at_fifteen() {
    let (server, client) = common::fake_checkmk().await;
    let mut value: Vec<Value> = (0..16).map(|i| json!({"id": format!("c{i}"), "user_id": "alice", "action_name": "create-host", "text": format!("Created host h{i}")})).collect();
    value.push(json!({"id": 99, "action_name": "edit-rule"}));
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": value})))
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_get_pending_changes", &json!({}))
        .await;
    let t = text(&r);
    assert!(t.starts_with("📋 **Pending Changes** (17 total)\n\n**Summary:** create: 16, edit: 1\n\n**Details:**\n➕ Created host h0 (by alice)\n"), "{t}");
    assert!(
        t.ends_with(
            "\n\n... and 2 more changes\n\n💡 Use 'activate_changes' to apply these changes"
        ),
        "{t}"
    );
    assert!(!t.contains("h15"));
    assert_eq!(r[1]["data"]["count"], json!(17));
    assert_eq!(
        r[1]["data"]["changes"][16],
        json!({"change_id": "99", "user": "unknown user", "text": "edit-rule"})
    );
}

#[tokio::test]
async fn an_empty_pending_list_is_structured_with_a_zero_count() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_get_pending_changes", &json!({}))
        .await;
    assert_eq!(text(&r), "ℹ️ **No Pending Changes**\n\nAll configuration changes have been activated.\nThe CheckMK configuration is up to date.");
    assert_eq!(r[1]["data"], json!({"count": 0, "changes": []}));
}

#[tokio::test]
async fn the_rules_guide_and_unknown_tools() {
    let (_server, client) = common::fake_checkmk().await;
    let h = ConfigurationHandler::new(client);
    let guide = h.handle("vibemk_checkmk_rules_guide", &json!({})).await;
    assert_eq!(text(&guide), RULES_GUIDE);
    assert!(
        RULES_GUIDE.starts_with("# Checkmk Rule Management Guide\n")
            && RULES_GUIDE.contains("## Activation workflow")
    );
    let unknown = h.handle("vibemk_nope", &json!({})).await;
    assert_eq!(
        text(&unknown),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
