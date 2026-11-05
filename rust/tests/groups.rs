//! tools/call of the 5 group tools against a fake CheckMK (method, path, body, If-Match checked).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::groups::GroupsHandler;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/cmk/check_mk/api/1.0";

fn text(b: &[Value]) -> String {
    b[0]["text"].as_str().unwrap().to_string()
}
fn is_error(b: &[Value]) -> bool {
    b.iter().any(|x| x["type"] == "_error")
}
async fn setup() -> (MockServer, GroupsHandler) {
    let (server, client) = common::fake_checkmk().await;
    (server, GroupsHandler::new(client))
}
async fn mount(s: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(s)
        .await;
}

#[tokio::test]
async fn get_host_groups_lists_with_alias_fallback() {
    let (s, h) = setup().await;
    mount(&s, "GET", "domain-types/host_group_config/collections/all", 200, json!({"value": [{"id": "a", "extensions": {"alias": "Alpha"}}, {"id": "b", "extensions": {}}, {}]})).await;
    assert_eq!(
        text(&h.handle("vibemk_get_host_groups", &json!({})).await),
        "🏠 **Host Groups** (3 total):\n\n🏠 **a** - Alpha\n🏠 **b** - b\n🏠 **Unknown** - Unknown"
    );
    let (s, h) = setup().await;
    mount(
        &s,
        "GET",
        "domain-types/host_group_config/collections/all",
        200,
        json!({"value": []}),
    )
    .await;
    assert_eq!(
        text(&h.handle("vibemk_get_host_groups", &json!({})).await),
        "🏠 **No Host Groups Found**\n\nNo host groups are configured."
    );
}

#[tokio::test]
async fn get_service_groups_lists_and_reports_empty() {
    let (s, h) = setup().await;
    mount(
        &s,
        "GET",
        "domain-types/service_group_config/collections/all",
        200,
        json!({"value": [{"id": "web", "extensions": {"alias": "Web"}}]}),
    )
    .await;
    assert_eq!(
        text(&h.handle("vibemk_get_service_groups", &json!({})).await),
        "🔧 **Service Groups** (1 total):\n\n🔧 **web** - Web"
    );
    let (s, h) = setup().await;
    mount(
        &s,
        "GET",
        "domain-types/service_group_config/collections/all",
        200,
        json!({"value": []}),
    )
    .await;
    assert_eq!(
        text(&h.handle("vibemk_get_service_groups", &json!({})).await),
        "🔧 **No Service Groups Found**\n\nNo service groups are configured."
    );
    let (s, h) = setup().await;
    mount(
        &s,
        "GET",
        "domain-types/service_group_config/collections/all",
        500,
        json!({}),
    )
    .await;
    let out = h.handle("vibemk_get_service_groups", &json!({})).await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert!(is_error(&out));
}

#[tokio::test]
async fn create_host_group_posts_name_and_alias() {
    let (s, h) = setup().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{API}/domain-types/host_group_config/collections/all"
        )))
        .and(body_json(json!({"name": "g", "alias": "G"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_group",
            &json!({"name": "g", "alias": "G"}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Host Group Created Successfully**\n\nName: g\nAlias: G\n\n⚠️ **Remember to activate changes!**");
    assert_eq!(
        text(
            &h.handle("vibemk_create_host_group", &json!({"name": "g"}))
                .await
        ),
        "❌ **Missing parameters**\n\nname and alias are required"
    );
}

#[tokio::test]
async fn update_host_group_puts_alias_with_the_etag() {
    let (s, h) = setup().await;
    let obj = format!("{API}/objects/host_group_config/g");
    Mock::given(method("GET"))
        .and(path(&obj))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"abc\"")
                .set_body_json(json!({})),
        )
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .and(path(&obj))
        .and(header("If-Match", "\"abc\""))
        .and(body_json(json!({"alias": "New"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle(
            "vibemk_update_host_group",
            &json!({"name": "g", "alias": "New"}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Host Group Updated Successfully**\n\nName: g\nUpdated fields: alias\n\n⚠️ **Remember to activate changes!**");
    assert_eq!(
        text(
            &h.handle("vibemk_update_host_group", &json!({"name": "g"}))
                .await
        ),
        "❌ **No data to update**\n\nAt least one field (alias) must be provided"
    );
    assert_eq!(
        text(&h.handle("vibemk_update_host_group", &json!({})).await),
        "❌ **Missing parameter**\n\nname is required"
    );
}

#[tokio::test]
async fn update_host_group_missing_object_is_an_api_error() {
    let (s, h) = setup().await;
    mount(
        &s,
        "GET",
        "objects/host_group_config/gone",
        404,
        json!({"title": "Not Found"}),
    )
    .await;
    let out = h
        .handle(
            "vibemk_update_host_group",
            &json!({"name": "gone", "alias": "x"}),
        )
        .await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert!(is_error(&out));
}

#[tokio::test]
async fn delete_host_group_checks_then_deletes() {
    let (s, h) = setup().await;
    let obj = format!("{API}/objects/host_group_config/g");
    Mock::given(method("GET"))
        .and(path(&obj))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .and(path(&obj))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&s)
        .await;
    let out = h
        .handle("vibemk_delete_host_group", &json!({"name": "g"}))
        .await;
    assert_eq!(text(&out), "✅ **Host Group Deleted Successfully**\n\nName: g\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The host group is only marked for deletion until you activate changes!");
    assert_eq!(
        text(&h.handle("vibemk_delete_host_group", &json!({})).await),
        "❌ **Missing parameter**\n\nname is required"
    );
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
