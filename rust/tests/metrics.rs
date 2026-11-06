//! Port of tests/test_handlers_metrics.py (time window, 400 advice) plus the request/answer
//! shapes of the metrics handler against a fake CheckMK.
mod common;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::metrics::{series_summary, MetricsHandler};
use wiremock::matchers::{body_partial_json, method, path, query_param_contains};
use wiremock::{Mock, ResponseTemplate};

fn as_utc(stamp: &Value) -> DateTime<Utc> {
    let s = stamp.as_str().expect("string stamp");
    assert!(
        s.ends_with('Z') && s.len() == 20,
        "not ISO-8601 UTC with Z: {s}"
    );
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .expect("ISO stamp")
        .and_utc()
}

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().expect("text block").to_string()
}

fn is_error(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "_error")
}

const METRIC_GET: &str = "/cmk/check_mk/api/1.0/domain-types/metric/actions/get/invoke";

#[test]
fn window_ends_at_the_current_utc_instant() {
    let before = Utc::now();
    let window = MetricsHandler::parse_time_range("1h");
    let after = Utc::now();
    let end = as_utc(&window["end"]);
    assert!(before - Duration::seconds(2) <= end && end <= after + Duration::seconds(2));
}

#[test]
fn named_ranges_span_their_duration() {
    for (name, span) in [
        ("1h", Duration::hours(1)),
        ("4h", Duration::hours(4)),
        ("24h", Duration::days(1)),
        ("7d", Duration::days(7)),
        ("30d", Duration::days(30)),
    ] {
        let w = MetricsHandler::parse_time_range(name);
        assert_eq!(as_utc(&w["end"]) - as_utc(&w["start"]), span, "{name}");
    }
}

#[test]
fn unknown_range_falls_back_to_one_hour() {
    let w = MetricsHandler::parse_time_range("not-a-range");
    assert_eq!(as_utc(&w["end"]) - as_utc(&w["start"]), Duration::hours(1));
}

#[test]
fn a_time_range_error_shows_the_format_actually_sent() {
    let sent = MetricsHandler::parse_time_range("1h")["start"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        as_utc(&json!(sent)) <= Utc::now(),
        "precondition: the window is ISO-8601 UTC"
    );
    let message = MetricsHandler::handle_400(
        &json!({"detail": "time_range is invalid"}),
        "web01",
        "Check_MK",
        "util",
    );
    // Same shape as what is transmitted: YYYY-MM-DDTHH:MM:SSZ.
    let example = message
        .split("e.g. ")
        .nth(1)
        .expect("an example in the advice");
    as_utc(&json!(example));
}

#[test]
fn series_summary_takes_the_last_point_that_has_a_value() {
    let s = series_summary(&json!({"title": "cpu", "data_points": [1, 5.5, 3, null, null]}));
    assert_eq!(s["latest"], json!(3));
    assert_eq!(s["min"], json!(1));
    assert_eq!(s["max"], json!(5.5));
    assert_eq!(s["points"], json!(5));
    assert_eq!(s["avg"], json!(3.1667));
    let empty = series_summary(&json!({"data_points": [null, null]}));
    assert!(empty["latest"].is_null() && empty["min"].is_null() && empty["avg"].is_null());
}

#[tokio::test]
async fn host_metric_asks_for_the_pseudo_service_and_a_utc_window() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(METRIC_GET))
        .and(body_partial_json(json!({"host_name": "web01", "service_description": "_HOST_", "metric_id": "rta", "site": "cmk", "type": "single_metric", "reduce": "max"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "time_range": {"start": "a", "end": "b"}, "step": 60,
            "metrics": [{"title": "Round trip", "data_points": [1.0, 2.0, null]}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let r = MetricsHandler::new(client)
        .handle(
            "vibemk_get_host_metrics",
            &json!({"host_name": "web01", "metric_name": "rta"}),
        )
        .await;
    assert!(!is_error(&r));
    let t = text(&r);
    assert!(
        t.starts_with("📊 **Host Metrics: web01**\n\nMetric: rta\nTime Range: 1h\nMetrics: 1\n\n"),
        "{t}"
    );
    assert!(
        t.contains("   Latest Value: 2.0\n   Min/Max/Avg: 1.0 / 2.0 / 1.50\n   Data Points: 3\n"),
        "{t}"
    );
    assert_eq!(r[1]["data"]["step"], json!(60));
    assert_eq!(r[1]["data"]["series"][0]["latest"], json!(2.0));
    let req = &server.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    assert!(as_utc(&body["time_range"]["start"]) < as_utc(&body["time_range"]["end"]));
}

#[tokio::test]
async fn a_rejected_service_metric_names_the_ones_that_exist() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(METRIC_GET))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"title": "Bad", "detail": "metric_id unknown"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/cmk/check_mk/api/1.0/domain-types/service/collections/all",
        ))
        .and(query_param_contains("columns", "metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"extensions": {"metrics": ["util", "load1"]}}]})),
        )
        .mount(&server)
        .await;
    let r = MetricsHandler::new(client)
        .handle(
            "vibemk_get_service_metrics",
            &json!({"host_name": "web01", "service_description": "CPU", "metric_name": "nope"}),
        )
        .await;
    assert!(is_error(&r));
    assert_eq!(
        text(&r),
        "❌ **Metrics Request Failed**\n\nService: web01/CPU\nRequested metric: nope\nError: Invalid metric ID 'nope': metric_id unknown. Metric may not exist for this service\n\n✅ **Available Metrics:** util, load1\n\n💡 **Suggestion:** Try one of these metric IDs instead"
    );
}

#[tokio::test]
async fn listing_metrics_of_an_unknown_host_is_an_error_and_of_a_known_one_a_list() {
    let (server, client) = common::fake_checkmk().await;
    let h = MetricsHandler::new(client);
    Mock::given(method("GET"))
        .and(path(
            "/cmk/check_mk/api/1.0/domain-types/host/collections/all",
        ))
        .and(query_param_contains("query", "web01"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"extensions": {"metrics": ["rta", "pl"]}}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/cmk/check_mk/api/1.0/domain-types/host/collections/all",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let ok = h
        .handle(
            "vibemk_list_available_metrics",
            &json!({"host_name": "web01"}),
        )
        .await;
    assert_eq!(text(&ok), "📊 **Available Metrics**\n\nTarget: web01\nMetrics (2 total):\n\n📈 rta\n📈 pl\n\n💡 **Usage:** pass one of these as metric_name to get its values");
    assert_eq!(ok[1]["data"]["available_metrics"], json!(["rta", "pl"]));
    let missing = h
        .handle(
            "vibemk_list_available_metrics",
            &json!({"host_name": "ghost"}),
        )
        .await;
    assert!(is_error(&missing));
    assert_eq!(
        text(&missing),
        "❌ **Host/service not found**\n\nNo data found for ghost"
    );
}

#[tokio::test]
async fn search_needs_exactly_one_of_graph_or_metric_and_posts_the_filter() {
    let (server, client) = common::fake_checkmk().await;
    let h = MetricsHandler::new(client);
    let none = h
        .handle("vibemk_search_metrics", &json!({"host_filter": "web"}))
        .await;
    assert!(text(&none)
        .starts_with("❌ **Missing parameter**\n\nEither graph_id or metric_id is required."));
    let both = h
        .handle(
            "vibemk_search_metrics",
            &json!({"host_filter": "web", "graph_id": "g", "metric_id": "m"}),
        )
        .await;
    assert_eq!(
        text(&both),
        "❌ **Conflicting parameters**\n\nGive either graph_id or metric_id, not both"
    );
    let no_service = h
        .handle(
            "vibemk_search_metrics",
            &json!({"host_filter": "web", "metric_id": "m"}),
        )
        .await;
    assert_eq!(
        text(&no_service),
        "❌ **Missing parameter**\n\nservice_filter is required (the service holding the metric)"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    Mock::given(method("POST"))
        .and(path("/cmk/check_mk/api/1.0/domain-types/metric/actions/get/invoke"))
        .and(body_partial_json(json!({"type": "single_metric", "metric_id": "util", "site": "cmk", "host_name": "web", "service_description": "CPU"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"metrics": [{"title": "CPU", "data_points": [1, null]}]})))
        .expect(1)
        .mount(&server)
        .await;
    let r = h
        .handle(
            "vibemk_search_metrics",
            &json!({"host_filter": "web", "service_filter": "CPU", "metric_id": "util"}),
        )
        .await;
    assert!(text(&r).starts_with("🔍 **Metrics Search Results**\n\nFilter: web/CPU\n\n📊 **Search Metrics: web/CPU**\n\nTime Range: 1h\nMetrics: 1 found\n\n📈 **CPU**\n   Latest: 1\n"), "{}", text(&r));
}

#[tokio::test]
async fn custom_graph_failures_use_the_status_diagnostics() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(406).set_body_json(json!({"title": "NA", "detail": "d"})),
        )
        .mount(&server)
        .await;
    let h = MetricsHandler::new(client);
    let r = h
        .handle("vibemk_get_custom_graph", &json!({"custom_graph_id": "cg"}))
        .await;
    assert_eq!(
        text(&r),
        "❌ **Failed to retrieve custom graph**\n\nCould not get custom graph 'cg': Accept header issue: NA. CheckMK API cannot satisfy the requested content type. Detail: d"
    );
    let missing = h.handle("vibemk_get_custom_graph", &json!({})).await;
    assert_eq!(
        text(&missing),
        "❌ **Missing parameter**\n\ncustom_graph_id is required"
    );
}
