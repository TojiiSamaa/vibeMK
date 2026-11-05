//! Tools of src/vibemk/handlers/users.py: one call per tool against a fake CheckMK,
//! checking method, path, body and If-Match as the server received them.

mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::users::UserHandler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const USERS: &str = "domain-types/user_config/collections/all";
const GROUPS: &str = "domain-types/contact_group_config/collections/all";
const ALICE: &str = "objects/user_config/alice";
const OPS: &str = "objects/contact_group_config/ops";

async fn handler() -> (MockServer, UserHandler) {
    let (server, client) = fake_checkmk().await;
    (server, UserHandler::new(client))
}

async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

/// GET answering with an ETag header, as CheckMK does for single objects.
async fn mount_get_etag(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/{p}")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v7\"")
                .set_body_json(body),
        )
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

fn if_match(r: &wiremock::Request) -> String {
    r.headers
        .get("if-match")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn get_users_lists_every_user() {
    let (server, h) = handler().await;
    mount(&server, "GET", USERS, 200, json!({"value": [
        {"id": "alice", "extensions": {"fullname": "Alice A", "contact_options": {"email": "a@x.io"}, "roles": ["admin", "user"]}},
        {"id": "bob", "extensions": {"disable_login": true}}
    ]})).await;
    let out = h.handle("vibemk_get_users", &json!({})).await;
    assert_eq!(
        text(&out),
        "👥 **CheckMK Users** (2 total):\n\n👤 **alice** (Alice A)\n   📧 a@x.io\n   🔑 Roles: admin, user\n   ✅ Active\n\n👤 **bob** (bob)\n   📧 No email\n   🔑 Roles: \n   🔒 Disabled"
    );
    assert_eq!(sent(&server, "GET", USERS).await.len(), 1);
}

#[tokio::test]
async fn get_users_empty_and_api_error() {
    let (server, h) = handler().await;
    mount(&server, "GET", USERS, 200, json!({"value": []})).await;
    assert_eq!(
        text(&h.handle("vibemk_get_users", &json!({})).await),
        "👥 **No Users Found**\n\nNo CheckMK users are configured."
    );
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        USERS,
        403,
        json!({"title": "Forbidden", "detail": "no right"}),
    )
    .await;
    let out = h.handle("vibemk_get_users", &json!({})).await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert_eq!(out[1]["type"], "_error");
    drop(server);
}

#[tokio::test]
async fn create_user_posts_the_full_body() {
    let (server, h) = handler().await;
    mount(&server, "POST", USERS, 200, json!({})).await;
    let args = json!({"username": "carol", "fullname": "Carol C", "email": "c@x.io", "password": "fixture-pw-1", "roles": ["admin"], "contactgroups": ["ops"]});
    let out = h.handle("vibemk_create_user", &args).await;
    let req = &sent(&server, "POST", USERS).await[0];
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"username": "carol", "fullname": "Carol C", "disable_notifications": {}, "interface_options": {}, "auth_option": {"auth_type": "password", "password": "fixture-pw-1"}, "contact_options": {"email": "c@x.io"}, "roles": ["admin"], "contactgroups": ["ops"]})
    );
    assert_eq!(
        text(&out),
        "✅ **User Created Successfully**\n\nUsername: carol\nFull Name: Carol C\nEmail: c@x.io\nRoles: admin\nContact Groups: ops\nLogin: Enabled\n\n⚠️ **Remember to activate changes!**"
    );
    assert!(
        !text(&out).contains("fixture-pw-1"),
        "the password must never be echoed"
    );
}

#[tokio::test]
async fn create_user_defaults_and_validation() {
    let (server, h) = handler().await;
    mount(&server, "POST", USERS, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_create_user",
            &json!({"username": "dan", "fullname": "Dan D"}),
        )
        .await;
    let body: Value = sent(&server, "POST", USERS).await[0].body_json().unwrap();
    assert_eq!(body["roles"], json!(["user"]));
    assert_eq!(body["contactgroups"], json!(["all"]));
    assert!(body.get("auth_option").is_none() && body.get("contact_options").is_none());
    assert!(
        text(&out).contains("Email: Not set\nRoles: user\nContact Groups: all\nLogin: Disabled")
    );
    let out = h
        .handle("vibemk_create_user", &json!({"username": "x"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameters**\n\nusername and fullname are required"
    );
    assert_eq!(sent(&server, "POST", USERS).await.len(), 1);
}

#[tokio::test]
async fn create_user_failure_never_echoes_the_password() {
    let (server, h) = handler().await;
    mount(
        &server,
        "POST",
        USERS,
        400,
        json!({"title": "Bad", "detail": "invalid"}),
    )
    .await;
    let out = h
        .handle(
            "vibemk_create_user",
            &json!({"username": "eve", "fullname": "Eve", "password": "fixture-pw-2"}),
        )
        .await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert!(!text(&out).contains("fixture-pw-2"));
    drop(server);
}

#[tokio::test]
async fn update_user_puts_changed_fields_with_the_current_etag() {
    let (server, h) = handler().await;
    mount_get_etag(&server, ALICE, json!({"id": "alice"})).await;
    mount(&server, "PUT", ALICE, 200, json!({})).await;
    let out = h.handle("vibemk_update_user", &json!({"username": "alice", "fullname": "Alice B", "email": "ab@x.io", "roles": ["user"], "contactgroups": []})).await;
    let req = &sent(&server, "PUT", ALICE).await[0];
    assert_eq!(if_match(req), "\"v7\"");
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"fullname": "Alice B", "contact_options": {"email": "ab@x.io"}, "roles": ["user"], "contactgroups": []})
    );
    assert_eq!(
        text(&out),
        "✅ **User Updated Successfully**\n\nUsername: alice\nUpdated fields: fullname, contact_options, roles, contactgroups\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn update_user_unknown_user_is_an_api_error_and_sends_no_put() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALICE, 404, json!({"title": "Not Found"})).await;
    let out = h
        .handle(
            "vibemk_update_user",
            &json!({"username": "alice", "fullname": "A"}),
        )
        .await;
    assert!(text(&out).starts_with("❌ **CheckMK API Error**"));
    assert!(sent(&server, "PUT", ALICE).await.is_empty());
    let out = h.handle("vibemk_update_user", &json!({})).await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nusername is required"
    );
}

#[tokio::test]
async fn delete_user_checks_then_deletes() {
    let (server, h) = handler().await;
    mount(&server, "GET", ALICE, 200, json!({"id": "alice"})).await;
    mount(&server, "DELETE", ALICE, 204, json!({})).await;
    let out = h
        .handle("vibemk_delete_user", &json!({"username": "alice"}))
        .await;
    assert_eq!(sent(&server, "GET", ALICE).await.len(), 1);
    assert_eq!(sent(&server, "DELETE", ALICE).await.len(), 1);
    assert!(text(&out).starts_with(
        "✅ **User Deleted Successfully**\n\nUsername: alice\n\n📝 **Next Steps:**\n1️⃣"
    ));
    assert!(
        text(&out).ends_with("The user is only marked for deletion until you activate changes!")
    );
}

#[tokio::test]
async fn unsafe_username_is_refused_before_any_request() {
    let (server, h) = handler().await;
    let out = h
        .handle("vibemk_delete_user", &json!({"username": ".."}))
        .await;
    assert_eq!(out[1]["type"], "_error");
    assert!(server
        .received_requests()
        .await
        .unwrap_or_default()
        .is_empty());
}

#[tokio::test]
async fn get_contact_groups_lists_groups() {
    let (server, h) = handler().await;
    mount(
        &server,
        "GET",
        GROUPS,
        200,
        json!({"value": [{"id": "ops", "extensions": {"alias": "Operations"}}, {"id": "dev"}]}),
    )
    .await;
    let out = h.handle("vibemk_get_contact_groups", &json!({})).await;
    assert_eq!(
        text(&out),
        "👥 **Contact Groups** (2 total):\n\n👥 **ops** - Operations\n👥 **dev** - dev"
    );
    let (server, h) = handler().await;
    mount(&server, "GET", GROUPS, 200, json!({"value": []})).await;
    assert_eq!(
        text(&h.handle("vibemk_get_contact_groups", &json!({})).await),
        "👥 **No Contact Groups Found**\n\nNo contact groups are configured."
    );
    drop(server);
}

#[tokio::test]
async fn create_contact_group_posts_name_and_alias_only() {
    let (server, h) = handler().await;
    mount(&server, "POST", GROUPS, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_create_contact_group",
            &json!({"name": "ops", "alias": "Operations", "members": ["alice", "bob"]}),
        )
        .await;
    assert_eq!(
        sent(&server, "POST", GROUPS).await[0]
            .body_json::<Value>()
            .unwrap(),
        json!({"name": "ops", "alias": "Operations"})
    );
    assert_eq!(text(&out), "✅ **Contact Group Created Successfully**\n\nName: ops\nAlias: Operations\nMembers: 2 users\n\n⚠️ **Remember to activate changes!**");
    assert_eq!(
        text(
            &h.handle("vibemk_create_contact_group", &json!({"name": "x"}))
                .await
        ),
        "❌ **Missing parameters**\n\nname and alias are required"
    );
}

#[tokio::test]
async fn update_contact_group_puts_alias_with_the_current_etag() {
    let (server, h) = handler().await;
    mount_get_etag(&server, OPS, json!({"id": "ops"})).await;
    mount(&server, "PUT", OPS, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_update_contact_group",
            &json!({"name": "ops", "alias": "Ops 2"}),
        )
        .await;
    let req = &sent(&server, "PUT", OPS).await[0];
    assert_eq!(if_match(req), "\"v7\"");
    assert_eq!(req.body_json::<Value>().unwrap(), json!({"alias": "Ops 2"}));
    assert_eq!(text(&out), "✅ **Contact Group Updated Successfully**\n\nName: ops\nUpdated fields: alias\n\n⚠️ **Remember to activate changes!**");
    let out = h
        .handle("vibemk_update_contact_group", &json!({"name": "ops"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **No data to update**\n\nAt least alias must be provided"
    );
    assert_eq!(sent(&server, "PUT", OPS).await.len(), 1);
}

#[tokio::test]
async fn delete_contact_group_checks_then_deletes() {
    let (server, h) = handler().await;
    mount(&server, "GET", OPS, 200, json!({"id": "ops"})).await;
    mount(&server, "DELETE", OPS, 204, json!({})).await;
    let out = h
        .handle("vibemk_delete_contact_group", &json!({"name": "ops"}))
        .await;
    assert_eq!(sent(&server, "DELETE", OPS).await.len(), 1);
    assert!(text(&out).starts_with("✅ **Contact Group Deleted Successfully**\n\nName: ops\n\n"));
    assert!(text(&out)
        .ends_with("The contact group is only marked for deletion until you activate changes!"));
}

#[tokio::test]
async fn add_user_to_group_appends_and_sends_if_match() {
    let (server, h) = handler().await;
    mount_get_etag(
        &server,
        ALICE,
        json!({"extensions": {"contactgroups": ["all"]}}),
    )
    .await;
    mount(&server, "PUT", ALICE, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_add_user_to_group",
            &json!({"username": "alice", "group_name": "ops"}),
        )
        .await;
    let req = &sent(&server, "PUT", ALICE).await[0];
    assert_eq!(if_match(req), "\"v7\"");
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"contactgroups": ["all", "ops"]})
    );
    assert_eq!(text(&out), "✅ **User Added to Contact Group**\n\nUsername: alice\nAdded to group: ops\nTotal groups: all, ops\n\n⚠️ **Remember to activate changes!**");
    let out = h
        .handle(
            "vibemk_add_user_to_group",
            &json!({"username": "alice", "group_name": "all"}),
        )
        .await;
    assert_eq!(text(&out), "ℹ️ **User Already in Group**\n\nUsername: alice\nGroup: all\n\nUser is already a member of this contact group.");
    assert_eq!(sent(&server, "PUT", ALICE).await.len(), 1);
}

#[tokio::test]
async fn remove_user_from_group_removes_and_sends_if_match() {
    let (server, h) = handler().await;
    mount_get_etag(
        &server,
        ALICE,
        json!({"extensions": {"contactgroups": ["all", "ops"]}}),
    )
    .await;
    mount(&server, "PUT", ALICE, 200, json!({})).await;
    let out = h
        .handle(
            "vibemk_remove_user_from_group",
            &json!({"username": "alice", "group_name": "ops"}),
        )
        .await;
    let req = &sent(&server, "PUT", ALICE).await[0];
    assert_eq!(if_match(req), "\"v7\"");
    assert_eq!(
        req.body_json::<Value>().unwrap(),
        json!({"contactgroups": ["all"]})
    );
    assert_eq!(text(&out), "✅ **User Removed from Contact Group**\n\nUsername: alice\nRemoved from group: ops\nRemaining groups: all\n\n⚠️ **Remember to activate changes!**");
    let out = h
        .handle(
            "vibemk_remove_user_from_group",
            &json!({"username": "alice", "group_name": "zzz"}),
        )
        .await;
    assert_eq!(text(&out), "ℹ️ **User Not in Group**\n\nUsername: alice\nGroup: zzz\n\nUser is not a member of this contact group.");
}

#[tokio::test]
async fn unknown_tool_is_reported() {
    let (_server, h) = handler().await;
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
