//! tools/call of the 8 service group tools against a fake CheckMK (method, path, body, If-Match checked).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::service_groups::ServiceGroupHandler;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OBJ: &str = "/cmk/check_mk/api/1.0/objects/service_group_config/web";
const ALL: &str = "/cmk/check_mk/api/1.0/domain-types/service_group_config/collections/all";
const BULK: &str = "/cmk/check_mk/api/1.0/domain-types/service_group_config/actions";

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}
fn is_error(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "_error")
}
async fn call(
    server_client: (MockServer, std::sync::Arc<vibemk::client::CheckMkClient>),
    tool: &str,
    args: Value,
) -> (MockServer, Vec<Value>) {
    let (server, client) = server_client;
    let out = ServiceGroupHandler::new(client).handle(tool, &args).await;
    (server, out)
}
async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(p))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn create_checks_absence_then_posts_name_and_alias() {
    let (server, client) = common::fake_checkmk().await;
    mount(&server, "GET", OBJ, 404, json!({"title": "Not Found"})).await;
    Mock::given(method("POST"))
        .and(path(ALL))
        .and(body_json(json!({"name": "web", "alias": "Web"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let (_s, out) = call(
        (server, client),
        "vibemk_create_service_group",
        json!({"name": "web", "alias": "Web"}),
    )
    .await;
    assert_eq!(text(&out), "✅ **Service Group Created Successfully**\n\n**Name:** web\n**Alias:** Web\n\n⚠️ **Remember to activate changes!**\n\n💡 **Next Steps:**\n1️⃣ Use 'list_service_groups' to view all groups\n2️⃣ Use 'activate_changes' to apply configuration");
    assert!(!is_error(&out));
}

#[tokio::test]
async fn create_refuses_an_existing_group_and_bad_input() {
    let (server, client) = common::fake_checkmk().await;
    mount(&server, "GET", OBJ, 200, json!({"title": "Web"})).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle(
            "vibemk_create_service_group",
            &json!({"name": "web", "alias": "Web"}),
        )
        .await;
    assert_eq!(text(&out), "❌ **Service group already exists**\n\nService group 'web' already exists. Use update to modify it.");
    assert!(is_error(&out));
    assert_eq!(
        text(
            &h.handle("vibemk_create_service_group", &json!({"alias": "x"}))
                .await
        ),
        "❌ **Missing parameter**\n\nname is required"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_create_service_group", &json!({"name": "web"}))
                .await
        ),
        "❌ **Missing parameter**\n\nalias is required"
    );
    assert_eq!(text(&h.handle("vibemk_create_service_group", &json!({"name": "a b", "alias": "x"})).await), "❌ **Invalid name**\n\nService group name must contain only letters, numbers, hyphens, and underscores");
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' not supported by service group handler"
    );
}

#[tokio::test]
async fn list_numbers_groups_and_falls_back_on_title_then_no_alias() {
    let (server, client) = common::fake_checkmk().await;
    let body = json!({"value": [
        {"id": "a", "title": "Alpha", "extensions": {}},
        {"id": "b", "extensions": {"alias": "Beta"}},
        {"title": ""},
    ]});
    mount(&server, "GET", ALL, 200, body).await;
    let (server, out) = call(
        (server, client.clone()),
        "vibemk_list_service_groups",
        json!({}),
    )
    .await;
    assert_eq!(text(&out), "📋 **Service Groups List** (3 total)\n\n**1. Service Group: a**\n   Alias: Alpha\n   ID: a\n\n**2. Service Group: b**\n   Alias: Beta\n   ID: b\n\n**3. Service Group: Unknown**\n   Alias: No alias\n   ID: Unknown\n\n💡 Use 'get_service_group' with a name to see detailed information");
    server.reset().await;
    mount(&server, "GET", ALL, 200, json!({"value": []})).await;
    let out = ServiceGroupHandler::new(client)
        .handle("vibemk_list_service_groups", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "ℹ️ **No Service Groups Found**\n\nUse 'create_service_group' to create one."
    );
}

#[tokio::test]
async fn get_shows_alias_and_only_update_delete_links() {
    let (server, client) = common::fake_checkmk().await;
    let body = json!({"title": "Web", "extensions": {}, "links": [{"rel": "cmk/update"}, {"rel": "self"}, {"rel": "cmk/delete"}]});
    mount(&server, "GET", OBJ, 200, body).await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle("vibemk_get_service_group", &json!({"name": "web"}))
        .await;
    assert_eq!(text(&out), "📋 **Service Group Details**\n\n**Name:** web\n**Alias:** Web\n**Type:** Service Group Configuration\n\n🔗 **Available Operations:**\n• Update\n• Delete\n\n💡 Use 'update_service_group' or 'delete_service_group' to modify this group");
    server.reset().await;
    mount(
        &server,
        "GET",
        OBJ,
        404,
        json!({"title": "Not Found", "detail": "gone"}),
    )
    .await;
    let out = h
        .handle("vibemk_get_service_group", &json!({"name": "web"}))
        .await;
    assert!(
        text(&out).starts_with("❌ **Service group not found**\n\n"),
        "{}",
        text(&out)
    );
    assert!(is_error(&out));
    assert_eq!(
        text(&h.handle("vibemk_get_service_group", &json!({})).await),
        "❌ **Missing parameter**\n\nname is required"
    );
}

#[tokio::test]
async fn update_puts_alias_with_the_etag_of_the_current_object() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(OBJ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v7\"")
                .set_body_json(json!({"title": "Web"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJ))
        .and(header("If-Match", "\"v7\""))
        .and(body_json(json!({"alias": "New"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let (_s, out) = call(
        (server, client),
        "vibemk_update_service_group",
        json!({"name": "web", "alias": "New"}),
    )
    .await;
    assert_eq!(text(&out), "✅ **Service Group Updated Successfully**\n\n**Name:** web\n**New Alias:** New\n\n⚠️ **Remember to activate changes!**");
}

#[tokio::test]
async fn update_without_etag_uses_the_wildcard_and_missing_group_is_reported() {
    let (server, client) = common::fake_checkmk().await;
    mount(&server, "GET", OBJ, 200, json!({"title": "Web"})).await;
    Mock::given(method("PUT"))
        .and(path(OBJ))
        .and(header("If-Match", "*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    assert!(text(
        &h.handle(
            "vibemk_update_service_group",
            &json!({"name": "web", "alias": "New"})
        )
        .await
    )
    .starts_with("✅"));
    server.reset().await;
    mount(&server, "GET", OBJ, 404, json!({})).await;
    let out = h
        .handle(
            "vibemk_update_service_group",
            &json!({"name": "web", "alias": "New"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Service group not found**\n\nService group 'web' not found"
    );
    assert!(is_error(&out));
    assert_eq!(
        text(
            &h.handle("vibemk_update_service_group", &json!({"name": "web"}))
                .await
        ),
        "❌ **Missing parameter**\n\nalias is required"
    );
}

#[tokio::test]
async fn delete_checks_existence_then_deletes() {
    let (server, client) = common::fake_checkmk().await;
    mount(&server, "GET", OBJ, 200, json!({"title": "Web"})).await;
    Mock::given(method("DELETE"))
        .and(path(OBJ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle("vibemk_delete_service_group", &json!({"name": "web"}))
        .await;
    assert_eq!(text(&out), "✅ **Service Group Deleted Successfully**\n\n**Deleted:** web\n\n⚠️ **Remember to activate changes!**");
    server.reset().await;
    mount(&server, "GET", OBJ, 404, json!({})).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let out = h
        .handle("vibemk_delete_service_group", &json!({"name": "web"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Service group not found**\n\nService group 'web' not found"
    );
}

#[tokio::test]
async fn bulk_create_posts_the_entries() {
    let (server, client) = common::fake_checkmk().await;
    let entries = json!([{"name": "a", "alias": "A"}, {"name": "b.c", "alias": "B"}]);
    Mock::given(method("POST"))
        .and(path(format!("{BULK}/bulk-create/invoke")))
        .and(body_json(json!({"entries": entries})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle(
            "vibemk_bulk_create_service_groups",
            &json!({"entries": entries}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Bulk Service Groups Created Successfully**\n\n**Created:** 2 service groups\n\n📋 **Created Groups:**\n• a (A)\n• b.c (B)\n\n⚠️ **Remember to activate changes!**");
    let bad = h
        .handle(
            "vibemk_bulk_create_service_groups",
            &json!({"entries": [{"name": "a b", "alias": "x"}]}),
        )
        .await;
    assert_eq!(
        text(&bad),
        "❌ **Invalid name**\n\nEntry 1: Service group name 'a b' contains invalid characters"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_create_service_groups",
                &json!({"entries": [{"name": "a"}]})
            )
            .await
        ),
        "❌ **Invalid entry**\n\nEntry 1 is missing 'alias'"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_bulk_create_service_groups", &json!({}))
                .await
        ),
        "❌ **Missing parameter**\n\nentries list is required"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_create_service_groups",
                &json!({"entries": "x"})
            )
            .await
        ),
        "❌ **Invalid parameter**\n\nentries must be a list"
    );
}

#[tokio::test]
async fn bulk_update_puts_the_entries() {
    let (server, client) = common::fake_checkmk().await;
    let entries = json!([{"name": "a", "attributes": {"alias": "A2"}}]);
    Mock::given(method("PUT"))
        .and(path(format!("{BULK}/bulk-update/invoke")))
        .and(body_json(json!({"entries": entries})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle(
            "vibemk_bulk_update_service_groups",
            &json!({"entries": entries}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Bulk Service Groups Updated Successfully**\n\n**Updated:** 1 service groups\n\n📋 **Updated Groups:**\n• a → A2\n\n⚠️ **Remember to activate changes!**");
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_update_service_groups",
                &json!({"entries": [{"name": "a", "attributes": {}}]})
            )
            .await
        ),
        "❌ **Invalid entry**\n\nEntry 1 is missing 'attributes.alias'"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_update_service_groups",
                &json!({"entries": ["a"]})
            )
            .await
        ),
        "❌ **Invalid entry**\n\nEntry 1 must be a dictionary"
    );
}

#[tokio::test]
async fn bulk_delete_posts_the_names() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(format!("{BULK}/bulk-delete/invoke")))
        .and(body_json(json!({"entries": ["a", "b"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceGroupHandler::new(client);
    let out = h
        .handle(
            "vibemk_bulk_delete_service_groups",
            &json!({"entries": ["a", "b"]}),
        )
        .await;
    assert_eq!(text(&out), "✅ **Bulk Service Groups Deleted Successfully**\n\n**Deleted:** 2 service groups\n\n📋 **Deleted Groups:**\n• a\n• b\n\n⚠️ **Remember to activate changes!**");
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_delete_service_groups",
                &json!({"entries": ["a", 3]})
            )
            .await
        ),
        "❌ **Invalid entry**\n\nEntry 2 must be a string (service group name)"
    );
    assert_eq!(
        text(
            &h.handle(
                "vibemk_bulk_delete_service_groups",
                &json!({"entries": ["  "]})
            )
            .await
        ),
        "❌ **Invalid entry**\n\nEntry 1 cannot be empty"
    );
}

#[tokio::test]
async fn api_errors_are_reported_as_checkmk_api_error() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let out = ServiceGroupHandler::new(client)
        .handle("vibemk_list_service_groups", &json!({}))
        .await;
    assert!(
        text(&out).starts_with("❌ **CheckMK API error**\n\n"),
        "{}",
        text(&out)
    );
    assert!(is_error(&out));
}
