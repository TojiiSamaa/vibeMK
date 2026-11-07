//! tools/call of the 3 ruleset tools against a fake CheckMK (method, path, query checked).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::rulesets::RulesetsHandler;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ALL: &str = "/cmk/check_mk/api/1.0/domain-types/ruleset/collections/all";

fn text(b: &[Value]) -> String {
    b[0]["text"].as_str().unwrap().to_string()
}
fn is_error(b: &[Value]) -> bool {
    b.iter().any(|x| x["type"] == "_error")
}
async fn setup() -> (MockServer, RulesetsHandler) {
    let (server, client) = common::fake_checkmk().await;
    (server, RulesetsHandler::new(client))
}
fn rs(id: &str, title: &str) -> Value {
    json!({"id": id, "extensions": {"title": title, "number_of_rules": 2}})
}

#[tokio::test]
async fn search_sends_filters_and_lists_results() {
    let (server, h) = setup().await;
    let long = "x".repeat(120);
    let body = json!({"value": [
        {"id": "a_rs", "extensions": {"title": "A", "help": long, "deprecated": true, "number_of_rules": 3}},
        rs("b_rs", "B"),
    ]});
    Mock::given(method("GET"))
        .and(path(ALL))
        .and(query_param("fulltext", "cpu"))
        .and(query_param("folder", "/x"))
        .and(query_param("name", "n"))
        .and(query_param("deprecated", "false"))
        .and(query_param("used", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_search_rulesets",
            &json!({"fulltext": "cpu", "folder": "/x", "name": "n"}),
        )
        .await;
    let expected = format!("🔍 **Ruleset Search Results**\n\n**Search Criteria:**\n• fulltext: cpu\n• folder: /x\n• deprecated: false\n• used: true\n• name: n\n\n**Found 2 rulesets**\n\n**1. ⚠️ a_rs**\n   📋 Title: A\n   📊 Rules: 3\n   💡 {}...\n   ⚠️ Deprecated ruleset\n\n**2. ✅ b_rs**\n   📋 Title: B\n   📊 Rules: 2\n\n💡 **Use `vibemk_show_ruleset` with ruleset name for detailed information**", "x".repeat(100));
    assert_eq!(text(&out), expected);
    assert!(!is_error(&out));
}

#[tokio::test]
async fn search_without_results_names_the_criteria() {
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_search_rulesets",
            &json!({"fulltext": "zz", "name": "q"}),
        )
        .await;
    assert_eq!(text(&out), "🔍 **No Rulesets Found**\n\nNo rulesets match text 'zz' and name 'q'.\n\n💡 **Tips:**\n• Try broader search terms\n• Check if deprecated rulesets should be included\n• Verify folder path is correct");
    assert!(text(&h.handle("vibemk_search_rulesets", &json!({})).await)
        .contains("match your criteria."));
}

#[tokio::test]
async fn search_maps_http_statuses() {
    for (status, expect) in [
        (400, "❌ **Search Parameter Error**\n\nInvalid search parameters: HTTP 400: Bad Request — bad one"),
        (403, "❌ **Permission Denied**\n\nAccess denied. You may not have 'wato.rulesets' permission for ruleset management."),
        (406, "❌ **Accept Header Error**\n\nAPI cannot satisfy the requested content type. Check Accept headers."),
    ] {
        let (server, h) = setup().await;
        Mock::given(method("GET")).and(path(ALL)).respond_with(ResponseTemplate::new(status).set_body_json(json!({"detail": "bad one"}))).mount(&server).await;
        let out = h.handle("vibemk_search_rulesets", &json!({})).await;
        assert_eq!(text(&out), expect);
        assert!(is_error(&out));
    }
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({})))
        .mount(&server)
        .await;
    assert!(text(&h.handle("vibemk_search_rulesets", &json!({})).await)
        .starts_with("❌ **Ruleset Search Failed**\n\n"));
}

#[tokio::test]
async fn show_gets_the_ruleset_and_formats_details() {
    let (server, h) = setup().await;
    // The shape objects/ruleset/{name} has on 2.3 and 2.5.
    let body = json!({"id": "checkgroup_parameters:cpu", "title": "T", "extensions": {"name": "checkgroup_parameters:cpu", "deprecated": true, "number_of_rules": 4}});
    Mock::given(method("GET"))
        .and(path(
            "/cmk/check_mk/api/1.0/objects/ruleset/checkgroup_parameters%3Acpu",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_show_ruleset",
            &json!({"ruleset_name": "checkgroup_parameters:cpu"}),
        )
        .await;
    assert_eq!(text(&out), "📋 **Ruleset Details: checkgroup_parameters:cpu**\n\n⚠️ **DEPRECATED RULESET**\n\n**Title:** T\n**Number of Rules:** 4\n\n**Available Operations:**\n• Use `vibemk_get_ruleset` to see actual rules in this ruleset\n• Use `vibemk_create_rule` to add new rules to this ruleset\n• Use `vibemk_search_rulesets` to find related rulesets\n\n⚠️ **Note:** This is a deprecated ruleset. Consider using newer alternatives.");
}

#[tokio::test]
async fn show_reports_missing_not_found_and_denied() {
    let (server, h) = setup().await;
    assert_eq!(
        text(&h.handle("vibemk_show_ruleset", &json!({})).await),
        "❌ **Missing parameter**\n\nruleset_name is required"
    );
    Mock::given(method("GET"))
        .and(path("/cmk/check_mk/api/1.0/objects/ruleset/nope"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cmk/check_mk/api/1.0/objects/ruleset/locked"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({})))
        .mount(&server)
        .await;
    let out = h
        .handle("vibemk_show_ruleset", &json!({"ruleset_name": "nope"}))
        .await;
    assert_eq!(text(&out), "❌ **Ruleset Not Found**\n\nRuleset 'nope' not found. Check the ruleset name and try again.");
    assert!(is_error(&out));
    assert!(text(
        &h.handle("vibemk_show_ruleset", &json!({"ruleset_name": "locked"}))
            .await
    )
    .starts_with("❌ **Permission Denied**"));
    assert_eq!(
        text(
            &h.handle("vibemk_show_ruleset", &json!({"ruleset_name": ".."}))
                .await
        )
        .split("\n\n")
        .next()
        .unwrap(),
        "❌ **Unexpected Error**"
    );
}

#[tokio::test]
async fn list_limits_groups_by_prefix_and_sends_used_deprecated() {
    let (server, h) = setup().await;
    let mut v: Vec<Value> = (0..7)
        .map(|i| rs(&format!("cpu_{i}"), &format!("C{i}")))
        .collect();
    v.push(rs("mem_1", "M1"));
    v.push(rs("mem_2", "M2"));
    v.push(json!({"id": "solo", "extensions": {"title": "S"}}));
    Mock::given(method("GET"))
        .and(path(ALL))
        .and(query_param("used", "true"))
        .and(query_param("deprecated", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": v})))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_list_rulesets",
            &json!({"limit": 9, "show_deprecated": true}),
        )
        .await;
    let t = text(&out);
    assert!(t.starts_with("📋 **Available Rulesets**\n\n**Including deprecated rulesets**\n\n**Showing 9 rulesets (limited to 9)**\n\n**CPU:**\n  ✅ `cpu_0` - C0\n"));
    assert!(t.contains("  ... and 2 more cpu rulesets\n\n**MEM:**\n  ✅ `mem_1` - M1\n"));
    assert!(t.contains("`mem_2` - M2") && !t.contains("solo"));
    assert!(t.ends_with("\n⚠️ **Note:** Results limited to 9. Use search filters for more specific results.\n\n💡 **Usage:**\n• `vibemk_show_ruleset` - Get detailed information about a specific ruleset\n• `vibemk_search_rulesets` - Search rulesets with filters\n• `vibemk_get_ruleset` - View rules within a ruleset\n"));
}

#[tokio::test]
async fn list_defaults_and_errors() {
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(ALL))
        .and(query_param("deprecated", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [rs("a", "A")]})))
        .mount(&server)
        .await;
    assert_eq!(text(&h.handle("vibemk_list_rulesets", &json!({})).await), "📋 **Available Rulesets**\n\n**Showing 1 rulesets**\n\n\n💡 **Usage:**\n• `vibemk_show_ruleset` - Get detailed information about a specific ruleset\n• `vibemk_search_rulesets` - Search rulesets with filters\n• `vibemk_get_ruleset` - View rules within a ruleset\n");
    let (server, h) = setup().await;
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({})))
        .mount(&server)
        .await;
    assert_eq!(
        text(&h.handle("vibemk_list_rulesets", &json!({})).await),
        "❌ **Permission Denied**\n\nAccess denied. You may not have 'wato.rulesets' permission."
    );
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}

#[tokio::test]
async fn get_rulesets_hides_the_none_string_checkmk_sends_for_no_help() {
    let (server, client) = common::fake_checkmk().await;
    let body = json!({"value": [{"id": "checkgroup_parameters:interfaces",
        "extensions": {"title": "Network interfaces and switch ports", "help": "None"}}]});
    Mock::given(method("GET"))
        .and(path(ALL))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let out = vibemk::handlers::rules::RulesHandler::new(client)
        .handle("vibemk_get_rulesets", &json!({}))
        .await;
    let t = text(&out);
    assert!(t.contains("Help: No description..."), "{t}");
    assert!(!t.contains("None"), "{t}");
}
