//! Port of test_read_only_mode.py (dispatcher part), test_argument_validation.py,
//! test_activate_changes_param.py, plus the result shaping of dispatch.py.
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use vibemk::client::CheckMkClient;
use vibemk::dispatch::{is_error, read_only_tools, structured_of, to_content_blocks, Dispatcher};
use vibemk::handlers::base::Handler;
use vibemk::registry::ToolRegistry;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

struct Recording {
    calls: Mutex<Vec<Value>>,
    reply: Vec<Value>,
}

impl Recording {
    fn new(reply: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            reply,
        })
    }
    fn done() -> Arc<Self> {
        Self::new(vec![json!({"type": "text", "text": "✅ **Done**"})])
    }
    fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Handler for Recording {
    async fn handle(&self, _tool: &str, args: &Value) -> Vec<Value> {
        self.calls.lock().unwrap().push(args.clone());
        self.reply.clone()
    }
}

struct Panicking;

#[async_trait]
impl Handler for Panicking {
    async fn handle(&self, _tool: &str, _args: &Value) -> Vec<Value> {
        panic!("boom");
    }
}

fn registry(entries: Vec<(&'static str, Arc<dyn Handler>)>) -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new(entries.into_iter().collect()))
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------- read-only mode ----------

#[tokio::test]
async fn a_write_tool_is_refused_without_running() {
    let h = Recording::done();
    let d = Dispatcher::new(registry(vec![("vibemk_delete_host", h.clone())])).read_only();
    let r = d
        .call_tool("vibemk_delete_host", Some(&json!({"host_name": "web01"})))
        .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("read-only"));
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn a_read_tool_still_runs() {
    let h = Recording::done();
    let d = Dispatcher::new(registry(vec![("vibemk_get_checkmk_hosts", h.clone())])).read_only();
    let r = d
        .call_tool("vibemk_get_checkmk_hosts", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn every_write_tool_of_the_catalogue_is_refused_in_read_only_mode() {
    let d = Dispatcher::new(registry(vec![])).read_only();
    let allowed = read_only_tools();
    let mut refused = 0;
    for name in vibemk::tools::tool_names()
        .into_iter()
        .filter(|n| !allowed.contains(*n))
    {
        let r = d.call_tool(name, Some(&json!({}))).await;
        assert_eq!(r["isError"], true, "{name}");
        assert!(text(&r).contains("read-only"), "{name}: {}", text(&r));
        refused += 1;
    }
    assert_eq!(refused, 154 - 64);
}

#[tokio::test]
async fn without_the_switch_a_write_tool_runs() {
    let h = Recording::done();
    let d = Dispatcher::new(registry(vec![("vibemk_delete_host", h.clone())]));
    let r = d
        .call_tool("vibemk_delete_host", Some(&json!({"host_name": "web01"})))
        .await;
    assert_eq!(r["isError"], false);
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn a_hidden_tool_is_refused_before_the_registry() {
    // The registry is empty: reaching it would say "Unknown tool", not "read-only".
    let d = Dispatcher::new(registry(vec![])).read_only();
    let r = d
        .call_tool("vibemk_delete_host", Some(&json!({"host_name": "web01"})))
        .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("read-only"));
    assert!(!text(&r).contains("Unknown tool"));
}

#[tokio::test]
async fn the_read_only_message_names_the_tool_and_the_switch() {
    let d = Dispatcher::new(registry(vec![])).read_only();
    let r = d.call_tool("vibemk_delete_host", None).await;
    assert_eq!(
        text(&r),
        "❌ **vibemk_delete_host is not available**\n\nThis vibeMK server runs read-only: it only offers tools that read from CheckMK. Ask the operator to restart it without --read-only (VIBEMK_READ_ONLY) to make changes."
    );
}

// ---------- unknown tool / failures are results ----------

#[tokio::test]
async fn an_unknown_tool_is_a_result_not_a_protocol_error() {
    let r = Dispatcher::new(registry(vec![]))
        .call_tool("nope", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], true);
    assert_eq!(text(&r), "❌ **Unknown tool: nope**");
}

#[tokio::test]
async fn a_panicking_handler_becomes_an_error_result() {
    let d = Dispatcher::new(registry(vec![("t", Arc::new(Panicking))]));
    let r = d.call_tool("t", Some(&json!({}))).await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).starts_with("❌ **t failed**\n\n"));
}

// ---------- result shaping ----------

#[test]
fn only_the_error_block_marks_a_failure() {
    assert!(is_error(&[
        json!({"type": "text", "text": "x"}),
        json!({"type": "_error"})
    ]));
    // A leading cross is a perfectly good symbol for a CRITICAL service.
    assert!(!is_error(&[
        json!({"type": "text", "text": "❌ CRITICAL service"})
    ]));
    assert!(!is_error(&[]));
}

#[test]
fn content_blocks_drop_the_markers_and_the_data() {
    let blocks = to_content_blocks(&[
        json!({"type": "text", "text": "a"}),
        json!({"type": "_structured", "data": {"k": 1}}),
        json!({"type": "_error"}),
        json!({"type": "text"}),
    ]);
    assert_eq!(
        blocks,
        vec![
            json!({"type": "text", "text": "a"}),
            json!({"type": "text", "text": ""})
        ]
    );
}

#[test]
fn structured_of_returns_the_first_non_null_payload() {
    assert_eq!(
        structured_of(&[json!({"type": "_structured", "data": {"k": 1}})]),
        Some(json!({"k": 1}))
    );
    assert_eq!(structured_of(&[json!({"type": "text", "text": "a"})]), None);
    assert_eq!(
        structured_of(&[json!({"type": "_structured", "data": null})]),
        None
    );
    assert_eq!(
        structured_of(&[json!({"type": "_structured", "data": []})]),
        Some(json!([]))
    );
}

#[tokio::test]
async fn structured_content_is_lifted_out_of_the_blocks() {
    let h = Recording::new(vec![
        json!({"type": "text", "text": "ok"}),
        json!({"type": "_structured", "data": {"hosts": []}}),
    ]);
    let r = Dispatcher::new(registry(vec![("t", h)]))
        .call_tool("t", None)
        .await;
    assert_eq!(r["structuredContent"], json!({"hosts": []}));
    assert_eq!(r["content"], json!([{"type": "text", "text": "ok"}]));
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn a_handler_error_block_sets_is_error() {
    let h = Recording::new(vec![
        json!({"type": "text", "text": "❌ **no**"}),
        json!({"type": "_error"}),
    ]);
    let r = Dispatcher::new(registry(vec![("t", h)]))
        .call_tool("t", None)
        .await;
    assert_eq!(r["isError"], true);
    assert!(r.get("structuredContent").is_none());
}

// ---------- argument validation ----------

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "host_name": {"type": "string"},
            "mode": {"type": "string", "enum": ["new", "refresh"]},
            "timeout": {"type": "number", "minimum": 1}
        },
        "required": ["host_name"]
    })
}

fn validating() -> (Dispatcher, Arc<Recording>) {
    let h = Recording::done();
    let d = Dispatcher::new(registry(vec![("tool", h.clone())]))
        .with_input_schemas(HashMap::from([("tool".to_string(), schema())]));
    (d, h)
}

async fn refused(arguments: Value, expected: &str) {
    let (d, h) = validating();
    let r = d.call_tool("tool", Some(&arguments)).await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("Invalid arguments"), "{}", text(&r));
    assert!(
        text(&r).contains(expected),
        "{} not in {}",
        expected,
        text(&r)
    );
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn valid_arguments_reach_the_handler() {
    let (d, h) = validating();
    let args = json!({"host_name": "web01", "mode": "new", "timeout": 30});
    let r = d.call_tool("tool", Some(&args)).await;
    assert_eq!(r["isError"], false);
    assert_eq!(h.calls(), vec![args]);
}

#[tokio::test]
async fn a_missing_required_argument_is_named() {
    refused(json!({}), "'host_name' is a required property").await;
}

#[tokio::test]
async fn a_wrong_type_is_named() {
    refused(
        json!({"host_name": 42}),
        "host_name: 42 is not of type 'string'",
    )
    .await;
}

#[tokio::test]
async fn a_value_outside_the_enum_is_named() {
    refused(
        json!({"host_name": "web01", "mode": "tabula"}),
        "mode: 'tabula' is not one of ['new', 'refresh']",
    )
    .await;
}

#[tokio::test]
async fn a_string_for_a_number_is_named() {
    refused(
        json!({"host_name": "web01", "timeout": "30"}),
        "timeout: '30' is not of type 'number'",
    )
    .await;
}

#[tokio::test]
async fn a_value_below_the_minimum_is_named() {
    refused(
        json!({"host_name": "web01", "timeout": 0}),
        "timeout: 0 is less than the minimum of 1",
    )
    .await;
}

#[tokio::test]
async fn every_problem_is_reported_at_once() {
    let (d, _) = validating();
    let t = text(
        &d.call_tool("tool", Some(&json!({"mode": "tabula", "timeout": "30"})))
            .await,
    );
    assert!(t.starts_with("❌ **Invalid arguments for tool**\n\n- "));
    assert!(t.contains("- 'host_name' is a required property"));
    assert!(t.contains("- mode: "));
    assert!(t.contains("- timeout: "));
}

#[tokio::test]
async fn missing_arguments_count_as_an_empty_object() {
    let (d, _) = validating();
    for args in [None, Some(&Value::Null)] {
        let r = d.call_tool("tool", args).await;
        assert_eq!(r["isError"], true);
        assert!(text(&r).contains("'host_name' is a required property"));
    }
}

#[tokio::test]
async fn a_nullable_integer_reports_every_accepted_type() {
    let schema = json!({"type": "object", "properties": {"limit": {"type": ["integer", "null"]}}});
    let d = Dispatcher::new(registry(vec![("t", Recording::done())]))
        .with_input_schemas(HashMap::from([("t".to_string(), schema)]));
    let t = text(&d.call_tool("t", Some(&json!({"limit": "x"}))).await);
    assert!(
        t.contains("limit: 'x' is not of type 'integer', 'null'"),
        "{t}"
    );
}

#[tokio::test]
async fn the_server_validates_against_the_catalogue() {
    let d = Dispatcher::new(registry(vec![]));
    let r = d
        .call_tool("vibemk_get_host_status", Some(&json!({})))
        .await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("'host_name' is a required property"));
}

#[tokio::test]
async fn a_misspelt_argument_is_named() {
    let d = Dispatcher::new(registry(vec![]));
    let r = d
        .call_tool(
            "vibemk_get_host_status",
            Some(&json!({"host_name": "web01", "hostname": "web01"})),
        )
        .await;
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).contains("'hostname' was unexpected"),
        "{}",
        text(&r)
    );
}

#[test]
fn every_tool_has_a_schema_to_validate_against() {
    for t in vibemk::tools::tools() {
        assert!(
            jsonschema::draft202012::new(&t["inputSchema"]).is_ok(),
            "{}",
            t["name"]
        );
    }
}

// ---------- activate_changes ----------

const PENDING: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/collections/pending_changes";
const ACTIVATE: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/actions/activate-changes/invoke";

fn write_schemas() -> HashMap<String, Value> {
    let write = json!({"type": "object", "properties": {"name": {"type": "string"}, "activate_changes": {"type": "boolean"}}, "additionalProperties": false});
    let read = json!({"type": "object", "properties": {"name": {"type": "string"}}, "additionalProperties": false});
    HashMap::from([("write".to_string(), write), ("read".to_string(), read)])
}

fn activating(h: Arc<Recording>, client: Arc<CheckMkClient>) -> Dispatcher {
    Dispatcher::new(registry(vec![("write", h.clone()), ("read", h)]))
        .with_input_schemas(write_schemas())
        .with_activation_client(client)
}

#[tokio::test]
async fn a_successful_write_with_the_flag_is_activated() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"abc\"")
                .set_body_json(json!({"value": [{"id": "c1"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .and(header("If-Match", "\"abc\""))
        .and(body_json(
            json!({"redirect": false, "sites": ["cmk"], "force_foreign_changes": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let r = activating(
        Recording::new(vec![json!({"type": "text", "text": "✅ **Rule created**"})]),
        client,
    )
    .call_tool(
        "write",
        Some(&json!({"name": "x", "activate_changes": true})),
    )
    .await;
    assert_eq!(
        text(&r),
        "✅ **Rule created**\n✅ Activation started (CheckMK runs it in the background)."
    );
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn without_the_flag_nothing_is_activated() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(path(PENDING))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let h = Recording::done();
    for args in [
        json!({"name": "x"}),
        json!({"name": "x", "activate_changes": false}),
    ] {
        let r = activating(h.clone(), client.clone())
            .call_tool("write", Some(&args))
            .await;
        assert_eq!(text(&r), "✅ **Done**");
    }
}

#[tokio::test]
async fn a_failed_write_is_not_activated() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(path(PENDING))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let h = Recording::new(vec![
        json!({"type": "text", "text": "❌ **Creating the rule failed**"}),
        json!({"type": "_error"}),
    ]);
    let r = activating(h, client)
        .call_tool(
            "write",
            Some(&json!({"name": "x", "activate_changes": true})),
        )
        .await;
    assert_eq!(r["isError"], true);
    assert_eq!(text(&r), "❌ **Creating the rule failed**");
}

#[tokio::test]
async fn a_tool_that_does_not_offer_the_flag_never_activates() {
    // "read" has no activate_changes property: the flag would be refused as unexpected anyway.
    let (server, client) = common::fake_checkmk().await;
    Mock::given(path(PENDING))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let r = activating(Recording::done(), client)
        .call_tool(
            "read",
            Some(&json!({"name": "x", "activate_changes": true})),
        )
        .await;
    assert_eq!(r["isError"], true);
}

#[tokio::test]
async fn never_activate_changes_is_honoured() {
    let mut config = common::test_config("http://127.0.0.1:9");
    config.never_activate_changes = true;
    let client = Arc::new(CheckMkClient::with_api_base_url(
        config,
        "http://127.0.0.1:9/cmk/check_mk/api/1.0",
    ));
    let r = activating(Recording::done(), client)
        .call_tool(
            "write",
            Some(&json!({"name": "x", "activate_changes": true})),
        )
        .await;
    assert_eq!(
        text(&r),
        "✅ **Done**\n🚫 Changes were not activated: NEVER_ACTIVATE_CHANGES is set."
    );
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn a_failed_activation_does_not_hide_the_write() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(path(PENDING))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let r = activating(Recording::done(), client)
        .call_tool(
            "write",
            Some(&json!({"name": "x", "activate_changes": true})),
        )
        .await;
    assert!(
        text(&r).starts_with("✅ **Done**\n⚠️ Activation failed: "),
        "{}",
        text(&r)
    );
    assert_eq!(r["isError"], false);
}

#[tokio::test]
async fn no_pending_changes_is_reported_not_activated() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let r = activating(Recording::done(), client)
        .call_tool(
            "write",
            Some(&json!({"name": "x", "activate_changes": true})),
        )
        .await;
    assert_eq!(text(&r), "✅ **Done**\nℹ️ No pending changes to activate.");
}

#[tokio::test]
async fn the_line_is_appended_as_a_block_when_there_is_no_text() {
    let mut config = common::test_config("http://127.0.0.1:9");
    config.never_activate_changes = true;
    let client = Arc::new(CheckMkClient::with_api_base_url(
        config,
        "http://127.0.0.1:9/cmk/check_mk/api/1.0",
    ));
    let h = Recording::new(vec![json!({"type": "_structured", "data": {"a": 1}})]);
    let r = activating(h, client)
        .call_tool("write", Some(&json!({"activate_changes": true})))
        .await;
    assert_eq!(
        r["content"],
        json!([{"type": "text", "text": "🚫 Changes were not activated: NEVER_ACTIVATE_CHANGES is set."}])
    );
    assert_eq!(r["structuredContent"], json!({"a": 1}));
}

#[test]
fn every_catalogue_tool_offering_the_flag_is_activating() {
    let offering = vibemk::tools::tools()
        .iter()
        .filter(|t| {
            t.pointer("/inputSchema/properties/activate_changes")
                .is_some()
        })
        .count();
    assert!(offering > 30, "{offering}");
}
