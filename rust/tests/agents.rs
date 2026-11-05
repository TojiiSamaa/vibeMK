//! tools/call of every agent bakery tool against a fake CheckMK.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::agents::AgentHandler;
use vibemk::handlers::base::Handler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn bake_agents_posts_to_the_bake_action() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{}/domain-types/agent/actions/bake/invoke",
            common::API
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let out = AgentHandler::new(client)
        .handle("vibemk_bake_agents", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "🍞 **Agent Baking Started**\n\nUse vibemk_baking_status to check progress."
    );
}

#[tokio::test]
async fn baking_status_reads_the_job_in_result_value() {
    let (server, client) = common::fake_checkmk().await;
    let job = json!({"result": {"value": {"state": "finished", "started": 1700000000, "duration": 12.34, "loginfo": {"JobResult": ["ok one", "ok two"], "JobException": ["boom"]}}}});
    Mock::given(method("GET"))
        .and(path(format!(
            "{}/domain-types/agent/actions/baking_status/invoke",
            common::API
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(job))
        .mount(&server)
        .await;
    let out = AgentHandler::new(client)
        .handle("vibemk_baking_status", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "🍞 **Agent Baking Status**\n\nState: finished\nStarted: 2023-11-14 22:13:20 UTC\nDuration: 12.3s\nResult: ok one\nResult: ok two\nError: boom"
    );
}

#[tokio::test]
async fn baking_status_falls_back_to_the_top_level_and_to_unknown() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let out = AgentHandler::new(client)
        .handle("vibemk_baking_status", &json!({}))
        .await;
    assert_eq!(text(&out), "🍞 **Agent Baking Status**\n\nState: unknown");
}

#[tokio::test]
async fn download_gives_the_url_without_calling_the_api() {
    let (server, client) = common::fake_checkmk().await;
    let out = AgentHandler::new(client)
        .handle(
            "vibemk_download_agent_by_host",
            &json!({"host_name": "web 01"}),
        )
        .await;
    let url = format!("{}{}/domain-types/agent/actions/download_by_host/invoke?os_type=linux_deb&host_name=web+01&agent_type=host_name", server.uri(), common::API);
    let t = text(&out);
    assert!(
        t.starts_with(&format!(
            "📦 **Agent Download URL**\n\nHost: web 01\nOS type: linux_deb\nURL: {url}\n\n"
        )),
        "{t}"
    );
    assert!(t.contains(&format!(
        "  curl -H 'Authorization: Bearer USER PASS' '{url}' -o agent.linux_deb\n"
    )));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn download_needs_a_host_name() {
    let (_server, client) = common::fake_checkmk().await;
    let out = AgentHandler::new(client)
        .handle("vibemk_download_agent_by_host", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nhost_name is required"
    );
    assert_eq!(out[1]["type"], "_error");
}

#[tokio::test]
async fn a_404_means_this_edition_has_no_bakery() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let out = AgentHandler::new(client)
        .handle("vibemk_bake_agents", &json!({}))
        .await;
    assert!(
        text(&out).starts_with(
            "❌ **Agent bakery not available**\n\nThis CheckMK edition has no agent bakery"
        ),
        "{out:?}"
    );
}

#[tokio::test]
async fn other_api_errors_and_unknown_tools_are_errors() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let h = AgentHandler::new(client);
    assert!(text(&h.handle("vibemk_bake_agents", &json!({})).await)
        .starts_with("❌ **CheckMK API Error**"));
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
