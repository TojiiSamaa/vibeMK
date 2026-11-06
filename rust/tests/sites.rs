//! tools/call of every sites tool against a fake CheckMK.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::sites::SitesHandler;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn get_sites_lists_each_site() {
    let (server, client) = common::fake_checkmk().await;
    let body = json!({"value": [
        {"id": "remote1", "title": "R1", "extensions": {"basic_settings": {"alias": "Remote one"}, "logged_in": true,
            "configuration_connection": {"url_of_remote_site": "http://r1/cmk/", "enable_replication": true}}},
        {"id": "remote2", "title": "R2", "extensions": {}}
    ]});
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/domain-types/site_connection/collections/all",
            common::API
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let out = SitesHandler::new(client)
        .handle("vibemk_get_sites", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "🌐 **Monitoring Sites (2)**\n\n• **remote1** — Remote one [🟢 connected]\n  URL: http://r1/cmk/\n  Replication: enabled\n• **remote2** — R2 [🔴 not connected]\n  Replication: disabled"
    );
}

#[tokio::test]
async fn get_sites_says_when_there_are_none() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let out = SitesHandler::new(client)
        .handle("vibemk_get_sites", &json!({}))
        .await;
    assert_eq!(text(&out), "🌐 No sites found.");
}

#[tokio::test]
async fn login_site_posts_the_credentials() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{}/objects/site_connection/remote1/actions/login/invoke",
            common::API
        )))
        .and(body_json(json!({"username": "u", "password": "p"})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let out = SitesHandler::new(client)
        .handle(
            "vibemk_login_site",
            &json!({"site_id": "remote1", "username": "u", "password": "p"}),
        )
        .await;
    assert_eq!(text(&out), "✅ Logged in to site 'remote1'.");
}

#[tokio::test]
async fn login_site_needs_all_three_arguments() {
    let (_server, client) = common::fake_checkmk().await;
    let out = SitesHandler::new(client)
        .handle("vibemk_login_site", &json!({"site_id": "remote1"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nsite_id, username and password are required"
    );
    assert_eq!(out[1]["type"], "_error");
}

#[tokio::test]
async fn logout_site_posts_no_body() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{}/objects/site_connection/remote1/actions/logout/invoke",
            common::API
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let out = SitesHandler::new(client)
        .handle("vibemk_logout_site", &json!({"site_id": "remote1"}))
        .await;
    assert_eq!(text(&out), "✅ Logged out of site 'remote1'.");
    // No body: 2.5 rejects even an empty JSON object here.
    assert!(server.received_requests().await.unwrap()[0].body.is_empty());
}

#[tokio::test]
async fn a_site_id_cannot_leave_its_path_segment() {
    let (server, client) = common::fake_checkmk().await;
    let out = SitesHandler::new(client)
        .handle("vibemk_logout_site", &json!({"site_id": ".."}))
        .await;
    assert_eq!(out[1]["type"], "_error");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_api_error_is_reported() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"title": "boom"})))
        .mount(&server)
        .await;
    let out = SitesHandler::new(client)
        .handle("vibemk_get_sites", &json!({}))
        .await;
    assert!(
        text(&out).starts_with("❌ **CheckMK API Error**"),
        "{}",
        text(&out)
    );
}

#[tokio::test]
async fn an_unknown_tool_is_refused() {
    let (_server, client) = common::fake_checkmk().await;
    let out = SitesHandler::new(client).handle("nope", &json!({})).await;
    assert_eq!(
        text(&out),
        "❌ **Unknown tool**\n\nTool 'nope' is not supported"
    );
}
