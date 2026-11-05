//! Port of the applicable part of tests/test_mcp_server.py: protocol shape, catalogue, wiring, is_error.
//! Tool-call semantics (handler reached, args, concurrency) belong to dispatch.rs (C2).
mod common;

use std::sync::Arc;

use serde_json::{json, Value};
use vibemk::dispatch::{is_error, Dispatcher};
use vibemk::mcp::{handle_line, handle_request};
use vibemk::registry::ToolRegistry;

async fn dispatcher() -> (wiremock::MockServer, Dispatcher) {
    let (fake, client) = common::fake_checkmk().await;
    (
        fake,
        Dispatcher::new(Arc::new(ToolRegistry::from_client(client))),
    )
}

async fn ask(d: &Dispatcher, request: Value) -> Value {
    handle_request(d, &request)
        .await
        .expect("a request is answered")
}

#[tokio::test]
async fn the_server_carries_its_name_and_version() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).await;
    assert_eq!(
        r["result"]["serverInfo"],
        json!({"name":"vibemk","version":"0.6.4"})
    );
    assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert_eq!(r["id"], 1);
}

#[tokio::test]
async fn an_unknown_protocol_version_gets_one_the_server_speaks() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}})).await;
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn the_catalogue_is_available_without_contacting_checkmk() {
    let (fake, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 154);
    assert!(fake.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn tools_list_is_tools_json_unchanged() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
    let expected: Value = serde_json::from_str(vibemk::tools::TOOLS_JSON).unwrap();
    assert_eq!(r["result"]["tools"], expected);
}

#[tokio::test]
async fn every_declared_tool_is_well_formed() {
    for tool in vibemk::tools::tools() {
        assert!(tool["name"].is_string(), "{tool}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }
}

#[tokio::test]
async fn notifications_get_no_answer() {
    let (_f, d) = dispatcher().await;
    assert!(handle_line(
        &d,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
    )
    .await
    .is_none());
    assert!(handle_line(
        &d,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#
    )
    .await
    .is_none());
}

#[tokio::test]
async fn ping_answers_an_empty_result() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":"abc","method":"ping"})).await;
    assert_eq!(r, json!({"jsonrpc":"2.0","id":"abc","result":{}}));
}

#[tokio::test]
async fn an_unknown_method_is_minus_32601() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":3,"method":"nope"})).await;
    assert_eq!(r["error"]["code"], -32601);
}

#[tokio::test]
async fn invalid_json_is_minus_32700_with_a_null_id() {
    let (_f, d) = dispatcher().await;
    let r: Value = serde_json::from_str(&handle_line(&d, "{not json").await.unwrap()).unwrap();
    assert_eq!(r["error"]["code"], -32700);
    assert!(r["id"].is_null());
}

#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() {
    let (_f, d) = dispatcher().await;
    let r = ask(
        &d,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"vibemk_not_a_tool"}}),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602);
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown tool"));
}

#[tokio::test]
async fn a_known_tool_call_comes_back_as_a_call_tool_result() {
    let (_f, d) = dispatcher().await;
    let r = ask(&d, json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"vibemk_get_checkmk_version","arguments":{}}})).await;
    assert!(r["result"]["content"].is_array());
    assert!(r["result"]["isError"].is_boolean());
}

fn text(t: &str) -> Value {
    json!({"type":"text","text":t})
}

#[test]
fn the_error_block_marks_a_failure() {
    assert!(is_error(&[text("❌ **Failed**"), json!({"type":"_error"})]));
}

#[test]
fn a_cross_in_the_text_alone_is_not_a_failure() {
    assert!(!is_error(&[text("❌ **Check_MK** is CRITICAL")]));
}

#[test]
fn a_tick_and_empty_content_are_successes() {
    assert!(!is_error(&[text("✅ **Done**")]));
    assert!(!is_error(&[]));
}
