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

// ---- a 412 rereads the pending changes once (test_activate_changes_retry.py) ----

/// Pending changes answered once with `first`, then always with `then` (ETag, changes).
async fn pending_twice(server: &wiremock::MockServer, first: (&str, Value), then: (&str, Value)) {
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", first.0)
                .set_body_json(json!({"value": first.1})),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", then.0)
                .set_body_json(json!({"value": then.1})),
        )
        .mount(server)
        .await;
}

fn stale() -> ResponseTemplate {
    ResponseTemplate::new(412).set_body_json(json!({"title": "Precondition failed"}))
}

#[tokio::test]
async fn a_412_then_no_pending_changes_is_not_an_error() {
    let (server, client) = common::fake_checkmk().await;
    pending_twice(
        &server,
        ("\"a\"", json!([{"id": "c1"}])),
        ("\"b\"", json!([])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(stale())
        .expect(1)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(!is_error(&r));
    assert!(text(&r).contains("No Pending Changes"), "{}", text(&r));
}

#[tokio::test]
async fn a_412_is_retried_with_the_fresh_etag() {
    let (server, client) = common::fake_checkmk().await;
    pending_twice(
        &server,
        ("\"a\"", json!([{"id": "c1"}])),
        ("\"c\"", json!([{"id": "c1"}])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "\"a\""))
        .respond_with(stale())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "\"c\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "act-1"})))
        .expect(1)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("act-1"), "{}", text(&r));
}

#[tokio::test]
async fn a_second_412_is_reported() {
    let (server, client) = common::fake_checkmk().await;
    pending_twice(
        &server,
        ("\"a\"", json!([{"id": "c1"}])),
        ("\"a\"", json!([{"id": "c1"}])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(stale())
        .expect(2)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(is_error(&r));
    assert!(text(&r).contains("Activation failed"), "{}", text(&r));
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

const RUNNING: &str = "/cmk/check_mk/api/1.0/domain-types/activation_run/collections/running";

/// Body of collections/running copied from CheckMK 2.3.0p52 (127.0.0.1:5080).
fn running_body(title: &str, is_running: bool) -> Value {
    json!({"id": "activation_run", "domainType": "activation_run", "value": [{
        "domainType": "activation_run",
        "id": "6389bbe2-d9a0-45e3-be4f-e0e137baef31",
        "title": title,
        "members": {},
        "extensions": {"sites": ["cmk"], "is_running": is_running, "force_foreign_changes": true,
                       "time_started": "2026-11-06T21:38:12.721958+00:00", "changes": []}
    }]})
}

/// 2.3 accepts a second activate-changes while a run is in progress, then
/// marks the site "Locked" and leaves the later changes pending: the POST
/// must wait for the running run to finish.
#[tokio::test]
async fn activation_waits_for_the_run_already_in_progress() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(RUNNING))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(running_body("Activation status: In progress.", true)),
        )
        .up_to_n_times(2)
        .expect(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(RUNNING))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(running_body("Activation status: Complete.", false)),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"etag-9\"")
                .set_body_json(json!({"value": [{"id": "85822ffa-1673-4999-9e6b-7f66c35a75dd", "user_id": "cmkadmin", "action_name": "new-rule"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "\"etag-9\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "d072208a"})))
        .expect(1)
        .mount(&server)
        .await;
    let r = ConfigurationHandler::new(client)
        .handle("vibemk_activate_changes", &json!({}))
        .await;
    assert!(text(&r).contains("Activation ID: d072208a"), "{}", text(&r));
    // The three GETs on collections/running all happened before the POST.
    let reqs = server.received_requests().await.expect("recording");
    let order: Vec<String> = reqs
        .iter()
        .map(|q| format!("{} {}", q.method, q.url.path()))
        .collect();
    let post_at = order
        .iter()
        .position(|o| o.starts_with("POST"))
        .expect("a POST");
    assert_eq!(
        order[..post_at]
            .iter()
            .filter(|o| o.ends_with("/running"))
            .count(),
        3,
        "{order:?}"
    );
}

/// The activation a write tool runs on its own (activate_changes=true,
/// set_process_thresholds) used to POST straight away and hit the same
/// "Locked" refusal on 2.3: it must wait for the run in progress too.
#[tokio::test]
async fn write_tools_activation_also_waits_for_the_run_in_progress() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(RUNNING))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(running_body("Activation status: In progress.", true)),
        )
        .up_to_n_times(1)
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(RUNNING))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(running_body("Activation status: Complete.", false)),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"etag-9\"")
                .set_body_json(json!({"value": [{"id": "85822ffa-1673-4999-9e6b-7f66c35a75dd"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "act-3"})))
        .expect(1)
        .mount(&server)
        .await;
    let line = vibemk::handlers::base::Base::new(client)
        .run_activation()
        .await;
    assert!(line.starts_with("✅ Activation started"), "{line}");
    let reqs = server.received_requests().await.expect("recording");
    let order: Vec<String> = reqs
        .iter()
        .map(|q| format!("{} {}", q.method, q.url.path()))
        .collect();
    let post_at = order
        .iter()
        .position(|o| o.starts_with("POST"))
        .expect("a POST");
    assert_eq!(
        order[..post_at]
            .iter()
            .filter(|o| o.ends_with("/running"))
            .count(),
        2,
        "{order:?}"
    );
}
