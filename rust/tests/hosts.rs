//! Port of tests/test_handlers_hosts.py (11 tests) + a tools/call per read tool + the write paths.
mod common;

use std::sync::Arc;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::client::CheckMkClient;
use vibemk::dispatch::{is_error, structured_of, Dispatcher};
use vibemk::handlers::base::Handler;
use vibemk::handlers::hosts::HostHandler;
use vibemk::mcp::handle_request;
use vibemk::registry::ToolRegistry;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn on(server: &MockServer, verb: &str, p: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(format!("{API}/{p}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

fn text(r: &[Value]) -> &str {
    r[0]["text"].as_str().unwrap()
}

async fn handler() -> (MockServer, Arc<CheckMkClient>, HostHandler) {
    let (server, client) = fake_checkmk().await;
    let h = HostHandler::new(client.clone());
    (server, client, h)
}

/// A tools/call through the real registry and dispatcher.
async fn call(client: Arc<CheckMkClient>, tool: &str, args: Value) -> Value {
    let d = Dispatcher::new(Arc::new(ToolRegistry::from_client(client)));
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    handle_request(&d, &request).await.unwrap()["result"].clone()
}

fn host_status(state: i64, output: &str) -> Value {
    json!({"id": "test-server-01", "extensions": {"name": "test-server-01", "state": state, "hard_state": state, "state_type": 1, "plugin_output": output, "last_check": 1640995200, "has_been_checked": true}})
}

#[tokio::test]
async fn get_checkmk_hosts_success() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "domain-types/host/collections/all",
        200,
        json!({"value": [{"id": "test-server-01", "extensions": {"name": "test-server-01"}}]}),
    )
    .await;
    let r = h.handle("vibemk_get_checkmk_hosts", &json!({})).await;
    assert_eq!(r[0]["type"], "text");
    assert!(text(&r).contains("test-server-01"));
    assert!(text(&r).contains("🖥️"));
    // The fixture carries no state column, which is exactly what UNKNOWN means.
    assert_eq!(
        structured_of(&r),
        Some(json!({"total": 1, "hosts": [{"host_name": "test-server-01", "state": "UNKNOWN"}]}))
    );
}

#[tokio::test]
async fn get_checkmk_hosts_with_filter() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "domain-types/host/collections/all",
        200,
        json!({"value": [{"id": "test-server-01", "extensions": {}}]}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_get_checkmk_hosts",
            &json!({"host_name_filter": "test-server"}),
        )
        .await;
    assert!(text(&r).contains("test-server-01"));
    // The Monitoring collection (host_config is Setup and empty for non-admins), 'state' asked for explicitly.
    let url = s.received_requests().await.unwrap()[0].url.to_string();
    assert!(
        url.contains("domain-types/host/collections/all") && !url.contains("host_config"),
        "{url}"
    );
    assert!(url.contains("name") && url.contains("state"), "{url}");
}

#[tokio::test]
async fn get_host_status_success() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "objects/host/test-server-01",
        200,
        host_status(0, "OK"),
    )
    .await;
    let r = h
        .handle(
            "vibemk_get_host_status",
            &json!({"host_name": "test-server-01"}),
        )
        .await;
    assert!(
        text(&r).contains("🟢 **UP**")
            && text(&r).contains("test-server-01")
            && text(&r).contains("Hard State: 0")
    );
    let d = structured_of(&r).unwrap();
    assert_eq!(
        (
            &d["host_name"],
            &d["state"],
            &d["state_code"],
            &d["is_hard_state"]
        ),
        (
            &json!("test-server-01"),
            &json!("UP"),
            &json!(0),
            &json!(true)
        )
    );
}

#[tokio::test]
async fn get_host_status_down() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "objects/host/test-server-01",
        200,
        host_status(1, "CRITICAL - Host unreachable"),
    )
    .await;
    let r = h
        .handle(
            "vibemk_get_host_status",
            &json!({"host_name": "test-server-01"}),
        )
        .await;
    assert!(text(&r).contains("🔴 **DOWN**") && text(&r).contains("Hard State: 1"));
}

#[tokio::test]
async fn create_host_success() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "objects/host_config/new-test-server",
        404,
        json!({"title": "Not Found"}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{API}/domain-types/host_config/collections/all"
        )))
        .and(body_partial_json(
            json!({"host_name": "new-test-server", "folder": "~servers"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "new-test-server"})))
        .expect(1)
        .mount(&s)
        .await;
    let args = json!({"host_name": "new-test-server", "folder": "/servers", "attributes": {"ipaddress": "192.168.1.101", "alias": "New Test Server"}});
    let r = h.handle("vibemk_create_host", &args).await;
    assert_eq!(r.len(), 1);
    assert!(
        text(&r).contains("✅") && text(&r).contains("new-test-server"),
        "{}",
        text(&r)
    );
    assert!(text(&r).contains("• IP Address: 192.168.1.101\n• Alias: New Test Server\n"));
    assert!(text(&r).ends_with("Individual Creation API\n📡 **API Endpoint:** POST domain-types/host_config/collections/all"));
}

#[tokio::test]
async fn create_host_missing_parameters() {
    let (_s, _c, h) = handler().await;
    let r = h
        .handle("vibemk_create_host", &json!({"folder": "/test"}))
        .await;
    assert!(is_error(&r));
    assert!(text(&r).contains("❌") && text(&r).to_lowercase().contains("host_name"));
}

#[tokio::test]
async fn delete_host_success() {
    let (s, _c, h) = handler().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/objects/host_config/test-server-01")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&s)
        .await;
    let r = h
        .handle(
            "vibemk_delete_host",
            &json!({"host_name": "test-server-01"}),
        )
        .await;
    assert_eq!(r.len(), 1);
    assert!(text(&r).contains("✅") && text(&r).to_lowercase().contains("deleted"));
}

#[tokio::test]
async fn move_host_success() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "POST",
        "objects/host_config/test-server-01/actions/move/invoke",
        200,
        json!({}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_move_host",
            &json!({"host_name": "test-server-01", "target_folder": "/production/servers"}),
        )
        .await;
    assert_eq!(r.len(), 1);
    assert!(text(&r).contains("✅") && text(&r).to_lowercase().contains("moved"));
}

#[tokio::test]
async fn api_error_handling() {
    let (s, _c, h) = handler().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"title": "boom"})))
        .mount(&s)
        .await;
    let r = h
        .handle(
            "vibemk_get_host_status",
            &json!({"host_name": "test-server-01"}),
        )
        .await;
    assert!(is_error(&r));
    assert!(
        text(&r).contains("❌")
            && text(&r).contains("Host Status Retrieval Failed")
            && text(&r).contains("test-server-01")
    );
}

#[tokio::test]
async fn host_not_found() {
    let (s, _c, h) = handler().await;
    on(
        &s,
        "GET",
        "objects/host/nonexistent-host",
        404,
        json!({"title": "Not Found"}),
    )
    .await;
    on(
        &s,
        "GET",
        "domain-types/host/collections/all",
        200,
        json!({"value": []}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_get_host_status",
            &json!({"host_name": "nonexistent-host"}),
        )
        .await;
    assert!(is_error(&r));
    assert!(text(&r).contains("❌") && text(&r).to_lowercase().contains("not found"));
}

#[tokio::test]
async fn invalid_tool_name() {
    let (_s, _c, h) = handler().await;
    let r = h.handle("invalid_tool_name", &json!({})).await;
    assert!(is_error(&r));
    assert!(text(&r).contains("❌") && text(&r).contains("Unknown tool"));
    assert_eq!(
        r[0]["text"],
        "❌ **Unknown tool**\n\nTool 'invalid_tool_name' is not supported"
    );
}

// ---- one tools/call per read tool ----

#[tokio::test]
async fn tools_call_get_checkmk_hosts() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "domain-types/host/collections/all", 200, json!({"value": [{"id": "web1", "extensions": {"state": 1}}, {"id": "web2", "extensions": {"state": 7}}]})).await;
    let r = call(c, "vibemk_get_checkmk_hosts", json!({})).await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        r["content"][0]["text"],
        "🖥️ **CheckMK Hosts** (2 total, showing first 2):\n\n🖥️ web1 (DOWN)\n🖥️ web2 (UNKNOWN)"
    );
    assert_eq!(
        r["structuredContent"],
        json!({"total": 2, "hosts": [{"host_name": "web1", "state": "DOWN"}, {"host_name": "web2", "state": "UNKNOWN"}]})
    );
}

#[tokio::test]
async fn tools_call_get_host_status() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host/web1", 200, host_status(2, "x")).await;
    let r = call(c, "vibemk_get_host_status", json!({"host_name": "web1"})).await;
    assert_eq!(r["isError"], false);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("🟡 **UNREACHABLE**"));
    assert_eq!(r["structuredContent"]["state"], "UNREACHABLE");
}

const CONFIG: fn() -> Value = || json!({"id": "web1", "extensions": {"folder": "/linux/web", "attributes": {"ipaddress": "10.0.0.5", "alias": "Web 1"}}});

#[tokio::test]
async fn tools_call_get_host_details() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/web1", 200, CONFIG()).await;
    let r = call(c, "vibemk_get_host_details", json!({"host_name": "web1"})).await;
    assert_eq!(r["content"][0]["text"], "🔍 **Host Details: web1**\n\nFolder: /linux/web\nIP Address: 10.0.0.5\nAlias: Web 1\nAgent Type: Unknown\nSite: Not set");
}

#[tokio::test]
async fn tools_call_get_host_config() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/web1", 200, CONFIG()).await;
    let r = call(c, "vibemk_get_host_config", json!({"host_name": "web1"})).await;
    assert_eq!(
        r["content"][0]["text"],
        "ℹ️ **Host Configuration: web1**\n\n**id**: web1\n**extensions**: object"
    );
}

#[tokio::test]
async fn tools_call_compare_host_states() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/web1", 200, CONFIG()).await;
    let r = call(c, "vibemk_compare_host_states", json!({"host_name": "web1", "desired_attributes": {"ipaddress": "10.0.0.6", "site": "cmk"}})).await;
    assert_eq!(
        r["content"][0]["text"],
        "🔄 **Host State Comparison**\n\n**Host:** web1\n**Changes Required:** Yes\n\n📋 **Detected Changes:**\n**Added:**\n• site: cmk\n**Modified:**\n• ipaddress: 10.0.0.5 → 10.0.0.6\n**Removed:**\n• alias: Web 1\n"
    );
}

#[tokio::test]
async fn tools_call_get_host_effective_attributes() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/web1", 200, CONFIG()).await;
    on(
        &s,
        "GET",
        "objects/folder_config/~linux~web",
        200,
        json!({"extensions": {"attributes": {"tag_agent": "cmk-agent", "alias": "Folder"}}}),
    )
    .await;
    let r = call(
        c,
        "vibemk_get_host_effective_attributes",
        json!({"host_name": "web1"}),
    )
    .await;
    assert_eq!(
        r["content"][0]["text"],
        "📋 **Effective Host Attributes**\n\n**Host:** web1\n**Folder:** /linux/web\n\n🎯 **Effective Attributes:**\n• **tag_agent:** cmk-agent _Inherited_\n• **alias:** Web 1 _Host_\n• **ipaddress:** 10.0.0.5 _Host_\n"
    );
}

#[tokio::test]
async fn tools_call_validate_host_config() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/folder_config/~", 200, json!({})).await;
    let r = call(c, "vibemk_validate_host_config", json!({"host_name": "bad name!", "operation": "update", "attributes": {"ipaddress": "nope"}})).await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        r["content"][0]["text"],
        "🔍 **Host Configuration Validation**\n\n**Host:** bad name!\n**Operation:** update\n**Status:** ❌ Invalid\n\n🚨 **Errors:**\n• Invalid host name format\n• Invalid IP address format\n\n"
    );
}

// ---- write paths ----

#[tokio::test]
async fn update_host_sends_the_etag_it_read() {
    let (s, c, _h) = handler().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/objects/host_config/web1")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"abc\"")
                .set_body_json(CONFIG()),
        )
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/objects/host_config/web1")))
        .and(header("If-Match", "\"abc\""))
        .and(body_partial_json(
            json!({"update_attributes": {"alias": "New"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(CONFIG()))
        .expect(1)
        .mount(&s)
        .await;
    let r = call(
        c,
        "vibemk_update_host",
        json!({"host_name": "web1", "attributes": {"alias": "New"}}),
    )
    .await;
    assert_eq!(r["isError"], false);
    assert_eq!(
        r["content"][0]["text"],
        "✅ **Host Updated Successfully**\n\n**Host:** web1\n**Update Mode:** update\n**Operation:** Merging with existing host attributes\n\n📋 **Changes Applied:**\n**Modified:**\n• alias: Web 1 → New\n\n\n⚠️ **Remember to activate changes!**\n💡 Use 'vibemk_activate_changes' to apply the configuration"
    );
}

#[tokio::test]
async fn update_host_412_gives_the_concurrency_advice() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/web1", 200, CONFIG()).await;
    on(
        &s,
        "PUT",
        "objects/host_config/web1",
        412,
        json!({"title": "Precondition failed"}),
    )
    .await;
    let r = call(
        c,
        "vibemk_update_host",
        json!({"host_name": "web1", "attributes": {"alias": "New"}}),
    )
    .await;
    assert_eq!(r["isError"], true);
    assert_eq!(r["content"][0]["text"], "❌ **Concurrent modification detected**\n\nHost 'web1' was modified by another process. Please re-read it and retry the operation.");
}

#[tokio::test]
async fn bulk_create_maps_root_folder_and_lists_hosts() {
    let (s, _c, h) = handler().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{API}/domain-types/host_config/actions/bulk-create/invoke"
        )))
        .and(body_partial_json(
            json!({"entries": [{"host_name": "a", "folder": "~"}]}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"id": "a", "extensions": {"folder": "/"}}]})),
        )
        .expect(1)
        .mount(&s)
        .await;
    let r = h
        .handle(
            "vibemk_create_host",
            &json!({"hosts": [{"host_name": "a"}]}),
        )
        .await;
    assert!(text(&r).starts_with("✅ **Bulk Host Creation Successful**\n\n**Hosts Created:** 1/1\n\n📋 **Created Hosts:**\n• a (Folder: /)\n"), "{}", text(&r));
    assert!(text(&r).contains("Multiple hosts detected → Bulk Creation API"));
}

#[tokio::test]
async fn clone_host_escapes_like_python_json_dumps() {
    let (s, c, _h) = handler().await;
    on(&s, "GET", "objects/host_config/src", 200, json!({"extensions": {"folder": "/a/b", "attributes": {"alias": "é", "meta_data": {"x": 1}}}})).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "{API}/domain-types/host_config/collections/all"
        )))
        .and(body_partial_json(
            json!({"host_name": "dst", "folder": "~a~b", "attributes": {"ipaddress": "1.2.3.4"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let r = call(
        c,
        "vibemk_clone_host",
        json!({"source_hostname": "src", "target_hostname": "dst", "ip_address": "1.2.3.4"}),
    )
    .await;
    assert_eq!(r["content"][0]["text"], "✅ Host 'dst' created as a clone of 'src'.\nFolder: /a/b\nAttributes: {\n  \"alias\": \"\\u00e9\",\n  \"ipaddress\": \"1.2.3.4\"\n}");
}

#[tokio::test]
async fn folder_rules_are_enforced_on_create() {
    let (s, _c, h) = handler().await;
    on(&s, "GET", "objects/host_config/h1", 404, json!({})).await;
    let r = h
        .handle(
            "vibemk_create_host",
            &json!({"host_name": "h1", "folder": "/Main/b c"}),
        )
        .await;
    assert!(is_error(&r));
    // create_host_smart appends its routing note to errors too, as in Python.
    assert!(text(&r).starts_with(
        "❌ **Invalid folder path**\n\n❌ **Folder Path Validation Errors:**\n• 'main' is a reserved folder name\n• Invalid folder name 'b c'. Use only letters, numbers, underscore, and hyphen.\n\n🤖 **Smart Routing Applied:**"
    ));
}
