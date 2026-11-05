//! Tools of src/vibemk/handlers/user_roles.py: one call per tool against a fake CheckMK,
//! checking method, path and body as the server received them.

mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::user_roles::UserRolesHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const ROLES: &str = "domain-types/user_role/collections/all";
const OPS: &str = "objects/user_role/ops";

async fn handler() -> (MockServer, UserRolesHandler) {
    let (server, client) = fake_checkmk().await;
    (server, UserRolesHandler::new(client))
}

async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn sent(server: &MockServer, m: &str, p: &str) -> Vec<wiremock::Request> {
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

#[tokio::test]
async fn list_splits_builtin_and_custom_roles() {
    let (server, h) = handler().await;
    mount(&server, "GET", ROLES, 200, json!({"value": [
        {"id": "admin", "extensions": {"alias": "Administrator", "builtin": true, "permissions": ["a", "b"]}},
        {"id": "ops", "extensions": {"alias": "Operators", "permissions": ["a"]}}
    ]})).await;
    let out = h.handle("vibemk_list_user_roles", &json!({})).await;
    assert_eq!(
        text(&out),
        "👥 **User Roles**\n\n**🏗️ Built-in Roles** (cannot be deleted):\n👑 **admin** - Administrator\n   📊 2 permissions\n   💡 Full CheckMK administrator with all permissions\n\n**🎨 Custom Roles**:\n✏️ **ops** - Operators\n   📊 1 permissions\n\n**💡 Available Operations:**\n• `vibemk_show_user_role` - View detailed role information\n• `vibemk_create_user_role` - Clone an existing role\n• `vibemk_update_user_role` - Modify custom role permissions\n• `vibemk_delete_user_role` - Delete custom roles\n"
    );
    let out = h
        .handle("vibemk_list_user_roles", &json!({"show_builtin": false}))
        .await;
    assert!(!text(&out).contains("Built-in Roles") && text(&out).contains("ops"));
    assert_eq!(sent(&server, "GET", ROLES).await.len(), 2);
}

#[tokio::test]
async fn list_empty_and_http_errors() {
    let (server, h) = handler().await;
    mount(&server, "GET", ROLES, 200, json!({"value": []})).await;
    assert!(text(&h.handle("vibemk_list_user_roles", &json!({})).await)
        .starts_with("👥 **No User Roles Found**"));
    drop(server);
    for (status, expected) in [
        (403, "❌ **Permission Denied**\n\nAccess denied. You need 'wato.users' permission for user role management."),
        (406, "❌ **Accept Header Error**\n\nAPI cannot satisfy the requested content type."),
    ] {
        let (server, h) = handler().await;
        mount(&server, "GET", ROLES, status, json!({"title": "x"})).await;
        let out = h.handle("vibemk_list_user_roles", &json!({})).await;
        assert_eq!(text(&out), expected);
        assert_eq!(out[1]["type"], "_error");
        drop(server);
    }
    let (server, h) = handler().await;
    mount(&server, "GET", ROLES, 500, json!({"title": "boom"})).await;
    assert!(text(&h.handle("vibemk_list_user_roles", &json!({})).await)
        .starts_with("❌ **Failed to list user roles**"));
    drop(server);
}

#[tokio::test]
async fn show_custom_role_with_permission_list() {
    let (server, h) = handler().await;
    let perms: Vec<String> = (1..=12).map(|i| format!("p{i}")).collect();
    mount(
        &server,
        "GET",
        OPS,
        200,
        json!({"extensions": {"alias": "Operators", "permissions": perms}}),
    )
    .await;
    let out = h
        .handle("vibemk_show_user_role", &json!({"role_id": "ops"}))
        .await;
    let t = text(&out);
    assert!(t.starts_with("👥 **User Role Details: ops**\n\n🎨 **Custom Role**\n\n**Role Information:**\n• **ID**: `ops`\n• **Alias**: Operators\n• **Type**: Custom\n• **Permissions**: 12 total\n\n**Key Permissions** (showing first 10):\n✅ `p1`\n"));
    assert!(t.contains("✅ `p10`\n... and 2 more permissions\n") && !t.contains("`p11`"));
    assert!(t.ends_with("\n**Available Operations:**\n• `vibemk_update_user_role` - Modify this role\n• `vibemk_delete_user_role` - Delete this role\n• `vibemk_create_user_role` - Clone this role to create a new one\n"));
    assert_eq!(sent(&server, "GET", OPS).await.len(), 1);
}

#[tokio::test]
async fn show_builtin_role_with_permission_map_and_errors() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        "objects/user_role/guest",
        200,
        json!({"extensions": {"permissions": {"read": true, "write": false}}}),
    )
    .await;
    let t = text(
        &h.handle("vibemk_show_user_role", &json!({"role_id": "guest"}))
            .await,
    );
    assert!(
        t.contains("🏗️ **Built-in Role** (cannot be deleted)")
            && t.contains("• **Type**: Built-in")
    );
    assert!(t.contains(
        "**Description:**\nRead-only access - can see everything but change nothing\n\n"
    ));
    assert!(t.contains("✅ `read`\n❌ `write`\n"));
    assert!(!t.contains("Modify this role"));
    mount(&server, "GET", OPS, 404, json!({"title": "nf"})).await;
    assert_eq!(
        text(
            &h.handle("vibemk_show_user_role", &json!({"role_id": "ops"}))
                .await
        ),
        "❌ **Role Not Found**\n\nUser role 'ops' not found. Check the role ID and try again."
    );
    assert_eq!(
        text(&h.handle("vibemk_show_user_role", &json!({})).await),
        "❌ **Missing parameter**\n\nrole_id is required"
    );
}

#[tokio::test]
async fn create_posts_the_clone_request() {
    let (server, h) = handler().await;
    mount(&server, "POST", ROLES, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_create_user_role",
            &json!({"base_role_id": "user", "new_role_id": "ops", "new_alias": "Operators"}),
        )
        .await;
    assert_eq!(
        sent(&server, "POST", ROLES).await[0]
            .body_json::<Value>()
            .unwrap(),
        json!({"role_id": "user", "new_role_id": "ops", "new_alias": "Operators"})
    );
    assert!(text(&out).starts_with("✅ **User Role Created Successfully**\n\n**New Role Details:**\n• **Role ID**: `ops`\n• **Cloned from**: `user`\n• **Alias**: Operators\n\n"));
    h.handle(
        "vibemk_create_user_role",
        &json!({"base_role_id": "user", "new_role_id": "ops2"}),
    )
    .await;
    let body: Value = sent(&server, "POST", ROLES).await[1].body_json().unwrap();
    assert_eq!(body, json!({"role_id": "user", "new_role_id": "ops2"}));
    assert_eq!(
        text(
            &h.handle("vibemk_create_user_role", &json!({"base_role_id": "user"}))
                .await
        ),
        "❌ **Missing parameters**\n\nbase_role_id and new_role_id are required"
    );
}

#[tokio::test]
async fn create_maps_http_statuses() {
    for (status, head) in [
        (
            400,
            "❌ **Invalid Parameters**\n\nInvalid role parameters: ",
        ),
        (403, "❌ **Permission Denied**"),
        (
            409,
            "❌ **Role Already Exists**\n\nRole 'ops' already exists.",
        ),
    ] {
        let (server, h) = handler().await;
        mount(
            &server,
            "POST",
            ROLES,
            status,
            json!({"title": "t", "detail": "bad id"}),
        )
        .await;
        let t = text(
            &h.handle(
                "vibemk_create_user_role",
                &json!({"base_role_id": "user", "new_role_id": "ops"}),
            )
            .await,
        );
        assert!(t.starts_with(head), "{status}: {t}");
        drop(server);
    }
}

#[tokio::test]
async fn update_puts_new_alias_and_permissions_without_if_match() {
    let (server, h) = handler().await;
    mount(&server, "PUT", OPS, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_update_user_role",
            &json!({"role_id": "ops", "alias": "Ops 2", "permissions": {"wato.edit": true}}),
        )
        .await;
    let req = &sent(&server, "PUT", OPS).await[0];
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"new_alias": "Ops 2", "permissions": {"wato.edit": true}})
    );
    assert!(
        req.headers.get("if-match").is_none(),
        "AUDIT E7: no If-Match on role updates"
    );
    assert_eq!(
        text(&out),
        "✅ **User Role Updated Successfully**\n\n**Updated Role**: `ops`\n\n**Changes Applied:**\n• **Alias**: Ops 2\n• **Permissions**: Updated 1 permissions\n\n💡 **Use `vibemk_show_user_role` to view the updated role details**"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_update_user_role", &json!({"role_id": "ops"}))
                .await
        ),
        "❌ **No updates specified**\n\nAt least one of 'alias' or 'permissions' must be provided"
    );
    assert_eq!(sent(&server, "PUT", OPS).await.len(), 1);
}

#[tokio::test]
async fn update_not_found_is_translated() {
    let (server, h) = handler().await;
    mount(&server, "PUT", OPS, 404, json!({"title": "nf"})).await;
    assert_eq!(
        text(
            &h.handle(
                "vibemk_update_user_role",
                &json!({"role_id": "ops", "alias": "x"})
            )
            .await
        ),
        "❌ **Role Not Found**\n\nUser role 'ops' not found."
    );
    drop(server);
}

#[tokio::test]
async fn delete_calls_the_api_and_refuses_builtin_roles() {
    let (server, h) = handler().await;
    mount(&server, "DELETE", OPS, 204, json!({})).await;
    let out = h
        .handle("vibemk_delete_user_role", &json!({"role_id": "ops"}))
        .await;
    assert_eq!(sent(&server, "DELETE", OPS).await.len(), 1);
    assert!(text(&out).starts_with("✅ **User Role Deleted Successfully**\n\n**Deleted Role**: `ops`\n\n⚠️ **Important Notes:**"));
    for builtin in ["admin", "user", "guest"] {
        let out = h
            .handle("vibemk_delete_user_role", &json!({"role_id": builtin}))
            .await;
        assert!(text(&out).starts_with("❌ **Cannot Delete Built-in Role**"));
        assert_eq!(out[1]["type"], "_error");
    }
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        1
    );
}

#[tokio::test]
async fn delete_maps_http_statuses_and_unsafe_ids() {
    let (server, h) = handler().await;
    mount(&server, "DELETE", OPS, 400, json!({})).await;
    assert_eq!(text(&h.handle("vibemk_delete_user_role", &json!({"role_id": "ops"})).await), "❌ **Cannot Delete Role**\n\nRole 'ops' cannot be deleted. It may still be in use by users.");
    let before = server.received_requests().await.unwrap_or_default().len();
    let out = h
        .handle("vibemk_delete_user_role", &json!({"role_id": ".."}))
        .await;
    assert!(text(&out).starts_with("❌ **Unexpected Error**"));
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        before
    );
}

#[tokio::test]
async fn unknown_tool_is_reported() {
    let (_server, h) = handler().await;
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
