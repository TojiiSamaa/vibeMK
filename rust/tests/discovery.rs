//! Port of tests/test_handlers_discovery.py (8 tests) plus one test per other discovery tool path.
//! Discovery must not remove services nobody asked it to remove; a mode nobody mapped is refused.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::DiscoveryHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BULK: &str = "/domain-types/discovery_run/actions/bulk-discovery-start/invoke";
const START: &str = "/domain-types/service_discovery_run/actions/start/invoke";

async fn setup() -> (MockServer, DiscoveryHandler) {
    let (server, client) = fake_checkmk().await;
    (server, DiscoveryHandler::new(client))
}

async fn mount(server: &MockServer, verb: &str, p: &str, template: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(format!("{API}{p}")))
        .respond_with(template)
        .mount(server)
        .await;
}

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap_or_default().to_string()
}

/// JSON body of the nth request the fake server received on `p`.
async fn sent_body(server: &MockServer, p: &str) -> Value {
    let full = format!("{API}{p}");
    let requests = server.received_requests().await.unwrap();
    let r = requests
        .iter()
        .find(|r| r.url.path() == full)
        .unwrap_or_else(|| panic!("no request on {full}"));
    serde_json::from_slice(&r.body).unwrap()
}

async fn bulk_options(args: Value) -> Value {
    let (server, h) = setup().await;
    mount(
        &server,
        "POST",
        BULK,
        ResponseTemplate::new(200).set_body_json(json!({"id": "job-1"})),
    )
    .await;
    h.handle("vibemk_start_bulk_discovery", &args).await;
    sent_body(&server, BULK).await["options"].clone()
}

// ---------- TestBulkDiscoveryDefaults ----------

#[tokio::test]
async fn does_not_remove_vanished_services_by_default() {
    assert_eq!(
        bulk_options(json!({"hostnames": ["web01"]})).await["remove_vanished_services"],
        false
    );
}

#[tokio::test]
async fn monitors_undecided_services_by_default() {
    assert_eq!(
        bulk_options(json!({"hostnames": ["web01"]})).await["monitor_undecided_services"],
        true
    );
}

#[tokio::test]
async fn removal_can_still_be_asked_for() {
    let o = bulk_options(
        json!({"hostnames": ["web01"], "options": {"remove_vanished_services": true}}),
    )
    .await;
    assert_eq!(o["remove_vanished_services"], true);
}

#[tokio::test]
async fn label_updates_are_not_forced_on() {
    let o = bulk_options(json!({"hostnames": ["web01"]})).await;
    assert_eq!(
        (&o["update_service_labels"], &o["update_host_labels"]),
        (&json!(false), &json!(false))
    );
}

// ---------- TestDiscoveryModes ----------

#[tokio::test]
async fn an_unknown_mode_is_refused_and_nothing_is_sent() {
    let (server, h) = setup().await;
    let out = h
        .handle(
            "vibemk_start_service_discovery",
            &json!({"host_name": "web01", "mode": "scorched_earth"}),
        )
        .await;
    assert!(text(&out).contains('❌'));
    assert_eq!(out[1]["type"], "_error");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_known_mode_falls_back_to_bulk_with_its_options() {
    let (server, h) = setup().await;
    mount(&server, "POST", START, ResponseTemplate::new(500)).await;
    mount(
        &server,
        "POST",
        BULK,
        ResponseTemplate::new(200).set_body_json(json!({"id": "job-7"})),
    )
    .await;
    let out = h
        .handle(
            "vibemk_start_service_discovery",
            &json!({"host_name": "web01", "mode": "refresh"}),
        )
        .await;
    assert!(
        text(&out).starts_with("✅ **Service Discovery Started** (via bulk discovery)"),
        "{}",
        text(&out)
    );
    assert!(text(&out).contains("Job ID: job-7"));
    let body = sent_body(&server, BULK).await;
    assert_eq!(body["options"]["monitor_undecided_services"], true);
    assert_eq!(
        (
            &body["do_full_scan"],
            &body["bulk_size"],
            &body["hostnames"]
        ),
        (&json!(true), &json!(1), &json!(["web01"]))
    );
}

// ---------- TestModeTableMatchesSchema ----------

#[tokio::test]
async fn the_mode_table_and_the_advertised_enum_are_the_same_set() {
    let schema = vibemk::tools::tool("vibemk_start_service_discovery").expect("tool in catalogue");
    let mut advertised: Vec<String> = schema["inputSchema"]["properties"]["mode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    advertised.sort();
    // The refusal lists every mapped mode: it must equal the enum (mapped-but-not-advertised and the reverse).
    let (_server, h) = setup().await;
    let out = h
        .handle(
            "vibemk_start_service_discovery",
            &json!({"host_name": "web01", "mode": "zzz"}),
        )
        .await;
    let listed = text(&out).split_once("one of ").unwrap().1.to_string();
    let expected = format!(
        "[{}]",
        advertised
            .iter()
            .map(|m| format!("'{m}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert_eq!(listed, expected);
    // And every advertised mode is accepted (the start call is answered, no refusal).
    for mode in &advertised {
        let (server, h) = setup().await;
        mount(&server, "POST", START, ResponseTemplate::new(204)).await;
        let out = h
            .handle(
                "vibemk_start_service_discovery",
                &json!({"host_name": "web01", "mode": mode}),
            )
            .await;
        assert!(
            text(&out).starts_with("✅ **Service Discovery Completed**"),
            "{mode}: {}",
            text(&out)
        );
    }
}

// ---------- the other tools ----------

#[tokio::test]
async fn a_started_job_answers_303_and_is_reported_as_running() {
    let (server, h) = setup().await;
    mount(
        &server,
        "POST",
        START,
        ResponseTemplate::new(303).insert_header("Location", "/wait"),
    )
    .await;
    let out = h
        .handle(
            "vibemk_start_service_discovery",
            &json!({"host_name": "web01"}),
        )
        .await;
    assert!(
        text(&out)
            .starts_with("✅ **Service Discovery Started**\n\nHost: **web01**\nMode: refresh"),
        "{}",
        text(&out)
    );
}

#[tokio::test]
async fn a_409_means_a_discovery_is_already_running() {
    let (server, h) = setup().await;
    mount(
        &server,
        "POST",
        START,
        ResponseTemplate::new(409).set_body_json(json!({"title": "Conflict", "detail": "busy"})),
    )
    .await;
    let out = h
        .handle(
            "vibemk_start_service_discovery",
            &json!({"host_name": "web01"}),
        )
        .await;
    assert!(text(&out).starts_with("ℹ️ **Service Discovery already running**"));
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() != format!("{API}{BULK}")));
}

#[tokio::test]
async fn bulk_start_reports_the_job_and_caps_the_host_list_at_five() {
    let (server, h) = setup().await;
    mount(
        &server,
        "POST",
        BULK,
        ResponseTemplate::new(200).set_body_json(json!({"id": "job-1"})),
    )
    .await;
    let hosts = json!(["a", "b", "c", "d", "e", "f"]);
    let out = h
        .handle("vibemk_start_bulk_discovery", &json!({"hostnames": hosts}))
        .await;
    let t = text(&out);
    assert!(
        t.contains("Job ID: **job-1**\nHosts: 6 hosts\n  • a, b, c, d, e...\n"),
        "{t}"
    );
    assert!(t.contains("  • Full scan: True\n  • Bulk size: 10\n  • Ignore errors: True\n  • Monitor undecided: True\n  • Remove vanished: False"));
}

#[tokio::test]
async fn missing_arguments_are_refused() {
    let (_s, h) = setup().await;
    assert_eq!(
        text(&h.handle("vibemk_start_bulk_discovery", &json!({})).await),
        "❌ Error: hostnames list is required"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_get_bulk_discovery_status", &json!({}))
                .await
        ),
        "❌ Error: job_id is required"
    );
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ Unknown discovery tool: vibemk_nope"
    );
}

#[tokio::test]
async fn discovery_status_counts_services_by_state() {
    let (server, h) = setup().await;
    let body = json!({"extensions": {"check_table": [
        {"state": "new", "service_name": "CPU load"}, {"state": "unchanged"}, {"state": "vanished", "service_name": "Old"}],
        "host_labels": {"os": "linux"}}});
    mount(
        &server,
        "GET",
        "/objects/service_discovery/web01",
        ResponseTemplate::new(200).set_body_json(body),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_get_discovery_status",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert_eq!(
        t,
        "📊 **Service Discovery Status**\n\nHost: **web01**\n\n📋 **Service Summary:**\n  • New services: 1\n  • Unchanged services: 1\n  • Vanished services: 1\n  • Total services: 3\n\n🏷️ **Host Labels:** 1 labels\n\n🆕 **New Services Found:**\n  • CPU load\n\n👻 **Vanished Services:**\n  • Old\n\n"
    );
}

#[tokio::test]
async fn bulk_status_hides_lock_chatter_and_shows_exceptions() {
    let (server, h) = setup().await;
    let body = json!({"extensions": {"status": {"state": "finished", "log_info": {
        "JobProgressUpdate": ["Waiting to acquire lock", "Acquired lock", "Host web01 done"], "JobException": ["boom"]}}}});
    mount(
        &server,
        "GET",
        "/objects/background_job/job-1",
        ResponseTemplate::new(200).set_body_json(body),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_get_bulk_discovery_status",
            &json!({"job_id": "job-1"}),
        )
        .await,
    );
    assert_eq!(
        t,
        "✅ **Bulk Discovery Job Status**\n\nJob ID: **job-1**\nState: FINISHED\n\n📊 **Progress:**\n  • Host web01 done\n\n⚠️ boom\n💡 Use 'get_discovery_status' on individual hosts for detailed results."
    );
}

/// 2.3 has no objects/background_job route: a real 404 there falls back to discovery_run,
/// whose progress sits under extensions.logs.progress.
#[tokio::test]
async fn bulk_status_falls_back_to_discovery_run_on_a_404() {
    let (server, h) = setup().await;
    mount(
        &server,
        "GET",
        "/objects/background_job/job-2",
        ResponseTemplate::new(404).set_body_json(json!({"title": "Not Found", "status": 404})),
    )
    .await;
    let body = json!({"extensions": {"state": "running", "logs": {"progress": ["Acquired lock", "Host web01 done"]}}});
    mount(
        &server,
        "GET",
        "/objects/discovery_run/job-2",
        ResponseTemplate::new(200).set_body_json(body),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_get_bulk_discovery_status",
            &json!({"job_id": "job-2"}),
        )
        .await,
    );
    assert!(t.contains("State: RUNNING"), "{t}");
    assert!(
        t.contains("  • Host web01 done") && !t.contains("Acquired lock"),
        "{t}"
    );
}

#[tokio::test]
async fn bulk_status_does_not_fall_back_on_other_errors() {
    let (server, h) = setup().await;
    mount(
        &server,
        "GET",
        "/objects/background_job/job-3",
        ResponseTemplate::new(500).set_body_json(json!({"title": "boom"})),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_get_bulk_discovery_status",
            &json!({"job_id": "job-3"}),
        )
        .await,
    );
    assert!(
        t.starts_with("❌ Error getting bulk discovery status"),
        "{t}"
    );
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.url.path().contains("discovery_run")));
}

/// 2.5 answers 303 to objects/background_job/<id>: the id is read from Location, not followed.
#[tokio::test]
async fn bulk_start_reads_the_job_id_from_a_303() {
    let (server, h) = setup().await;
    mount(
        &server,
        "POST",
        BULK,
        ResponseTemplate::new(303)
            .insert_header("Location", format!("{API}/objects/background_job/bulk-9")),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_start_bulk_discovery",
            &json!({"hostnames": ["web01"]}),
        )
        .await,
    );
    assert!(t.contains("Job ID: **bulk-9**"), "{t}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn wait_polls_until_the_job_is_done() {
    let (server, h) = setup().await;
    let p = "/objects/service_discovery_run/web01/actions/wait-for-completion/invoke";
    Mock::given(method("GET"))
        .and(path(format!("{API}{p}")))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", p))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount(&server, "GET", p, ResponseTemplate::new(204)).await;
    let t = text(
        &h.handle("vibemk_wait_for_discovery", &json!({"host_name": "web01"}))
            .await,
    );
    assert!(t.starts_with("✅ **Discovery Completed**"), "{t}");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn wait_gives_up_after_the_timeout() {
    let (server, h) = setup().await;
    let p = "/objects/service_discovery_run/web01/actions/wait-for-completion/invoke";
    mount(
        &server,
        "GET",
        p,
        ResponseTemplate::new(302).insert_header("Location", p),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_wait_for_discovery",
            &json!({"host_name": "web01", "timeout": 0}),
        )
        .await,
    );
    assert!(
        t.starts_with("⏳ **Discovery still running**") && t.contains("after 0s"),
        "{t}"
    );
}

#[tokio::test]
async fn background_job_shows_state_and_logs() {
    let (server, h) = setup().await;
    // The shape 2.3 and 2.5 send: no start or end time.
    let body = json!({"extensions": {"active": true, "state": "running",
        "logs": {"result": [], "progress": ["Starting job...", "Completed."]}}});
    mount(
        &server,
        "GET",
        "/objects/service_discovery_run/web01",
        ResponseTemplate::new(200).set_body_json(body),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_get_discovery_background_job",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert_eq!(t, "🔄 **Discovery Background Job**\n\nHost: **web01**\nState: RUNNING\nActive: yes\n\n• Starting job...\n• Completed.\n\n💡 Use 'get_discovery_status' to see discovery results.");
}

#[tokio::test]
async fn an_api_error_becomes_an_error_block() {
    let (server, h) = setup().await;
    mount(
        &server,
        "GET",
        "/objects/service_discovery/web01",
        ResponseTemplate::new(404)
            .set_body_json(json!({"title": "Not Found", "detail": "no such host"})),
    )
    .await;
    let out = h
        .handle(
            "vibemk_get_discovery_status",
            &json!({"host_name": "web01"}),
        )
        .await;
    assert!(
        text(&out).starts_with("❌ Error getting discovery status: "),
        "{}",
        text(&out)
    );
    assert_eq!(out[1]["type"], "_error");
}
