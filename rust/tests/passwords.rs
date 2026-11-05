//! A tools/call per password tool, through the real dispatcher, against a fake CheckMK.
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

const SECRET: &str = "s3cr3t-Value-42";

#[tokio::test]
async fn tools_call_get_passwords_lists_metadata_and_never_a_secret() {
    let (server, d) = setup().await;
    let body = json!({"value": [{"id": "db", "extensions": {"title": "Database", "comment": "prod database login", "owner": "admin", "shared": ["a", "b"], "password": SECRET}}]});
    Mock::given(method("GET"))
        .and(path(url("domain-types/password/collections/all")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let r = d.call_tool("vibemk_get_passwords", Some(&json!({}))).await;
    assert_eq!(r["isError"], false);
    assert_eq!(text(&r), "🔐 **Stored Passwords** (1 total):\n\n🔐 **db** - Database\n   Owner: admin\n   Shared with 2 users/groups\n   Comment: prod database login...");
    assert!(!r.to_string().contains(SECRET));
}

#[tokio::test]
async fn tools_call_create_password_posts_it_but_never_echoes_it() {
    let (server, d) = setup().await;
    Mock::given(method("POST"))
        .and(path(url("domain-types/password/collections/all")))
        .and(body_json(
            json!({"ident": "db", "password": SECRET, "owner": "admin", "title": "Database"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_create_password",
            Some(&json!({"ident": "db", "password": SECRET, "title": "Database"})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert!(text(&r).starts_with("✅ **Password Created Successfully**\n\nIdentifier: db\nTitle: Database\nOwner: admin\nShared with: 0 users/groups\nComment: None\n"), "{}", text(&r));
    assert!(!r.to_string().contains(SECRET));
}

#[tokio::test]
async fn tools_call_update_password_sends_if_match_and_names_only_the_fields() {
    let (server, d) = setup().await;
    let p = url("objects/password/db");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"e1\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(p))
        .and(header("If-Match", "\"e1\""))
        .and(body_json(json!({"password": SECRET})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool(
            "vibemk_update_password",
            Some(&json!({"ident": "db", "password": SECRET})),
        )
        .await;
    assert_eq!(r["isError"], false);
    assert!(
        text(&r).contains("Updated fields: password\n"),
        "{}",
        text(&r)
    );
    assert!(!r.to_string().contains(SECRET));
}

#[tokio::test]
async fn tools_call_delete_password_sends_if_match() {
    let (server, d) = setup().await;
    let p = url("objects/password/db");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"e2\"")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(p))
        .and(header("If-Match", "\"e2\""))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let r = d
        .call_tool("vibemk_delete_password", Some(&json!({"ident": "db"})))
        .await;
    assert_eq!(r["isError"], false);
    assert!(
        text(&r).starts_with("✅ **Password Deleted Successfully**\n\nIdentifier: db\n"),
        "{}",
        text(&r)
    );
}
