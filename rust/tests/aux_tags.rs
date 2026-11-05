//! A tools/call per auxiliary tag tool, through the real dispatcher, against a fake CheckMK.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{body_json, method, path};
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
async fn tools_call_get_aux_tags_lists_title_and_topic() {
    let (server, d) = setup().await;
    let body = json!({"value": [{"id": "ip-v4", "title": "IPv4", "extensions": {"topic": "Address"}}, {"id": "x", "title": "X"}]});
    Mock::given(method("GET"))
        .and(path(url("domain-types/aux_tag/collections/all")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let r = d.call_tool("vibemk_get_aux_tags", Some(&json!({}))).await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        text(&r),
        "🏷️ **Aux Tags (2)**\n\n• **ip-v4** — IPv4 [Address]\n• **x** — X"
    );
}

#[tokio::test]
async fn tools_call_create_aux_tag_posts_the_body() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .and(path(url("domain-types/aux_tag/collections/all")))
        .and(body_json(
            json!({"aux_tag_id": "t1", "title": "Tag one", "topic": "Misc"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_create_aux_tag",
            Some(&json!({"tag_id": "t1", "title": "Tag one", "topic": "Misc"})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ Auxiliary tag 't1' (Tag one) created.");
}

#[tokio::test]
async fn tools_call_update_aux_tag_puts_only_the_given_fields() {
    let (server, d) = setup().await;
    Mock::given(method("PUT"))
        .and(path(url("objects/aux_tag/t1")))
        .and(body_json(json!({"title": "Renamed"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_update_aux_tag",
            Some(&json!({"tag_id": "t1", "title": "Renamed"})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ Auxiliary tag 't1' updated.");
}

#[tokio::test]
async fn tools_call_delete_aux_tag_posts_the_delete_action() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .and(path(url("objects/aux_tag/t1/actions/delete/invoke")))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_delete_aux_tag", Some(&json!({"tag_id": "t1"})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "✅ Auxiliary tag 't1' deleted.");
}

#[tokio::test]
async fn an_unsafe_tag_id_never_reaches_the_server() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_delete_aux_tag", Some(&json!({"tag_id": ".."})))
        .await;
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).starts_with("❌ **Unexpected Error**"),
        "{}",
        text(&r)
    );
}
