//! tools/call of the 4 host group rule tools against a fake CheckMK (method, path, query, body checked).
mod common;

use serde_json::{json, Value};
use vibemk::handlers::base::Handler;
use vibemk::handlers::host_group_rules::HostGroupRulesHandler;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const RULESETS: &str = "/cmk/check_mk/api/1.0/domain-types/ruleset/collections/all";
const RULES: &str = "/cmk/check_mk/api/1.0/domain-types/rule/collections/all";
const RS: &str = "/cmk/check_mk/api/1.0/objects/ruleset";

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}
fn is_error(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "_error")
}

#[tokio::test]
async fn find_searches_five_terms_and_groups_rulesets_by_kind() {
    let (server, client) = common::fake_checkmk().await;
    let value = json!({"value": [
        {"id": "host_contactgroups", "title": "Contact groups"},
        {"id": "host_groups", "title": "Host groups"},
        {"id": "extra_host_conf_notification", "title": "Notif"},
        {"id": "host_check_commands", "title": "Check cmds"},
        {"id": "unrelated", "title": "No"},
    ]});
    for term in ["contact", "group", "host", "notification", "assignment"] {
        Mock::given(method("GET"))
            .and(path(RULESETS))
            .and(query_param("search", term))
            .respond_with(ResponseTemplate::new(200).set_body_json(value.clone()))
            .expect(1)
            .mount(&server)
            .await;
    }
    let out = HostGroupRulesHandler::new(client)
        .handle("vibemk_find_host_grouping_rulesets", &json!({}))
        .await;
    assert_eq!(
        text(&out),
        "🔍 **Host Grouping and Contact Assignment Rulesets**\n\n\n📞 **Contact Group Assignment Rules:**\n   • **host_contactgroups**: Contact groups\n\n🏠 **Host Group Assignment Rules:**\n   • **host_groups**: Host groups\n\n📨 **Notification Rules:**\n   • **extra_host_conf_notification**: Notif\n\n🔧 **Other Related Rules:**\n   • **host_check_commands**: Check cmds\n\n📊 **Summary:** Found 4 relevant rulesets"
    );
}

#[tokio::test]
async fn find_reports_a_failed_search_and_goes_on() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(RULESETS))
        .and(query_param("search", "contact"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(RULESETS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let out = HostGroupRulesHandler::new(client)
        .handle("vibemk_find_host_grouping_rulesets", &json!({}))
        .await;
    let t = text(&out);
    assert!(
        t.starts_with(
            "🔍 **Host Grouping and Contact Assignment Rulesets**\n\nSearch for 'contact' failed: "
        ),
        "{t}"
    );
    assert!(
        t.ends_with("\n\n📊 **Summary:** Found 0 relevant rulesets"),
        "{t}"
    );
}

#[tokio::test]
async fn contactgroup_rule_posts_the_corrected_format() {
    let (server, client) = common::fake_checkmk().await;
    let expected = json!({
        "properties": {"disabled": false, "comment": "c"},
        "value_raw": "['a', 'b']",
        "conditions": {"host_name": {"match_on": ["w.*"], "operator": "one_of"}},
        "ruleset": "host_contactgroups",
        "folder": "~hosts~linux",
    });
    Mock::given(method("POST"))
        .and(path(RULES))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r1"})))
        .expect(1)
        .mount(&server)
        .await;
    let args = json!({"contact_groups": ["a", "b"], "folder": "/hosts/linux", "comment": "c", "host_conditions": {"host_name": {"match_on": ["w.*"], "operator": "one_of"}}});
    let out = HostGroupRulesHandler::new(client)
        .handle("vibemk_create_host_contactgroup_rule", &args)
        .await;
    assert_eq!(
        text(&out),
        "✅ **Host Contact Group Rule Created Successfully**\n\nRuleset: host_contactgroups\nRule ID: r1\nContact Groups: ['a', 'b']\nFolder: /hosts/linux\nComment: c\n\n📝 **Conditions:** {'host_name': {'match_on': ['w.*'], 'operator': 'one_of'}}\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn contactgroup_rule_defaults_single_group_root_folder_and_errors() {
    let (server, client) = common::fake_checkmk().await;
    let expected = json!({"properties": {"disabled": false, "comment": "Host contact group assignment"}, "value_raw": "'admins'", "conditions": {}, "ruleset": "host_contactgroups", "folder": "~"});
    Mock::given(method("POST"))
        .and(path(RULES))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let h = HostGroupRulesHandler::new(client);
    let out = h
        .handle(
            "vibemk_create_host_contactgroup_rule",
            &json!({"contact_groups": ["admins"]}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Host Contact Group Rule Created Successfully**\n\nRuleset: host_contactgroups\nRule ID: unknown\nContact Groups: ['admins']\nFolder: /\nComment: Host contact group assignment\n\n📝 **Conditions:** None (applies to all hosts)\n\n⚠️ **Remember to activate changes!**"
    );
    assert_eq!(
        text(
            &h.handle("vibemk_create_host_contactgroup_rule", &json!({}))
                .await
        ),
        "❌ **Missing parameter**\n\ncontact_groups list is required"
    );
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"title": "Bad", "detail": "nope"})),
        )
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_contactgroup_rule",
            &json!({"contact_groups": ["a"]}),
        )
        .await;
    assert!(
        text(&out)
            .starts_with("❌ **Rule creation failed**\n\nCould not create contact group rule: "),
        "{}",
        text(&out)
    );
    assert!(is_error(&out));
}

#[tokio::test]
async fn hostgroup_rule_finds_the_first_working_ruleset_then_posts() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(format!("{RS}/host_groups")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{RS}/hostgroups")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let expected = json!({
        "ruleset": "hostgroups",
        "folder": "/x",
        "properties": {"disabled": false, "description": "Host group assignment via vibeMK", "comment": "Host group assignment"},
        "value_raw": ["g1", "g2"],
    });
    Mock::given(method("POST"))
        .and(path(RULES))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = HostGroupRulesHandler::new(client)
        .handle(
            "vibemk_create_host_hostgroup_rule",
            &json!({"host_groups": ["g1", "g2"], "folder": "/x"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "✅ **Host Group Assignment Rule Created**\n\nRuleset: hostgroups\nHost Groups: g1, g2\nFolder: /x\nComment: Host group assignment\n\n📝 **Conditions:** None (applies to all hosts)\n\n⚠️ **Remember to activate changes!**"
    );
}

#[tokio::test]
async fn hostgroup_rule_sends_conditions_and_reports_missing_ruleset_and_input() {
    let (server, client) = common::fake_checkmk().await;
    let h = HostGroupRulesHandler::new(client);
    assert_eq!(
        text(
            &h.handle("vibemk_create_host_hostgroup_rule", &json!({}))
                .await
        ),
        "❌ **Missing parameter**\n\nhost_groups list is required"
    );
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_hostgroup_rule",
            &json!({"host_groups": ["g"]}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Host Group Ruleset Not Found**\n\nCould not find a working ruleset for host group assignment.\n\n**Tried rulesets:**\n• host_groups\n• hostgroups\n• host_group_assignment\n• host_grouping\n\n**Recommendation:**\n1. Use 'find_host_grouping_rulesets' to find available rulesets\n2. Check existing host group rules in CheckMK GUI"
    );
    assert!(is_error(&out));
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("{RS}/host_groups")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let cond = json!({"host_tags": [{"key": "env", "operator": "is", "value": "prod"}]});
    let expected = json!({
        "ruleset": "host_groups",
        "folder": "/",
        "properties": {"disabled": false, "description": "Host group assignment via vibeMK", "comment": "k"},
        "value_raw": ["g"],
        "conditions": cond,
    });
    Mock::given(method("POST"))
        .and(path(RULES))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let out = h
        .handle(
            "vibemk_create_host_hostgroup_rule",
            &json!({"host_groups": ["g"], "comment": "k", "host_conditions": cond}),
        )
        .await;
    assert!(
        text(&out).contains(
            "📝 **Conditions:** {'host_tags': [{'key': 'env', 'operator': 'is', 'value': 'prod'}]}"
        ),
        "{}",
        text(&out)
    );
}

#[tokio::test]
async fn example_structures_are_the_fixed_text() {
    let (_server, client) = common::fake_checkmk().await;
    let h = HostGroupRulesHandler::new(client);
    let out = h
        .handle("vibemk_get_example_rule_structures", &json!({}))
        .await;
    let t = text(&out);
    assert!(t.starts_with("📚 **Example Rule Structures for Host Grouping**\n\n\n🔧 **1. Host Contact Group Assignment (Swagger Format)**\n```json\n{\n  \"extensions\": {\n    \"ruleset\": \"host_contactgroups\","), "{t}");
    assert!(t.contains("\n\n💡 **4. Simple All-Hosts Rule (Swagger Format)**\n```json\n{\n  \"extensions\": {\n    \"ruleset\": \"host_contactgroups\",\n    \"folder\": \"/\",\n    \"properties\": {\n      \"comment\": \"Default contact group for all hosts\"\n    },\n    \"value_raw\": [\"monitoring-team\"]\n  }\n}\n```\n"));
    assert!(t.ends_with("• Use `folder_index` for rule positioning (0 = top)\n• Remember to activate changes after creating rules"));
    assert_eq!(t.lines().count(), 107);
    assert_eq!(
        text(&h.handle("vibemk_nope", &json!({})).await),
        "❌ **Unknown tool**\n\nTool 'vibemk_nope' is not supported"
    );
}
