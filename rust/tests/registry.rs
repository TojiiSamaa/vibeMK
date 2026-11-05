//! Port of tests/test_tool_registry.py: catalogue (rust/tools.json) <-> registry <-> handlers.
mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use vibemk::dispatch::read_only_tools;
use vibemk::registry::ToolRegistry;
use vibemk::tools::{tool_names, tools};

/// annotations.py READ_ONLY, copied by scratchpad/gen_ro.py (64 tools).
const PYTHON_READ_ONLY: [&str; 64] = [
    "vibemk_baking_status",
    "vibemk_check_host_downtime_status",
    "vibemk_checkmk_rules_guide",
    "vibemk_compare_host_states",
    "vibemk_debug_api_endpoints",
    "vibemk_debug_checkmk_connection",
    "vibemk_debug_permissions",
    "vibemk_debug_url_detection",
    "vibemk_download_agent_by_host",
    "vibemk_find_host_grouping_rulesets",
    "vibemk_get_active_downtimes",
    "vibemk_get_audit_log",
    "vibemk_get_aux_tags",
    "vibemk_get_bulk_discovery_status",
    "vibemk_get_checkmk_hosts",
    "vibemk_get_checkmk_services",
    "vibemk_get_checkmk_version",
    "vibemk_get_comments",
    "vibemk_get_contact_groups",
    "vibemk_get_current_problems",
    "vibemk_get_custom_graph",
    "vibemk_get_discovery_background_job",
    "vibemk_get_discovery_status",
    "vibemk_get_downtimes",
    "vibemk_get_events",
    "vibemk_get_example_rule_structures",
    "vibemk_get_folder_hosts",
    "vibemk_get_folders",
    "vibemk_get_host_config",
    "vibemk_get_host_details",
    "vibemk_get_host_effective_attributes",
    "vibemk_get_host_groups",
    "vibemk_get_host_metrics",
    "vibemk_get_host_status",
    "vibemk_get_host_tags",
    "vibemk_get_notification_rule",
    "vibemk_get_notification_rules",
    "vibemk_get_passwords",
    "vibemk_get_pending_changes",
    "vibemk_get_ruleset",
    "vibemk_get_rulesets",
    "vibemk_get_service_group",
    "vibemk_get_service_groups",
    "vibemk_get_service_metrics",
    "vibemk_get_service_status",
    "vibemk_get_sites",
    "vibemk_get_timeperiods",
    "vibemk_get_users",
    "vibemk_list_acknowledgements",
    "vibemk_list_active_checks",
    "vibemk_list_available_metrics",
    "vibemk_list_downtimes",
    "vibemk_list_process_rules",
    "vibemk_list_rulesets",
    "vibemk_list_service_groups",
    "vibemk_list_user_roles",
    "vibemk_search_metrics",
    "vibemk_search_rulesets",
    "vibemk_show_ruleset",
    "vibemk_show_user_role",
    "vibemk_test_all_endpoints",
    "vibemk_test_direct_url",
    "vibemk_validate_host_config",
    "vibemk_wait_for_discovery",
];

async fn registry() -> ToolRegistry {
    let (_server, client) = common::fake_checkmk().await;
    ToolRegistry::from_client(client)
}

fn handler_sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("handlers");
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .expect("src/handlers")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

fn annotation(tool: &Value, key: &str) -> Option<bool> {
    tool.pointer(&format!("/annotations/{key}"))
        .and_then(Value::as_bool)
}

#[tokio::test]
async fn registry_exposes_every_wired_name() {
    let names = registry().await.tool_names();
    let listed = tool_names();
    assert_eq!(names.len(), 154);
    assert_eq!(
        names.into_iter().collect::<BTreeSet<_>>(),
        listed.into_iter().collect::<BTreeSet<_>>()
    );
}

#[tokio::test]
async fn registry_returns_none_for_an_unknown_tool() {
    assert!(registry()
        .await
        .handler_for("vibemk_does_not_exist")
        .is_none());
}

#[tokio::test]
async fn registry_returns_a_handler_that_answers() {
    let r = registry().await;
    let handler = r.handler_for("vibemk_get_checkmk_version").expect("routed");
    // The fake server has no mock: whatever the answer, it is content blocks, not a panic.
    let blocks = handler
        .handle("vibemk_get_checkmk_version", &serde_json::json!({}))
        .await;
    assert!(!blocks.is_empty());
}

#[test]
fn no_tool_is_declared_twice() {
    let names = tool_names();
    let unique: BTreeSet<_> = names.iter().collect();
    assert_eq!(names.len(), unique.len());
}

#[tokio::test]
async fn every_declared_tool_has_a_handler() {
    let r = registry().await;
    let missing: Vec<_> = tool_names()
        .into_iter()
        .filter(|n| r.handler_for(n).is_none())
        .collect();
    assert!(missing.is_empty(), "declared but not routed: {missing:?}");
}

#[tokio::test]
async fn every_handler_is_declared_as_a_tool() {
    let declared: BTreeSet<_> = tool_names().into_iter().collect();
    let orphans: Vec<_> = registry()
        .await
        .tool_names()
        .into_iter()
        .filter(|n| !declared.contains(n))
        .collect();
    assert!(orphans.is_empty(), "routed but not declared: {orphans:?}");
}

#[test]
fn every_vibemk_literal_in_a_handler_is_a_declared_tool() {
    let declared: BTreeSet<_> = tool_names().into_iter().collect();
    let mut orphans = Vec::new();
    for (file, src) in handler_sources() {
        for part in src.split('"').skip(1).step_by(2) {
            if part.starts_with("vibemk_") && !declared.contains(part) {
                orphans.push(format!("{file}: {part}"));
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "referenced in a handler but never declared: {orphans:?}"
    );
}

#[test]
fn every_tool_has_a_usable_schema() {
    for t in tools() {
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{}", t["name"]);
        assert!(
            jsonschema::draft202012::new(schema).is_ok(),
            "{} has an invalid schema",
            t["name"]
        );
    }
}

#[test]
fn every_tool_name_keeps_the_vibemk_prefix() {
    let bad: Vec<_> = tool_names()
        .into_iter()
        .filter(|n| !n.starts_with("vibemk_"))
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn every_tool_carries_a_title_and_annotations() {
    for t in tools() {
        assert!(
            t["title"].as_str().is_some_and(|s| !s.is_empty()),
            "{} has no title",
            t["name"]
        );
        assert!(
            t["annotations"].as_object().is_some_and(|a| !a.is_empty()),
            "{} has no annotations",
            t["name"]
        );
        assert!(
            t["description"].as_str().is_some_and(|s| !s.is_empty()),
            "{} has no description",
            t["name"]
        );
    }
}

#[test]
fn each_tool_is_classified_exactly_once() {
    for t in tools() {
        let read_only = annotation(t, "readOnlyHint")
            .unwrap_or_else(|| panic!("{} has no readOnlyHint", t["name"]));
        assert_eq!(annotation(t, "openWorldHint"), Some(true), "{}", t["name"]);
        if !read_only {
            assert!(
                annotation(t, "destructiveHint").is_some(),
                "{} is a write with no destructiveHint",
                t["name"]
            );
            assert!(
                annotation(t, "idempotentHint").is_some(),
                "{} is a write with no idempotentHint",
                t["name"]
            );
        }
    }
}

#[test]
fn a_read_only_tool_never_claims_to_be_destructive() {
    for t in tools()
        .iter()
        .filter(|t| annotation(t, "readOnlyHint") == Some(true))
    {
        assert_ne!(
            annotation(t, "destructiveHint"),
            Some(true),
            "{}",
            t["name"]
        );
    }
}

#[test]
fn the_read_only_set_is_the_python_one() {
    let rust: BTreeSet<String> = read_only_tools().into_iter().collect();
    let python: BTreeSet<String> = PYTHON_READ_ONLY.iter().map(|s| s.to_string()).collect();
    assert_eq!(rust.len(), 64);
    assert_eq!(rust, python);
}

#[test]
fn no_write_tool_is_in_the_read_only_set() {
    let ro = read_only_tools();
    for t in tools()
        .iter()
        .filter(|t| annotation(t, "readOnlyHint") == Some(false))
    {
        assert!(!ro.contains(t["name"].as_str().unwrap()), "{}", t["name"]);
    }
    // Names that delete or create can only be writes.
    for name in &ro {
        assert!(
            !name.starts_with("vibemk_delete_") && !name.starts_with("vibemk_create_"),
            "{name}"
        );
    }
}

#[test]
fn every_output_schema_belongs_to_a_read_only_tool_and_is_valid() {
    let with_schema: Vec<_> = tools()
        .iter()
        .filter(|t| t.get("outputSchema").is_some())
        .collect();
    assert_eq!(with_schema.len(), 11);
    for t in with_schema {
        assert_eq!(
            annotation(t, "readOnlyHint"),
            Some(true),
            "{} has an output schema but writes",
            t["name"]
        );
        assert!(
            jsonschema::draft202012::new(&t["outputSchema"]).is_ok(),
            "{}",
            t["name"]
        );
    }
}

#[test]
fn every_tool_closes_its_top_level_arguments() {
    let open: Vec<_> = tools()
        .iter()
        .filter(|t| t["inputSchema"].get("additionalProperties") != Some(&Value::Bool(false)))
        .map(|t| t["name"].clone())
        .collect();
    assert!(open.is_empty(), "{open:?}");
}

#[test]
fn no_handler_builds_an_error_text_block_by_hand() {
    let mut offenders = Vec::new();
    for (file, src) in handler_sources()
        .into_iter()
        .filter(|(f, _)| f != "base.rs")
    {
        for (i, line) in src.lines().enumerate() {
            if line.contains("\"text\"") && line.contains('❌') {
                offenders.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use error_text / error_response instead: {offenders:?}"
    );
}

#[test]
fn the_handlers_leave_activation_to_the_dispatcher() {
    let copies: Vec<_> = handler_sources()
        .into_iter()
        .filter(|(f, src)| {
            f != "base.rs"
                && src.lines().any(|l| {
                    !l.trim_start().starts_with("//") && l.contains("\"activate_changes\"")
                })
        })
        .map(|(f, _)| f)
        .collect();
    assert!(copies.is_empty(), "one place decides: {copies:?}");
}

#[tokio::test]
async fn every_tool_offering_activation_is_routed() {
    let r = registry().await;
    let offering: Vec<_> = tools()
        .iter()
        .filter(|t| {
            t.pointer("/inputSchema/properties/activate_changes")
                .is_some()
        })
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(offering.len() > 30, "{}", offering.len());
    assert!(offering.iter().all(|n| r.handler_for(n).is_some()));
}
