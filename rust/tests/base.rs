//! Port of test_handlers_concurrency.py and of the BaseHandler helpers (base.py).
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vibemk::client::ApiResponse;
use vibemk::handlers::base::{
    py_repr, time_ago, Base, ERROR_BLOCK, MAX_CONCURRENT_REQUESTS, STRUCTURED_BLOCK,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

async fn base() -> (wiremock::MockServer, Base) {
    let (server, client) = common::fake_checkmk().await;
    (server, Base::new(client))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ---------- map_concurrently ----------

#[tokio::test]
async fn results_come_back_in_the_order_of_the_items() {
    let (_s, b) = base().await;
    let out = b
        .map_concurrently((0..5u64).collect(), |n| async move {
            tokio::time::sleep(Duration::from_millis((5 - n) * 10)).await;
            n * 10
        })
        .await;
    assert_eq!(out, vec![0, 10, 20, 30, 40]);
}

#[tokio::test]
async fn the_requests_overlap() {
    // Four calls that each wait for all four can only finish together.
    let (_s, b) = base().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let run = b.map_concurrently((0..4).collect(), |n| {
        let barrier = barrier.clone();
        async move {
            barrier.wait().await;
            n
        }
    });
    let out = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("the four requests did not overlap");
    assert_eq!(out, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn no_more_run_at_once_than_the_limit() {
    let (_s, b) = base().await;
    let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    b.map_concurrently((0..12).collect(), |_: i32| {
        let (running, peak) = (running.clone(), peak.clone());
        async move {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            running.fetch_sub(1, Ordering::SeqCst);
        }
    })
    .await;
    assert_eq!(MAX_CONCURRENT_REQUESTS, 4);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_failing_request_surfaces_as_it_would_in_a_loop() {
    let (_s, b) = base().await;
    let out = b
        .map_concurrently((0..4).collect(), |n: i32| async move {
            if n == 2 {
                Err("boom")
            } else {
                Ok(n)
            }
        })
        .await;
    assert_eq!(out.into_iter().collect::<Result<Vec<_>, _>>(), Err("boom"));
}

#[tokio::test]
async fn nothing_to_do_returns_nothing() {
    let (_s, b) = base().await;
    assert!(b
        .map_concurrently(Vec::<i32>::new(), |n| async move { n })
        .await
        .is_empty());
}

// ---------- time_ago ----------

#[test]
fn a_missing_or_zero_timestamp_is_none() {
    for v in [json!(null), json!(0), json!(0.0), json!(""), json!(false)] {
        assert_eq!(time_ago(&v), None, "{v}");
    }
}

#[test]
fn a_future_timestamp_is_clamped_to_zero() {
    assert_eq!(time_ago(&json!(now() + 18000.0)).unwrap(), "0s ago");
}

#[test]
fn time_ago_picks_the_right_unit() {
    let ago = |secs: f64| time_ago(&json!(now() - secs)).unwrap();
    assert!(
        ago(42.0).ends_with("s ago") && ago(42.0).starts_with("4"),
        "{}",
        ago(42.0)
    );
    assert_eq!(ago(300.5), "5m ago");
    assert_eq!(ago(3.0 * 3600.0 + 5.0), "3h ago");
    assert_eq!(ago(2.0 * 86400.0 + 5.0), "2d ago");
}

#[test]
fn a_timestamp_given_as_text_is_read() {
    assert_eq!(
        time_ago(&json!((now() - 7200.5).to_string())).unwrap(),
        "2h ago"
    );
}

#[test]
fn an_unreadable_timestamp_comes_back_as_is() {
    assert_eq!(time_ago(&json!("yesterday")).unwrap(), "yesterday");
    assert_eq!(time_ago(&json!([1, 2])).unwrap(), "[1, 2]");
}

// ---------- responses ----------

#[tokio::test]
async fn structured_response_carries_prose_then_data() {
    let (_s, b) = base().await;
    assert_eq!(
        b.structured_response("hi", json!({"k": 1})),
        vec![
            json!({"type": "text", "text": "hi"}),
            json!({"type": STRUCTURED_BLOCK, "data": {"k": 1}})
        ]
    );
    assert_eq!((STRUCTURED_BLOCK, ERROR_BLOCK), ("_structured", "_error"));
}

#[tokio::test]
async fn success_and_info_responses_use_their_emoji() {
    let (_s, b) = base().await;
    assert_eq!(
        b.success_response("Done", None),
        vec![json!({"type": "text", "text": "✅ **Done**"})]
    );
    assert_eq!(
        b.info_response("FYI", None),
        vec![json!({"type": "text", "text": "ℹ️ **FYI**"})]
    );
}

#[tokio::test]
async fn data_is_appended_unless_it_is_empty() {
    let (_s, b) = base().await;
    let with = b.success_response("Done", Some(&json!({"host": "web01", "n": 3})));
    assert_eq!(with[0]["text"], "✅ **Done**\n\n**host**: web01\n**n**: 3");
    assert_eq!(
        b.info_response("FYI", Some(&json!({})))[0]["text"],
        "ℹ️ **FYI**"
    );
    assert_eq!(
        b.info_response("FYI", Some(&Value::Null))[0]["text"],
        "ℹ️ **FYI**"
    );
}

#[tokio::test]
async fn error_response_marks_the_failure_and_adds_details() {
    let (_s, b) = base().await;
    assert_eq!(
        b.error_response("Failed", Some("why")),
        vec![
            json!({"type": "text", "text": "❌ **Failed**\n\nwhy"}),
            json!({"type": "_error"})
        ]
    );
    assert_eq!(b.error_response("Failed", None)[0]["text"], "❌ **Failed**");
    assert_eq!(
        b.error_response("Failed", Some(""))[0]["text"],
        "❌ **Failed**"
    );
}

#[tokio::test]
async fn error_text_keeps_the_exact_wording() {
    let (_s, b) = base().await;
    assert_eq!(
        b.error_text("no way"),
        vec![
            json!({"type": "text", "text": "no way"}),
            json!({"type": "_error"})
        ]
    );
}

#[tokio::test]
async fn format_data_counts_lists_and_names_objects() {
    let (_s, b) = base().await;
    let d = json!({"hosts": [1, 2, 3], "cfg": {"a": 1}, "name": "web01"});
    assert_eq!(
        b.format_data(&d),
        "**hosts**: 3\n**cfg**: object\n**name**: web01"
    );
}

#[tokio::test]
async fn format_data_renders_scalars_like_python() {
    let (_s, b) = base().await;
    assert_eq!(
        b.format_data(&json!({"on": true, "off": false, "none": null, "f": 1.5, "i": 7})),
        "**on**: True\n**off**: False\n**none**: None\n**f**: 1.5\n**i**: 7"
    );
    assert_eq!(b.format_data(&json!("plain")), "plain");
    assert_eq!(b.format_data(&json!(null)), "None");
    assert_eq!(b.format_data(&json!([1, "a"])), "[1, 'a']");
}

#[test]
fn py_repr_follows_python_quoting() {
    assert_eq!(py_repr(&json!("a")), "'a'");
    assert_eq!(py_repr(&json!("it's")), "\"it's\"");
    assert_eq!(py_repr(&json!({"a": [true, null]})), "{'a': [True, None]}");
}

// ---------- ETag / If-Match ----------

fn response(headers: &[(&str, &str)], data: Value) -> ApiResponse {
    ApiResponse {
        status: 200,
        data,
        raw: String::new(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[test]
fn the_etag_header_wins() {
    let r = response(
        &[("ETag", "\"h\"")],
        json!({"extensions": {"meta_data": {"etag": "\"b\""}}}),
    );
    assert_eq!(Base::extract_etag(&r), "\"h\"");
}

#[test]
fn the_etag_header_is_found_whatever_its_case() {
    assert_eq!(
        Base::extract_etag(&response(&[("etag", "\"h\"")], json!({}))),
        "\"h\""
    );
}

#[test]
fn the_etag_falls_back_to_the_body_then_to_the_wildcard() {
    assert_eq!(
        Base::extract_etag(&response(
            &[],
            json!({"extensions": {"meta_data": {"etag": "\"b\""}}})
        )),
        "\"b\""
    );
    assert_eq!(
        Base::extract_etag(&response(&[("ETag", "")], json!({}))),
        "*"
    );
    assert_eq!(Base::extract_etag(&response(&[], json!(null))), "*");
}

#[tokio::test]
async fn if_match_uses_the_current_etag() {
    let (server, b) = base().await;
    Mock::given(method("GET"))
        .and(path("/cmk/check_mk/api/1.0/objects/host_config/web1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v7\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        b.if_match_header("objects/host_config/web1").await,
        vec![("If-Match".to_string(), "\"v7\"".to_string())]
    );
}

#[tokio::test]
async fn if_match_degrades_to_the_wildcard_when_the_lookup_fails() {
    let (server, b) = base().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert_eq!(
        b.if_match_header("objects/host_config/gone").await,
        vec![("If-Match".to_string(), "*".to_string())]
    );
}
