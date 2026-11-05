//! Port of tests/test_checkmk_endpoints.py: the paths vibeMK calls exist in the
//! CheckMK 2.4 REST API (read from cmk/gui/openapi/endpoints at v2.4.0p2).

mod common;

use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

const SINGLE_HOST_DISCOVERY: &str = "domain-types/service_discovery_run/actions/start/invoke";
const BULK_DISCOVERY: &str = "domain-types/discovery_run/actions/bulk-discovery-start/invoke";
const BACKGROUND_JOB: &str = "objects/background_job/";

async fn dispatcher() -> (MockServer, Dispatcher) {
    let (server, client) = fake_checkmk().await;
    (server, Dispatcher::from_client(client, false))
}

async fn mount(server: &MockServer, m: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(m))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

/// (method, path below the API root) of every request received.
async fn calls(server: &MockServer) -> Vec<(String, String)> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| {
            (
                r.method.to_string(),
                r.url
                    .path()
                    .trim_start_matches(&format!("{API}/"))
                    .to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn single_host_discovery_uses_service_discovery_run() {
    let (server, d) = dispatcher().await;
    // The start answers 303; the handler must see it, not follow it.
    mount(&server, "POST", SINGLE_HOST_DISCOVERY, 303, json!({})).await;
    d.call_tool(
        "vibemk_start_service_discovery",
        Some(&json!({"host_name": "example.com"})),
    )
    .await;
    let c = calls(&server).await;
    assert_eq!(
        c.first(),
        Some(&("POST".to_string(), SINGLE_HOST_DISCOVERY.to_string())),
        "{c:?}"
    );
    assert!(!c.iter().any(|(_, p)| p == BULK_DISCOVERY), "{c:?}");
}

#[tokio::test]
async fn bulk_discovery_uses_discovery_run() {
    let (server, d) = dispatcher().await;
    mount(&server, "POST", BULK_DISCOVERY, 200, json!({})).await;
    d.call_tool(
        "vibemk_start_bulk_discovery",
        Some(&json!({"hostnames": ["a", "b"]})),
    )
    .await;
    let posts: Vec<_> = calls(&server)
        .await
        .into_iter()
        .filter(|(m, _)| m == "POST")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(posts, vec![BULK_DISCOVERY]);
}

#[tokio::test]
async fn bulk_discovery_status_queries_the_background_job() {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        "objects/background_job/job-42",
        200,
        json!({}),
    )
    .await;
    d.call_tool(
        "vibemk_get_bulk_discovery_status",
        Some(&json!({"job_id": "job-42"})),
    )
    .await;
    let c = calls(&server).await;
    assert!(!c.is_empty(), "no request was made");
    assert!(
        c.iter().all(|(_, p)| p.starts_with(BACKGROUND_JOB)),
        "{c:?}"
    );
}

#[test]
fn all_documented_discovery_modes_are_accepted() {
    let tool = vibemk::tools::tool("vibemk_start_service_discovery").expect("tool");
    let declared: Vec<&str> = tool["inputSchema"]["properties"]["mode"]["enum"]
        .as_array()
        .expect("enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for m in ["tabula_rasa", "only_service_labels"] {
        assert!(declared.contains(&m), "{m} missing from {declared:?}");
    }
}

#[tokio::test]
async fn service_status_never_calls_a_livestatus_endpoint() {
    // No Livestatus passthrough exists in the REST API; every attempt fails so the whole chain runs.
    let (server, d) = dispatcher().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"title": "boom"})))
        .mount(&server)
        .await;
    d.call_tool(
        "vibemk_get_service_status",
        Some(&json!({"host_name": "example.com", "service_description": "CPU utilization"})),
    )
    .await;
    let c = calls(&server).await;
    assert!(!c.is_empty());
    assert!(
        !c.iter()
            .any(|(_, p)| p.contains("bi_rule") || p.contains("livestatus")),
        "{c:?}"
    );
}

#[test]
fn discover_services_tool_is_not_advertised() {
    let names = vibemk::tools::tool_names();
    assert!(!names.contains(&"vibemk_discover_services"));
    assert!(
        names.contains(&"vibemk_start_service_discovery")
            && names.contains(&"vibemk_start_bulk_discovery")
    );
}

#[tokio::test]
async fn the_check_output_reaches_the_caller() {
    // show_service has no plugin_output; the service collection does.
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        "domain-types/service/collections/all",
        200,
        json!({"value": [{"extensions": {"host_name": "web01", "description": "Filesystem /var", "state": 2, "plugin_output": "CRIT - 94.1% used (89.2 of 94.8 GiB)"}}]}),
    )
    .await;
    let r = d
        .call_tool(
            "vibemk_get_service_status",
            Some(&json!({"host_name": "web01", "service_description": "Filesystem /var"})),
        )
        .await;
    let t = r["content"][0]["text"].as_str().unwrap_or_default();
    assert!(t.contains("94.1% used"), "{t}");
    assert!(t.contains("CRITICAL"), "{t}");
}

fn handler_source(file: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/src/handlers/{file}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("handler source")
}

#[test]
fn no_handler_calls_the_service_object_endpoint() {
    // objects/service/{host}/{description} answers 404 on 2.4.
    assert!(!handler_source("services.rs").contains("objects/service/"));
}

#[test]
fn no_handler_posts_to_show_service() {
    // show_service is a GET action; POST answers 405 on 2.4.
    let src = handler_source("services.rs");
    assert!(!src
        .lines()
        .any(|l| l.contains("client.post(") && l.contains("show_service")));
}
