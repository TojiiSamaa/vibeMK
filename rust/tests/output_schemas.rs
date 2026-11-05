//! Port of tests/test_output_schemas.py: structured output for service status,
//! problems, downtimes and metrics. Every successful tools/call of a tool with an
//! outputSchema must carry structuredContent that validates against rust/tools.json.

mod common;

use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use vibemk::handlers::metrics::series_summary;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const SERVICES: &str = "domain-types/service/collections/all";
const HOSTS: &str = "domain-types/host/collections/all";
const DOWNTIMES: &str = "domain-types/downtime/collections/all";
const METRIC_GET: &str = "domain-types/metric/actions/get/invoke";

async fn dispatcher() -> (MockServer, Dispatcher) {
    let (server, client) = fake_checkmk().await;
    (server, Dispatcher::from_client(client, false))
}

async fn mount(server: &MockServer, m: &str, p: &str, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// The call succeeded and its structuredContent matches the tool's outputSchema; returns it.
async fn call_valid(d: &Dispatcher, tool: &str, args: Value) -> (Value, Value) {
    let r = d.call_tool(tool, Some(&args)).await;
    assert_eq!(r["isError"], false, "{}", r["content"][0]["text"]);
    let data = r
        .get("structuredContent")
        .unwrap_or_else(|| panic!("{tool} answered without structured content"))
        .clone();
    let schema = vibemk::tools::tool(tool)
        .and_then(|t| t.get("outputSchema"))
        .unwrap_or_else(|| panic!("{tool} has no outputSchema"));
    let validator = jsonschema::draft202012::new(schema).expect("outputSchema compiles");
    let errors: Vec<String> = validator
        .iter_errors(&data)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{tool}: {errors:?} in {data}");
    (r, data)
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ------------------------------------------------------------------ service status

#[tokio::test]
async fn a_critical_service_is_a_finding_not_a_failed_call() {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        SERVICES,
        json!({"value": [{"extensions": {"host_name": "defender", "description": "Check_MK", "state": 2, "state_type": 1,
            "plugin_output": "[agent] Communication failed", "last_check": 1790982890, "last_state_change": 1790900000}}]}),
    )
    .await;
    let (_, data) = call_valid(
        &d,
        "vibemk_get_service_status",
        json!({"host_name": "defender", "service_description": "Check_MK"}),
    )
    .await;
    assert_eq!(data["state"], "CRITICAL");
    assert_eq!(data["state_code"], 2);
    assert_eq!(data["is_hard_state"], true);
    assert_eq!(data["plugin_output"], "[agent] Communication failed");
}

#[tokio::test]
async fn the_show_service_fallback_carries_structured_content_too() {
    let (server, d) = dispatcher().await;
    mount(&server, "GET", SERVICES, json!({"value": []})).await;
    mount(
        &server,
        "GET",
        "objects/host/web01/actions/show_service/invoke",
        json!({"extensions": {"host_name": "web01", "description": "CPU load", "state": 1, "state_type": 0, "last_check": 1790983377}}),
    )
    .await;
    let (_, data) = call_valid(
        &d,
        "vibemk_get_service_status",
        json!({"host_name": "web01", "service_description": "CPU load"}),
    )
    .await;
    assert_eq!(data["state"], "WARNING");
    assert_eq!(data["is_hard_state"], false);
    assert_eq!(data["plugin_output"], "");
    assert!(data["last_state_change"].is_null());
}

// ------------------------------------------------------------------ current problems

#[tokio::test]
async fn problems_say_whether_someone_already_handles_them() {
    let (server, d) = dispatcher().await;
    mount(&server, "GET", HOSTS, json!({"value": [{"extensions": {"name": "defender", "state": 1, "acknowledged": 1, "scheduled_downtime_depth": 0}}]})).await;
    mount(
        &server,
        "GET",
        SERVICES,
        json!({"value": [
            {"extensions": {"host_name": "defender", "description": "Check_MK", "state": 2, "plugin_output": "timeout",
                "last_state_change": 1790900000, "acknowledged": 0, "scheduled_downtime_depth": 1}},
            {"extensions": {"host_name": "defender", "description": "Uptime", "state": 0}}]}),
    )
    .await;
    let (r, data) = call_valid(&d, "vibemk_get_current_problems", json!({})).await;
    assert_eq!(data["total"], 2);
    assert_eq!(
        data["host_problems"],
        json!([{"host_name": "defender", "state": "DOWN", "state_code": 1, "acknowledged": true, "in_downtime": false}])
    );
    assert_eq!(data["service_problems"][0]["in_downtime"], true);
    let t = text(&r);
    assert!(
        t.contains("defender - DOWN (acknowledged)"),
        "clients that pass only text must see it as well: {t}"
    );
    assert!(t.contains("CRITICAL (in downtime)"), "{t}");
}

#[tokio::test]
async fn no_problems_still_carries_structured_content() {
    let (server, d) = dispatcher().await;
    mount(&server, "GET", HOSTS, json!({"value": []})).await;
    mount(&server, "GET", SERVICES, json!({"value": []})).await;
    let (_, data) = call_valid(&d, "vibemk_get_current_problems", json!({})).await;
    assert_eq!(
        data,
        json!({"total": 0, "host_problems": [], "service_problems": []})
    );
}

// ------------------------------------------------------------------ downtimes

fn iso(offset: Duration) -> String {
    (Utc::now() + offset).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// One host downtime running now and one service downtime starting tomorrow, as 2.5 lists them.
fn downtimes() -> Value {
    let (hour, day) = (Duration::hours(1), Duration::days(1));
    json!({"value": [
        {"id": "8", "extensions": {"site_id": "cmk", "host_name": "vibemk-test", "author": "cmkadmin", "start_time": iso(-hour), "end_time": iso(hour),
            "recurring": false, "comment": "patching", "mode": {"type": "fixed"}, "is_service": false}},
        {"id": "9", "extensions": {"site_id": "cmk", "host_name": "vibemk-test", "author": "cmkadmin", "start_time": iso(day), "end_time": iso(day + hour),
            "recurring": false, "comment": "tomorrow", "mode": {"type": "fixed"}, "is_service": true, "service_description": "Uptime"}}
    ]})
}

#[tokio::test]
async fn active_only_leaves_out_downtimes_that_have_not_started() {
    // The filter read an is_pending field 2.5 does not send, so it never filtered anything.
    let (server, d) = dispatcher().await;
    mount(&server, "GET", DOWNTIMES, downtimes()).await;
    let (_, data) = call_valid(&d, "vibemk_list_downtimes", json!({})).await;
    let ids: Vec<&str> = data["downtimes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["downtime_id"].as_str())
        .collect();
    assert_eq!(ids, vec!["8"]);
}

#[tokio::test]
async fn every_downtime_is_listed_with_its_kind_and_whether_it_runs() {
    let (server, d) = dispatcher().await;
    mount(&server, "GET", DOWNTIMES, downtimes()).await;
    let (_, data) = call_valid(&d, "vibemk_list_downtimes", json!({"active_only": false})).await;
    let (host, service) = (&data["downtimes"][0], &data["downtimes"][1]);
    assert_eq!(
        (
            &host["is_service"],
            &host["service_description"],
            &host["active"]
        ),
        (&json!(false), &Value::Null, &json!(true))
    );
    assert_eq!(
        (
            &service["is_service"],
            &service["service_description"],
            &service["active"]
        ),
        (&json!(true), &json!("Uptime"), &json!(false))
    );
}

#[tokio::test]
async fn the_other_downtime_lists_fit_the_same_schema() {
    for tool in ["vibemk_get_downtimes", "vibemk_get_active_downtimes"] {
        let (server, d) = dispatcher().await;
        mount(&server, "GET", DOWNTIMES, downtimes()).await;
        call_valid(&d, tool, json!({})).await;
    }
}

#[tokio::test]
async fn no_downtimes_still_carries_structured_content() {
    for tool in [
        "vibemk_get_downtimes",
        "vibemk_list_downtimes",
        "vibemk_get_active_downtimes",
    ] {
        let (server, d) = dispatcher().await;
        mount(&server, "GET", DOWNTIMES, json!({"value": []})).await;
        let (_, data) = call_valid(&d, tool, json!({})).await;
        assert_eq!(data, json!({"total": 0, "downtimes": []}), "{tool}");
    }
}

// ------------------------------------------------------------------ metrics

#[tokio::test]
async fn listing_reads_the_metrics_column_it_asks_for() {
    // Without columns the collection omits `metrics`, so every listing used to be empty.
    let (server, d) = dispatcher().await;
    mount(&server, "GET", SERVICES, json!({"value": [{"extensions": {"host_name": "vibemk-test", "description": "Uptime", "metrics": ["uptime"]}}]})).await;
    let (_, data) = call_valid(
        &d,
        "vibemk_get_service_metrics",
        json!({"host_name": "vibemk-test", "service_description": "Uptime"}),
    )
    .await;
    assert_eq!(data["available_metrics"], json!(["uptime"]));
    let reqs = server.received_requests().await.unwrap_or_default();
    let query = reqs
        .last()
        .expect("a request")
        .url
        .query()
        .unwrap_or_default()
        .to_string();
    assert!(
        query.contains("metrics"),
        "columns must ask for metrics: {query}"
    );
}

#[tokio::test]
async fn list_available_metrics_fits_its_schema() {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        HOSTS,
        json!({"value": [{"extensions": {"name": "vibemk-test", "metrics": ["rta", "pl"]}}]}),
    )
    .await;
    let (_, data) = call_valid(
        &d,
        "vibemk_list_available_metrics",
        json!({"host_name": "vibemk-test"}),
    )
    .await;
    assert_eq!(data["available_metrics"], json!(["rta", "pl"]));
}

#[tokio::test]
async fn a_host_metric_is_requested_under_the_host_pseudo_service() {
    // The endpoint requires service_description; without _HOST_ every host metric answered 400.
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "POST",
        METRIC_GET,
        json!({"time_range": {"start": "2026-10-02T20:23:00+00:00", "end": "2026-10-02T21:24:00+00:00"}, "step": 60,
            "metrics": [{"title": "Round trip average", "data_points": [0.0001, 0.0003, null]}]}),
    )
    .await;
    let (_, data) = call_valid(
        &d,
        "vibemk_get_host_metrics",
        json!({"host_name": "vibemk-test", "metric_name": "rta"}),
    )
    .await;
    let sent: Value = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .find(|r| r.method.as_str() == "POST")
        .expect("a POST")
        .body_json()
        .unwrap();
    assert_eq!(sent["service_description"], "_HOST_");
    assert_eq!(data["step"], 60);
    assert_eq!(data["series"][0]["latest"], json!(0.0003));
}

#[tokio::test]
async fn a_window_without_values_is_reported_not_crashed_on() {
    // All-None points made the average "N/A", which the :.2f format then raised on.
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "POST",
        METRIC_GET,
        json!({"step": 60, "metrics": [{"title": "Uptime", "data_points": [null, null]}]}),
    )
    .await;
    let (r, data) = call_valid(
        &d,
        "vibemk_get_service_metrics",
        json!({"host_name": "vibemk-test", "service_description": "Uptime", "metric_name": "uptime"}),
    )
    .await;
    assert_eq!(
        data["series"][0],
        json!({"title": "Uptime", "points": 2, "latest": null, "min": null, "max": null, "avg": null})
    );
    assert!(
        text(&r).contains("N/A (no values in this window)"),
        "{}",
        text(&r)
    );
}

#[test]
fn the_latest_value_is_the_last_one_carrying_a_value() {
    // CheckMK's newest bucket is usually still empty -- "Latest Value: None" was the result.
    let s = series_summary(&json!({"title": "Uptime", "data_points": [48597.9, 48655.1, null]}));
    assert_eq!(s["latest"], json!(48655.1));
    assert_eq!(
        (&s["min"], &s["max"], &s["points"]),
        (&json!(48597.9), &json!(48655.1), &json!(3))
    );
}
