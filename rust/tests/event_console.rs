//! A tools/call per Event Console tool, through the real dispatcher, against a fake CheckMK.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{body_json, method, path, query_param};
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
async fn tools_call_get_events_filters_and_formats() {
    let (server, d) = setup().await;
    let body = json!({"value": [
        {"id": "7", "extensions": {"host": "web1", "application": "sshd", "text": "login failed", "phase": "open", "state": 2}},
        {"id": "8", "extensions": {"host": "web2", "phase": "ack", "state": 9}}
    ]});
    Mock::given(method("GET"))
        .and(path(url("domain-types/event_console/collections/all")))
        .and(query_param("phase", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_get_events", Some(&json!({"phase": "open"})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "🗃️ **Event Console — 2 Events**\n\n• **7** [🔴 CRIT] [open] `web1` — sshd\n  login failed\n• **8** [9] [ack] `web2`");
}

#[tokio::test]
async fn tools_call_acknowledge_event_posts_comment_and_site() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .and(path(url(
            "objects/event_console/7/actions/update_and_acknowledge/invoke",
        )))
        .and(body_json(
            json!({"change_comment": "Acknowledged via vibeMK", "site_id": "cmk"}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_acknowledge_event", Some(&json!({"event_id": 7})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ Event 7 acknowledged.");
}

#[tokio::test]
async fn tools_call_change_event_state_posts_the_new_state() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .and(path(url(
            "objects/event_console/7/actions/change_state/invoke",
        )))
        .and(body_json(
            json!({"new_state": "warning", "site_id": "other"}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_change_event_state",
            Some(&json!({"event_id": 7, "new_state": "warning", "site_id": "other"})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ Event 7: state set to 'warning'.");
}

#[tokio::test]
async fn tools_call_delete_events_by_id_is_one_call_each_and_by_params_otherwise() {
    let (server, d) = setup().await;
    let p = url("domain-types/event_console/actions/delete/invoke");
    for id in [3, 4] {
        Mock::given(method("POST"))
            .and(path(p.clone()))
            .and(body_json(
                json!({"filter_type": "by_id", "site_id": "cmk", "event_id": id}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path(p))
        .and(body_json(
            json!({"filter_type": "params", "filters": {"phase": "open", "host": "web1"}}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let by_id = d
        .call_tool("vibemk_delete_events", Some(&json!({"event_ids": [3, 4]})))
        .await;
    assert_eq!(text(&by_id), "✅ 2 event(s) deleted.");
    let by_params = d
        .call_tool("vibemk_delete_events", Some(&json!({"host": "web1"})))
        .await;
    assert_eq!(by_params["isError"], false);
    assert_eq!(text(&by_params), "✅ All event(s) deleted.");
}
