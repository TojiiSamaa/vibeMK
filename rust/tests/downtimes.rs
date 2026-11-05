//! Port of tests/test_handlers_downtimes.py and tests/test_handlers_downtimes_times.py,
//! plus the rendering and endpoint behaviour the handler adds around them.

mod common;

use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::downtimes::{
    format_timestamp, is_downtime_active, parse_downtime_times, parse_natural_time,
    parse_time_delta, unix_time, DowntimeHandler,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

async fn handler() -> (MockServer, DowntimeHandler) {
    let (server, client) = fake_checkmk().await;
    (
        server,
        DowntimeHandler::new(client).with_verify_delay(Duration::ZERO),
    )
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

const ALL: &str = "domain-types/downtime/collections/all";
const DELETE: &str = "domain-types/downtime/actions/delete/invoke";

// ---------------------------------------------------------------- test_handlers_downtimes.py

#[tokio::test]
async fn an_id_deletes_by_id() {
    let (server, h) = handler().await;
    mount(&server, "POST", DELETE, 204, json!({})).await;
    h.handle("vibemk_delete_downtime", &json!({"downtime_id": "123"}))
        .await;
    let sent: Value = requests(&server, "POST", DELETE).await[0]
        .body_json()
        .unwrap();
    assert_eq!(sent["delete_type"], "by_id");
    assert_eq!(sent["downtime_id"], "123");
    assert_eq!(sent["site_id"], "cmk");
}

#[tokio::test]
async fn an_id_never_builds_a_query() {
    let (server, h) = handler().await;
    mount(&server, "POST", DELETE, 204, json!({})).await;
    h.handle("vibemk_delete_downtime", &json!({"downtime_id": "123"}))
        .await;
    let sent: Value = requests(&server, "POST", DELETE).await[0]
        .body_json()
        .unwrap();
    assert!(
        sent.get("query").is_none(),
        "an id must not be widened into a query"
    );
}

#[tokio::test]
async fn an_id_does_not_need_to_list_every_downtime_first() {
    let (server, h) = handler().await;
    mount(&server, "POST", DELETE, 204, json!({})).await;
    h.handle("vibemk_delete_downtime", &json!({"downtime_id": "123"}))
        .await;
    assert!(requests(&server, "GET", ALL).await.is_empty());
}

#[tokio::test]
async fn missing_host_and_id_is_refused() {
    let (server, h) = handler().await;
    let out = h.handle("vibemk_delete_downtime", &json!({})).await;
    assert!(text(&out).contains('❌'));
    assert_eq!(out[1]["type"], "_error");
    assert!(server
        .received_requests()
        .await
        .unwrap_or_default()
        .is_empty());
}

#[tokio::test]
async fn recur_reaches_checkmk_for_host_and_service() {
    for (tool, endpoint) in [
        ("vibemk_schedule_host_downtime", "collections/host"),
        ("vibemk_schedule_service_downtime", "collections/service"),
    ] {
        let (server, h) = handler().await;
        mount(&server, "GET", ALL, 200, json!({"value": []})).await;
        let target = format!("domain-types/downtime/{endpoint}");
        mount(&server, "POST", &target, 200, json!({})).await;
        let mut args = json!({"host_name": "web01", "duration": "30m", "comment": "patch window", "recur": "week"});
        if tool == "vibemk_schedule_service_downtime" {
            args["service_descriptions"] = json!(["CPU load"]);
        }
        h.handle(tool, &args).await;
        let sent: Value = requests(&server, "POST", &target).await[0]
            .body_json()
            .unwrap();
        assert_eq!(sent["recur"], "week", "{tool}");
    }
}

#[tokio::test]
async fn no_recur_sends_no_field() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALL, 200, json!({"value": []})).await;
    mount(
        &server,
        "POST",
        "domain-types/downtime/collections/host",
        200,
        json!({}),
    )
    .await;
    h.handle(
        "vibemk_schedule_host_downtime",
        &json!({"host_name": "web01", "duration": "30m"}),
    )
    .await;
    let sent: Value = requests(&server, "POST", "domain-types/downtime/collections/host").await[0]
        .body_json()
        .unwrap();
    assert!(sent.get("recur").is_none());
}

// ---------------------------------------------------------------- test_handlers_downtimes_times.py

#[test]
fn the_duration_grammar_reads_the_documented_forms() {
    for (t, m) in [
        ("30m", 30),
        ("2h", 120),
        ("1d", 1440),
        ("1h30m", 90),
        ("2d4h", 3120),
        ("1d2h30m", 1590),
    ] {
        assert_eq!(parse_time_delta(t), Ok(m), "{t}");
    }
}

#[test]
fn a_leading_plus_is_a_direction_not_a_digit() {
    assert_eq!(parse_time_delta("+1h"), parse_time_delta("1h"));
}

#[test]
fn a_bare_number_is_minutes_and_case_does_not_matter() {
    assert_eq!(parse_time_delta("45"), Ok(45));
    assert_eq!(parse_time_delta("2H30M"), Ok(150));
}

#[test]
fn an_unparseable_duration_falls_back_to_an_hour() {
    assert_eq!(parse_time_delta("later"), Ok(60));
    assert_eq!(parse_time_delta("90s"), Ok(60));
}

#[test]
fn an_iso_string_with_z_is_understood() {
    assert_eq!(
        format_timestamp(&json!("2026-03-01T22:00:00Z")),
        "2026-03-01 22:00"
    );
}

#[test]
fn a_unix_number_is_understood() {
    let stamp = Local
        .with_ymd_and_hms(2026, 3, 1, 22, 0, 0)
        .earliest()
        .unwrap()
        .timestamp();
    assert_eq!(format_timestamp(&json!(stamp)), "2026-03-01 22:00");
}

#[test]
fn anything_unreadable_says_so_rather_than_guessing() {
    for v in [
        json!(null),
        json!(0),
        json!(""),
        json!("not a time"),
        json!({}),
    ] {
        assert_eq!(format_timestamp(&v), "Unknown", "{v}");
    }
}

#[test]
fn unix_conversion_round_trips_an_iso_string() {
    let expected = DateTime::parse_from_rfc3339("2026-03-01T22:00:00+00:00")
        .unwrap()
        .timestamp() as f64;
    assert_eq!(unix_time(&json!("2026-03-01T22:00:00+00:00")), expected);
}

#[test]
fn unix_conversion_of_rubbish_is_zero_not_an_exception() {
    for v in [json!(null), json!(""), json!("not a time"), json!({})] {
        assert_eq!(unix_time(&v), 0.0, "{v}");
    }
}

fn window(start: Value, end: Value) -> Value {
    json!({"extensions": {"start_time": start, "end_time": end}})
}

#[test]
fn a_window_is_active_between_its_inclusive_boundaries() {
    let d = window(json!(100), json!(200));
    assert!(is_downtime_active(&d, 150.0));
    assert!(is_downtime_active(&d, 100.0));
    assert!(is_downtime_active(&d, 200.0));
    assert!(!is_downtime_active(&d, 201.0));
    assert!(!is_downtime_active(&d, 99.0));
}

#[test]
fn iso_strings_are_compared_the_same_as_numbers() {
    let d = window(
        json!("2026-03-01T22:00:00+00:00"),
        json!("2026-03-01T23:00:00+00:00"),
    );
    let inside = DateTime::parse_from_rfc3339("2026-03-01T22:30:00+00:00")
        .unwrap()
        .timestamp() as f64;
    assert!(is_downtime_active(&d, inside));
}

#[test]
fn an_unreadable_window_or_missing_extensions_is_not_active() {
    assert!(!is_downtime_active(
        &window(json!("nonsense"), json!("also nonsense")),
        150.0
    ));
    assert!(!is_downtime_active(&json!({}), 150.0));
}

// The zone is a parameter, so no process-wide TZ is touched: Asia/Kolkata is +5:30 all year,
// the half hour catches arithmetic that only handles whole-hour offsets.
fn kolkata() -> FixedOffset {
    FixedOffset::east_opt(5 * 3600 + 1800).unwrap()
}

/// 2026-03-01 (a Sunday) 10:00:00Z = 15:30 in Kolkata.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap()
}

fn times(start: Option<&str>, end: Option<&str>, minutes: i64) -> (String, String) {
    parse_downtime_times(&kolkata(), now(), start, end, minutes).unwrap()
}

fn utc(s: &str) -> DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .unwrap()
        .and_utc()
}

#[test]
fn a_parsed_wall_clock_time_carries_an_offset() {
    let parsed = parse_natural_time(&kolkata(), now(), "22:00 tomorrow")
        .unwrap()
        .unwrap();
    assert_eq!(parsed.offset().local_minus_utc(), 19800);
}

#[test]
fn a_wall_clock_time_is_converted_from_local_to_utc() {
    // 22:00 on 2 March in Kolkata is 16:30Z.
    assert_eq!(
        times(Some("22:00 tomorrow"), None, 120).0,
        "2026-03-02T16:30:00Z"
    );
}

#[test]
fn natural_language_forms_resolve_on_the_local_clock() {
    let at = |s: &str| {
        parse_natural_time(&kolkata(), now(), s).unwrap().map(|d| {
            d.with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
    };
    assert_eq!(at("22:00").as_deref(), Some("2026-03-01T16:30:00Z"));
    assert_eq!(
        at("10:00").as_deref(),
        Some("2026-03-02T04:30:00Z"),
        "already passed today: tomorrow"
    );
    assert_eq!(
        at("tomorrow at 14:00").as_deref(),
        Some("2026-03-02T08:30:00Z")
    );
    assert_eq!(
        at("monday at 09:00").as_deref(),
        Some("2026-03-02T03:30:00Z")
    );
    assert_eq!(
        at("next sunday at 09:00").as_deref(),
        Some("2026-03-08T03:30:00Z")
    );
    assert_eq!(at("in 2 hours").as_deref(), Some("2026-03-01T12:00:00Z"));
    assert_eq!(at("in 30 minutes").as_deref(), Some("2026-03-01T10:30:00Z"));
    assert_eq!(at("nonsense"), None);
}

#[test]
fn an_out_of_range_clock_time_is_not_a_time() {
    assert_eq!(
        parse_natural_time(&kolkata(), now(), "25:00").unwrap(),
        None
    );
}

#[test]
fn now_means_now_and_a_relative_start_is_measured_from_now() {
    assert_eq!(times(Some("now"), None, 60).0, "2026-03-01T10:00:00Z");
    assert_eq!(times(None, None, 60).0, "2026-03-01T10:00:00Z");
    assert_eq!(times(Some("+2h"), None, 60).0, "2026-03-01T12:00:00Z");
}

#[test]
fn the_duration_decides_the_end_when_none_is_given() {
    let (s, e) = times(Some("now"), None, 90);
    assert_eq!(utc(&e) - utc(&s), chrono::Duration::minutes(90));
}

#[test]
fn a_relative_end_is_measured_from_the_start() {
    let (s, e) = times(Some("now"), Some("+45m"), 120);
    assert_eq!(utc(&e) - utc(&s), chrono::Duration::minutes(45));
}

#[test]
fn an_end_before_the_start_is_pushed_past_it() {
    let (s, e) = times(Some("22:00 today"), Some("21:00 today"), 60);
    assert!(utc(&e) > utc(&s));
}

#[test]
fn an_iso_start_without_natural_time_is_read_as_given() {
    // No HH:MM in the text, so the natural parser leaves it to the ISO fallback.
    assert_eq!(times(Some("2030-01-02T03Z"), None, 60).0.len(), 20);
    assert_eq!(
        times(Some("garbage"), None, 60).0,
        "2026-03-01T10:00:00Z",
        "unparseable falls back to now"
    );
}

#[test]
fn the_format_is_the_one_the_api_accepts() {
    let (s, e) = times(Some("now"), None, 60);
    for v in [s, e] {
        assert!(
            chrono::NaiveDateTime::parse_from_str(&v, "%Y-%m-%dT%H:%M:%SZ").is_ok(),
            "{v}"
        );
    }
}

// ---------------------------------------------------------------- the handler around them

fn dt(id: &str, host: &str, svc: Option<&str>, start: i64, end: i64, comment: &str) -> Value {
    json!({"id": id, "extensions": {"host_name": host, "service_description": svc, "is_service": svc.is_some(), "start_time": start, "end_time": end, "comment": comment, "author": "me"}})
}

#[tokio::test]
async fn list_shows_host_and_service_downtimes_apart_and_structured() {
    let (server, h) = handler().await;
    let now = Utc::now().timestamp();
    let value = json!({"value": [dt("1", "web01", None, now - 60, now + 3600, "patch"), dt("2", "web01", Some("CPU load"), now - 60, now + 3600, "tune"), dt("3", "old", None, now - 7200, now - 3600, "past")]});
    mount(&server, "GET", ALL, 200, value).await;
    let out = h.handle("vibemk_list_downtimes", &json!({})).await;
    let t = text(&out);
    assert!(t.starts_with("📋 **Downtimes List**\n\nFound 2 downtimes: **1 Host-Level** + **1 Service-Level**\n\n🏠 **HOST DOWNTIMES**"), "{t}");
    assert!(t.contains("**web01** (1 host downtime)\n  • Downtime #1: "));
    assert!(t.contains("  📋 **CPU load** (1 downtime)\n"));
    assert!(
        t.contains("      **Effect**: Only 'CPU load' alerts suppressed, host alerts STILL FIRE")
    );
    assert!(t.ends_with(
        "💡 **Tip:** Use `vibemk_delete_downtime` with downtime ID to cancel a downtime"
    ));
    assert!(!t.contains("past"), "ended downtimes are hidden by default");
    let data = &out[1]["data"];
    assert_eq!(data["total"], 2);
    assert_eq!(data["downtimes"][1]["service_description"], "CPU load");
    assert_eq!(data["downtimes"][0]["service_description"], Value::Null);
    assert_eq!(data["downtimes"][0]["active"], true);
}

#[tokio::test]
async fn list_filters_by_host_and_can_include_inactive_ones() {
    let (server, h) = handler().await;
    let now = Utc::now().timestamp();
    mount(&server, "GET", ALL, 200, json!({"value": [dt("1", "web01", None, now + 600, now + 1200, "later"), dt("2", "db01", None, now - 60, now + 60, "now")]})).await;
    let none = h
        .handle("vibemk_list_downtimes", &json!({"host_name": "web01"}))
        .await;
    assert_eq!(
        text(&none),
        "📋 **No Downtimes Found**\n\nNo active downtimes for host 'web01'"
    );
    let all = h
        .handle(
            "vibemk_list_downtimes",
            &json!({"host_name": "web01", "active_only": false}),
        )
        .await;
    assert_eq!(all[1]["data"]["total"], 1);
    assert_eq!(all[1]["data"]["downtimes"][0]["active"], false);
}

#[tokio::test]
async fn active_downtimes_report_remaining_minutes() {
    let (server, h) = handler().await;
    let now = Utc::now().timestamp();
    mount(
        &server,
        "GET",
        ALL,
        200,
        json!({"value": [dt("7", "web01", None, now - 60, now + 31 * 60, "patch")]}),
    )
    .await;
    let out = h.handle("vibemk_get_active_downtimes", &json!({})).await;
    let t = text(&out);
    assert!(t.starts_with("🔴 **Currently Active Downtimes**\n\nFound 1 active downtimes:\n\n**1. Downtime #7**\n   🏠 Host: web01\n   🔧 Type: Host\n   ⏰ Remaining: 30 minutes\n   💬 Comment: patch\n"), "{t}");
    assert!(t.ends_with("💡 **Info:** These downtimes are currently suppressing alerts"));
    let empty = h
        .handle(
            "vibemk_get_active_downtimes",
            &json!({"host_name": "other"}),
        )
        .await;
    assert_eq!(
        text(&empty),
        "🟢 **No Active Downtimes**\n\nNo downtimes are currently active on host 'other'"
    );
}

#[tokio::test]
async fn host_status_distinguishes_host_and_service_downtimes() {
    let (server, h) = handler().await;
    let now = Utc::now().timestamp();
    mount(
        &server,
        "GET",
        ALL,
        200,
        json!({"value": [dt("5", "web01", Some("CPU load"), now - 60, now + 600, "tune")]}),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_check_host_downtime_status",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert!(
        t.contains("🏠 **Host Object Downtime:** ❌ **NO**")
            || t.contains("🏠 **Host Object Downtime:** ✅ **YES**")
    );
    // The fake answers both queries alike, so both sides see the one downtime.
    assert!(t.contains(
        "🔧 **Service Downtimes:** ✅ **YES** - 1 service(s) on this host have active downtimes"
    ));
    assert!(t.contains("   • Service 'CPU load' (#5): tune\n"));
    let sent = requests(&server, "GET", ALL).await;
    let q = sent[0]
        .url
        .query_pairs()
        .find(|(k, _)| k == "query")
        .unwrap()
        .1
        .to_string();
    assert_eq!(
        q,
        r#"{"op": "and", "expr": [{"op": "=", "left": "host_name", "right": "web01"}, {"op": "=", "left": "is_service", "right": "0"}]}"#
    );
}

#[tokio::test]
async fn host_status_without_downtimes_says_all_alerts_fire() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALL, 200, json!({"value": []})).await;
    let t = text(
        &h.handle(
            "vibemk_check_host_downtime_status",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert!(t.contains("🔴 **NO DOWNTIMES:** This host has neither host nor service downtimes.\n"));
    assert!(t.ends_with("   for proper alerting analysis and downtime troubleshooting."));
}

#[tokio::test]
async fn an_existing_host_downtime_blocks_a_duplicate_unless_forced() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        ALL,
        200,
        json!({"value": [dt("1", "web01", None, 0, 9, "patch")]}),
    )
    .await;
    mount(
        &server,
        "POST",
        "domain-types/downtime/collections/host",
        204,
        json!({}),
    )
    .await;
    let args = json!({"host_name": "web01", "comment": "patch"});
    let t = text(&h.handle("vibemk_schedule_host_downtime", &args).await);
    assert!(
        t.starts_with("⚠️ **Downtime Already Exists**\n\n**Host:** web01\n"),
        "{t}"
    );
    assert!(
        requests(&server, "POST", "domain-types/downtime/collections/host")
            .await
            .is_empty()
    );
    let forced = text(
        &h.handle(
            "vibemk_schedule_host_downtime",
            &json!({"host_name": "web01", "comment": "patch", "force": true}),
        )
        .await,
    );
    assert!(
        forced.starts_with("✅ **Host Downtime Scheduled Successfully**"),
        "{forced}"
    );
    assert!(forced.contains(
        "**Duration:** 60 minutes\n**Comment:** patch\n**Verification:** ✅ Confirmed\n"
    ));
}

#[tokio::test]
async fn a_downtime_that_never_shows_up_stays_pending() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALL, 200, json!({"value": []})).await;
    mount(
        &server,
        "POST",
        "domain-types/downtime/collections/host",
        200,
        json!({"id": 42}),
    )
    .await;
    let t = text(
        &h.handle(
            "vibemk_schedule_host_downtime",
            &json!({"host_name": "web01"}),
        )
        .await,
    );
    assert!(t.contains("**Downtime ID:** 42\n"));
    assert!(t.contains("**Verification:** ⚠️ Pending (may take a moment)\n"));
    // 1 existence check + 5 verification attempts.
    assert_eq!(requests(&server, "GET", ALL).await.len(), 6);
}

#[tokio::test]
async fn services_that_already_have_a_downtime_are_skipped() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        ALL,
        200,
        json!({"value": [dt("1", "web01", Some("CPU load"), 0, 9, "w")]}),
    )
    .await;
    mount(
        &server,
        "POST",
        "domain-types/downtime/collections/service",
        204,
        json!({}),
    )
    .await;
    let args = json!({"host_name": "web01", "service_descriptions": ["CPU load", "Memory"], "comment": "w"});
    let t = text(&h.handle("vibemk_schedule_service_downtime", &args).await);
    assert!(t.contains("**Services:** Memory\n"), "{t}");
    assert!(t.contains("**Skipped (existing):** CPU load\n"));
    let sent: Value = requests(&server, "POST", "domain-types/downtime/collections/service").await
        [0]
    .body_json()
    .unwrap();
    assert_eq!(sent["service_descriptions"], json!(["Memory"]));
    let all = text(
        &h.handle(
            "vibemk_schedule_service_downtime",
            &json!({"host_name": "web01", "service_description": "CPU load", "comment": "w"}),
        )
        .await,
    );
    assert!(all.starts_with("⚠️ **All Services Already Have Downtime**\n\n**Host:** web01\n**Services:** CPU load\n"), "{all}");
}

#[tokio::test]
async fn the_generic_tool_dispatches_and_refuses_other_types() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALL, 200, json!({"value": []})).await;
    mount(
        &server,
        "POST",
        "domain-types/downtime/collections/host",
        204,
        json!({}),
    )
    .await;
    let ok = h
        .handle(
            "vibemk_schedule_downtime",
            &json!({"downtime_type": "host", "host_name": "web01"}),
        )
        .await;
    assert!(text(&ok).starts_with("✅ **Host Downtime Scheduled Successfully**"));
    let bad = h
        .handle(
            "vibemk_schedule_downtime",
            &json!({"downtime_type": "cluster"}),
        )
        .await;
    assert_eq!(text(&bad), "❌ **Unsupported downtime_type**\n\n'cluster' cannot be scheduled here. Use 'host' or 'service'.");
}

#[tokio::test]
async fn modify_by_id_and_by_query() {
    let (server, h) = handler().await;
    mount(
        &server,
        "PUT",
        "domain-types/downtime/actions/modify/invoke",
        204,
        json!({}),
    )
    .await;
    let out = h
        .handle(
            "vibemk_modify_downtime",
            &json!({"downtime_id": "12", "end_time": "2026-03-01T23:00:00Z", "comment": "c"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "⏰ **Downtime Modified**\n\nTarget: downtime #12\nNew end: 2026-03-01T23:00:00Z"
    );
    h.handle("vibemk_modify_downtime", &json!({"host_name": "web01", "service_description": "CPU", "end_time": "2026-03-01T23:00:00Z"})).await;
    let sent = requests(
        &server,
        "PUT",
        "domain-types/downtime/actions/modify/invoke",
    )
    .await;
    let first: Value = sent[0].body_json().unwrap();
    assert_eq!(
        first,
        json!({"modify_type": "params", "downtime_id": 12, "end_time": {"modify_type": "set_end_time", "value": "2026-03-01T23:00:00Z"}, "comment": "c"})
    );
    let second: Value = sent[1].body_json().unwrap();
    assert_eq!(second["modify_type"], "by_query");
    assert_eq!(second["query"]["op"], "and");
    let none = h
        .handle("vibemk_modify_downtime", &json!({"end_time": "x"}))
        .await;
    assert_eq!(
        text(&none),
        "❌ **Missing parameter**\n\ndowntime_id or host_name required"
    );
    let no_end = h
        .handle("vibemk_modify_downtime", &json!({"downtime_id": 1}))
        .await;
    assert_eq!(
        text(&no_end),
        "❌ **Missing parameter**\n\nend_time is required (ISO 8601 format)"
    );
}

#[tokio::test]
async fn api_failures_and_unknown_tools_become_error_responses() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALL, 500, json!({"title": "boom"})).await;
    let out = h.handle("vibemk_list_downtimes", &json!({})).await;
    assert!(
        text(&out).starts_with("❌ **CheckMK API Error**"),
        "{}",
        text(&out)
    );
    assert_eq!(out[1]["type"], "_error");
    let unknown = h.handle("vibemk_nope", &json!({})).await;
    assert_eq!(
        text(&unknown),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}

#[tokio::test]
async fn has_host_level_downtime_reads_the_running_host_downtimes() {
    let (server, h) = handler().await;
    let now = Utc::now().timestamp();
    mount(
        &server,
        "GET",
        ALL,
        200,
        json!({"value": [dt("1", "web01", None, now - 10, now + 10, "x")]}),
    )
    .await;
    assert!(h.has_host_level_downtime("web01").await);
    let (server2, h2) = handler().await;
    mount(
        &server2,
        "GET",
        ALL,
        200,
        json!({"value": [dt("1", "web01", None, now + 100, now + 200, "x")]}),
    )
    .await;
    assert!(!h2.has_host_level_downtime("web01").await);
}
