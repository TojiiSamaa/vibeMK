//! tools/call of the service-parameter tools against a fake CheckMK
//! (replays tests/test_handlers_service_params.py, then covers the writers).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::service_params::ServiceParamsHandler;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

fn api(p: &str) -> String {
    format!("{}/{p}", common::API)
}

fn rule(value_raw: &str) -> Value {
    json!({"id": "rule-1", "extensions": {"conditions": {"host_name": {"match_on": ["web01"]}}, "value_raw": value_raw, "properties": {}}})
}

async fn list_with(rules: Vec<Value>) -> String {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(api("domain-types/rule/collections/all")))
        .and(query_param("ruleset_name", "inventory_processes_rules"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": rules})))
        .mount(&server)
        .await;
    text(
        &ServiceParamsHandler::new(client)
            .handle("vibemk_list_process_rules", &json!({}))
            .await,
    )
}

#[tokio::test]
async fn a_value_that_is_code_is_not_run() {
    let marker = std::env::temp_dir().join("vibemk-service-params-ran");
    let _ = std::fs::remove_file(&marker);
    let out = list_with(vec![rule(&format!(
        "open({:?}, 'w')",
        marker.to_string_lossy()
    ))])
    .await;
    assert!(!marker.exists());
    // Shown raw instead, as any value that does not parse is
    assert!(out.contains("open("), "{out}");
}

#[tokio::test]
async fn a_process_rule_is_rendered() {
    let out = list_with(vec![rule("{'descr': 'nginx', 'match': '~nginx', 'default_params': {'levels': (1, 1, 10, 12), 'cpulevels': (80.0, 90.0)}}")]).await;
    assert!(out.contains("**Process nginx**"), "{out}");
    assert!(out.contains("`~nginx`"), "{out}");
    assert!(
        out.contains("min warn/crit: 1/1, max warn/crit: 10/12"),
        "{out}"
    );
    assert!(out.contains("CPU warn/crit: 80.0%/90.0%"), "{out}");
}

#[tokio::test]
async fn the_listing_filters_by_host_and_process_and_says_when_empty() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"value": [rule("{'descr': 'nginx', 'match': 'nginx', 'default_params': {}}")]}),
        ))
        .mount(&server)
        .await;
    let h = ServiceParamsHandler::new(client);
    let out = text(
        &h.handle(
            "vibemk_list_process_rules",
            &json!({"hostname": "web01", "process_name": "NGINX"}),
        )
        .await,
    );
    assert!(
        out.contains("(no thresholds)") && out.contains("→ web01"),
        "{out}"
    );
    let out = text(
        &h.handle("vibemk_list_process_rules", &json!({"hostname": "other"}))
            .await,
    );
    assert_eq!(out, "🔍 **Process monitoring rules (inventory_processes_rules)**\n\nNo process monitoring rules found.");
}

#[tokio::test]
async fn set_process_thresholds_posts_a_python_literal_and_discovers() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(api("objects/host_config/web01")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"extensions": {"folder": "/linux/web"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(api("domain-types/rule/collections/all")))
        .and(body_json(json!({
            "ruleset": "inventory_processes_rules",
            "folder": "~linux~web",
            "value_raw": "{'descr': 'nginx', 'match': 'nginx', 'default_params': {'cpu_rescale_max': True, 'levels': (1, 1, 10, 12), 'cpulevels': (80.0, 90.0), 'virtual_levels': (1048576, 2097152)}}",
            "conditions": {"host_name": {"match_on": ["web01"], "operator": "one_of"}, "host_tags": [], "host_label_groups": [], "service_label_groups": []},
            "properties": {"disabled": false, "description": "Process nginx: warn>=10, crit>=12"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r-9"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(api(
            "domain-types/activation_run/collections/pending_changes",
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(api(
            "objects/host/web01/actions/discover_services/invoke",
        )))
        .and(body_json(json!({"mode": "refresh"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let args = json!({"hostname": "web01", "process_name": "Process nginx", "warn_max": 10, "crit_max": "12", "cpu_warn_percent": 80, "cpu_crit_percent": 90, "mem_warn_mb": 1, "mem_crit_mb": 2});
    let out = ServiceParamsHandler::new(client)
        .handle("vibemk_set_process_thresholds", &args)
        .await;
    assert_eq!(
        text(&out),
        "✅ Process thresholds for 'Process nginx' on 'web01' created.\nMinimum: warn<1, crit<1\nMaximum: warn>=10, crit>=12\nCPU total: warn 80%, crit 90%\nVirtual memory: warn 1 MB, crit 2 MB\nRule-ID: r-9\nℹ️ No pending changes to activate.\n✅ Service discovery run for 'web01' — the new thresholds are active."
    );
}

#[tokio::test]
async fn set_process_thresholds_checks_its_arguments() {
    let (_server, client) = common::fake_checkmk().await;
    let h = ServiceParamsHandler::new(client);
    let out = h
        .handle(
            "vibemk_set_process_thresholds",
            &json!({"hostname": "web01", "process_name": "x", "folder": "~"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nwarn_max and crit_max are required"
    );
    assert_eq!(out[1]["type"], "_error");
    let out = h
        .handle("vibemk_set_process_thresholds", &json!({"folder": "~"}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nhostname and process_name are required"
    );
}

#[tokio::test]
async fn set_interface_params_posts_the_speed_in_bits() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(api("domain-types/rule/collections/all")))
        .and(body_json(json!({
            "ruleset": "checkgroup_parameters:interfaces",
            "folder": "~",
            "value_raw": "{'speed': 1000000000}",
            "conditions": {"host_name": {"match_on": ["sw1"], "operator": "one_of"}, "service_description": {"match_on": ["Interface vmbr1"], "operator": "one_of"}, "host_tags": [], "host_label_groups": [], "service_label_groups": []},
            "properties": {"disabled": false, "description": "Interface vmbr1 on sw1: expected speed 1000 Mbit/s"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r-2"})))
        .expect(1)
        .mount(&server)
        .await;
    let out = ServiceParamsHandler::new(client).handle("vibemk_set_interface_params", &json!({"hostname": "sw1", "interface_name": "vmbr1", "expected_speed_mbit": 1000, "folder": "~"})).await;
    assert_eq!(
        text(&out),
        "✅ Interface parameters set for 'Interface vmbr1' on 'sw1'.\nExpected speed: 1000 Mbit/s (1,000,000,000 bits/s)\nRule-ID: r-2\nℹ️  Activate changes for the rule to take effect."
    );
}

#[tokio::test]
async fn set_memory_thresholds_posts_both_levels() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(api("domain-types/rule/collections/all")))
        .and(body_json(json!({
            "ruleset": "checkgroup_parameters:memory_linux",
            "folder": "~",
            "value_raw": "{'levels_virtual': ('perc_used', (80.0, 90.0)), 'levels_swap': ('perc_used', (50.0, 60.0))}",
            "conditions": {"host_name": {"match_on": ["web01"], "operator": "one_of"}, "host_tags": [], "host_label_groups": [], "service_label_groups": []},
            "properties": {"disabled": false, "description": "Memory web01: RAM warn=80% crit=90%, Swap warn=50% crit=60%"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r-3"})))
        .expect(1)
        .mount(&server)
        .await;
    let args = json!({"hostname": "web01", "folder": "~", "ram_warn_percent": 80, "ram_crit_percent": 90, "swap_warn_percent": 50, "swap_crit_percent": 60});
    let out = ServiceParamsHandler::new(client)
        .handle("vibemk_set_memory_thresholds", &args)
        .await;
    assert_eq!(
        text(&out),
        "✅ Memory thresholds set for 'web01'.\nRAM: warn>=80%, crit>=90%\nSwap: warn>=50%, crit>=60%\nRule-ID: r-3\nℹ️  The rule is at the top, so it takes precedence over older rules."
    );
    let (_s, client) = common::fake_checkmk().await;
    let out = ServiceParamsHandler::new(client)
        .handle(
            "vibemk_set_memory_thresholds",
            &json!({"hostname": "web01", "folder": "~"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nat least ram_warn_percent or swap_warn_percent required"
    );
}

#[tokio::test]
async fn delete_rule_calls_delete_and_refuses_a_navigating_id() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("DELETE"))
        .and(path(api("objects/rule/rule-1")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let h = ServiceParamsHandler::new(client);
    let out = h
        .handle(
            "vibemk_delete_service_param_rule",
            &json!({"rule_id": "rule-1"}),
        )
        .await;
    assert_eq!(text(&out), "✅ Rule `rule-1` deleted.");
    let out = h
        .handle(
            "vibemk_delete_service_param_rule",
            &json!({"rule_id": ".."}),
        )
        .await;
    assert_eq!(out[1]["type"], "_error");
    let out = h
        .handle("vibemk_delete_service_param_rule", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Missing parameter**\n\nrule_id is required"
    );
}

#[tokio::test]
async fn an_unknown_tool_is_refused() {
    let (_server, client) = common::fake_checkmk().await;
    let out = ServiceParamsHandler::new(client)
        .handle("nope", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "❌ **Unknown tool**\n\nTool 'nope' is not supported"
    );
}
