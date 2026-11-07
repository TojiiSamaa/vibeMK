//! Port of tests/test_integration.py (9 tests). Like the Python suite these need a real CheckMK:
//! they run only with INTEGRATION_TESTS=true and CHECKMK_SERVER_URL/USERNAME/PASSWORD, else they return at once.

use std::sync::Arc;

use serde_json::{json, Value};
use vibemk::client::CheckMkClient;
use vibemk::config::Config;
use vibemk::dispatch::Dispatcher;
use vibemk::mcp::handle_request;
use vibemk::registry::ToolRegistry;

fn enabled() -> bool {
    std::env::var("INTEGRATION_TESTS").as_deref() == Ok("true")
        && ["CHECKMK_SERVER_URL", "CHECKMK_USERNAME", "CHECKMK_PASSWORD"]
            .iter()
            .all(|k| std::env::var(k).is_ok())
}

fn dispatcher_for(config: Config) -> Dispatcher {
    Dispatcher::new(Arc::new(ToolRegistry::from_client(Arc::new(
        CheckMkClient::new(config),
    ))))
}

fn real_dispatcher() -> Dispatcher {
    dispatcher_for(Config::from_env().expect("CHECKMK_* configuration"))
}

async fn call(d: &Dispatcher, tool: &str, args: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    handle_request(d, &request).await.unwrap()["result"].clone()
}

#[tokio::test]
async fn real_connection() {
    if !enabled() {
        return;
    }
    let r = call(
        &real_dispatcher(),
        "vibemk_debug_checkmk_connection",
        json!({}),
    )
    .await;
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn real_hosts_list() {
    if !enabled() {
        return;
    }
    let r = call(&real_dispatcher(), "vibemk_get_checkmk_hosts", json!({})).await;
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn real_mcp_server_workflow() {
    if !enabled() {
        return;
    }
    let d = real_dispatcher();
    let list = handle_request(&d, &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await
        .unwrap();
    assert!(!list["result"]["tools"].as_array().unwrap().is_empty());
    let r = call(&d, "vibemk_debug_checkmk_connection", json!({})).await;
    assert_eq!(r["isError"], false);
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains('✅') || text.contains("Connection successful"));
}

#[tokio::test]
async fn host_operations_workflow() {
    if !enabled() {
        return;
    }
    let r = call(&real_dispatcher(), "vibemk_get_checkmk_hosts", json!({})).await;
    assert!(r["content"].is_array());
}

#[tokio::test]
async fn service_discovery_workflow() {
    if !enabled() {
        return;
    }
    // The tool is vibemk_get_checkmk_services (rust/tools.json); an unknown name
    // returns no `content`, which is how this test used to fail on 2.3 and 2.5.
    let r = call(&real_dispatcher(), "vibemk_get_checkmk_services", json!({})).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["content"].is_array());
}

#[tokio::test]
async fn error_handling_with_invalid_host() {
    if !enabled() {
        return;
    }
    let r = call(
        &real_dispatcher(),
        "vibemk_get_host_status",
        json!({"host_name": "definitely-not-a-host-wfx3y8zj"}),
    )
    .await;
    assert_eq!(r["isError"], true);
}

#[tokio::test]
async fn authentication_validation() {
    if !enabled() {
        return;
    }
    let mut config = Config::from_env().unwrap();
    config.password = "wrong-password-wfx3y8zj".into();
    let r = call(
        &dispatcher_for(config),
        "vibemk_get_checkmk_hosts",
        json!({}),
    )
    .await;
    assert_eq!(r["isError"], true);
}

#[tokio::test]
async fn concurrent_requests_load() {
    if !enabled() {
        return;
    }
    let d = Arc::new(real_dispatcher());
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let d = d.clone();
            tokio::spawn(async move { call(&d, "vibemk_get_checkmk_version", json!({})).await })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap()["isError"], false);
    }
}

#[tokio::test]
async fn rapid_sequential_requests() {
    if !enabled() {
        return;
    }
    let d = real_dispatcher();
    for _ in 0..10 {
        assert_eq!(
            call(&d, "vibemk_get_checkmk_version", json!({})).await["isError"],
            false
        );
    }
}
