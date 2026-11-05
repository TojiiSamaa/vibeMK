//! A tools/call per time period tool, through the real dispatcher, against a fake CheckMK.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Dispatcher) {
    let (server, client) = fake_checkmk().await;
    (server, Dispatcher::from_client(client, false))
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn url(p: &str) -> String {
    format!("{API}/{p}")
}

#[tokio::test]
async fn tools_call_get_timeperiods_lists_ranges_and_exceptions() {
    let (server, d) = setup().await;
    let body = json!({"value": [
        {"id": "24x7", "extensions": {"alias": "Always", "active_time_ranges": [{}, {}], "exceptions": [{}]}},
        {"id": "night", "extensions": {}}
    ]});
    Mock::given(method("GET"))
        .and(path(url("domain-types/time_period/collections/all")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_get_timeperiods", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "⏰ **Time Periods** (2 total):\n\n⏰ **24x7** - Always\n   2 active ranges, 1 exceptions\n\n⏰ **night** - night\n   0 active ranges");
}

#[tokio::test]
async fn tools_call_create_timeperiod_posts_and_formats_ranges() {
    let (server, d) = setup().await;
    let ranges = json!([{"day": "monday", "time_ranges": [{"start": "08:00", "end": "17:00"}]}]);
    Mock::given(method("POST"))
        .and(path(url("domain-types/time_period/collections/all")))
        .and(body_json(
            json!({"name": "office", "active_time_ranges": ranges, "alias": "Office hours"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_create_timeperiod",
            Some(&json!({"name": "office", "alias": "Office hours", "active_time_ranges": ranges})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ **Time Period Created Successfully**\n\nName: **office**\nAlias: Office hours\nActive time ranges:\n  • Monday: 08:00-17:00\n\n⚠️ **Remember to activate changes!**");
}

#[tokio::test]
async fn tools_call_create_timeperiod_refuses_a_malformed_range_without_a_request() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_create_timeperiod",
            Some(&json!({"name": "x", "active_time_ranges": [{"day": "monday"}]})),
        )
        .await;
    assert_eq!(r["isError"], true);
    assert_eq!(
        text(&r),
        "❌ **Invalid time range**\n\nTime ranges must have 'day' and 'time_ranges' fields"
    );
}

#[tokio::test]
async fn tools_call_update_timeperiod_sends_if_match_from_the_etag() {
    let (server, d) = setup().await;
    let p = url("objects/time_period/office");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"abc\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(p))
        .and(header("If-Match", "\"abc\""))
        .and(body_json(json!({"alias": "New"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_update_timeperiod",
            Some(&json!({"name": "office", "alias": "New"})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ **Time Period Updated Successfully**\n\nName: office\nUpdated fields: alias\n\n⚠️ **Remember to activate changes!**");
}

#[tokio::test]
async fn tools_call_delete_timeperiod_sends_if_match_and_deletes() {
    let (server, d) = setup().await;
    let p = url("objects/time_period/office");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v1\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(p))
        .and(header("If-Match", "\"v1\""))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_delete_timeperiod", Some(&json!({"name": "office"})))
        .await;
    assert_eq!(r["isError"], false);
    assert!(
        text(&r).starts_with("✅ **Time Period Deleted Successfully**\n\nName: office\n"),
        "{}",
        text(&r)
    );
}

#[tokio::test]
async fn a_missing_time_period_is_a_checkmk_api_error() {
    let (_server, d) = setup().await; // nothing mounted: 404
    let r = d
        .call_tool("vibemk_delete_timeperiod", Some(&json!({"name": "ghost"})))
        .await;
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).starts_with("❌ **CheckMK API Error**"),
        "{}",
        text(&r)
    );
}
