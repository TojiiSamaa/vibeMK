//! Port of tests/test_handlers_active_checks.py (18 tests) plus the value_raw shapes of the 1:1 port.
mod common;

use serde_json::{json, Value};
use vibemk::handlers::active_checks::ActiveChecksHandler;
use vibemk::handlers::base::Handler;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RULES: &str = "/cmk/check_mk/api/1.0/domain-types/rule/collections/all";

/// tool -> (minimal arguments, the ruleset CheckMK expects)
fn checks() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "vibemk_create_http_check",
            json!({"hostname": "web01", "uri": "/health"}),
            "active_checks:http",
        ),
        (
            "vibemk_create_tcp_check",
            json!({"hostname": "web01", "port": 443}),
            "active_checks:tcp",
        ),
        (
            "vibemk_create_icmp_check",
            json!({"hostname": "web01"}),
            "active_checks:icmp",
        ),
        (
            "vibemk_create_dns_check",
            json!({"hostname": "web01", "hostname_to_query": "example.org"}),
            "active_checks:dns",
        ),
        (
            "vibemk_create_smtp_check",
            json!({"hostname": "mail01"}),
            "active_checks:smtp",
        ),
        (
            "vibemk_create_ftp_check",
            json!({"hostname": "ftp01"}),
            "active_checks:ftp",
        ),
        (
            "vibemk_create_ldap_check",
            json!({"hostname": "dc01", "base_dn": "dc=example,dc=org"}),
            "active_checks:ldap",
        ),
        (
            "vibemk_create_smb_check",
            json!({"hostname": "fs01", "share": "public"}),
            "active_checks:disk_smb",
        ),
        (
            "vibemk_create_mkevents_check",
            json!({"hostname": "log01"}),
            "active_checks:mkevents",
        ),
        (
            "vibemk_create_inventory_check",
            json!({"hostname": "web01"}),
            "active_checks:cmk_inv",
        ),
        (
            "vibemk_create_custom_check",
            json!({"hostname": "web01", "command_line": "/bin/true", "service_description": "Custom"}),
            "custom_checks",
        ),
    ]
}

/// Fake CheckMK: rule creation answers {"id": "rule-1"}, every host lives in /servers/web.
async fn setup() -> (MockServer, ActiveChecksHandler) {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(RULES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "rule-1"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"/objects/host_config/[^/]+$",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"extensions": {"folder": "/servers/web"}})),
        )
        .mount(&server)
        .await;
    (server, ActiveChecksHandler::new(client))
}

async fn requests(server: &MockServer, verb: &str) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == verb)
        .collect()
}

/// The rule body the handler posted.
async fn sent_rule(server: &MockServer) -> Value {
    serde_json::from_slice(
        &requests(server, "POST")
            .await
            .last()
            .expect("no rule posted")
            .body,
    )
    .unwrap()
}

fn text(blocks: &[Value]) -> String {
    blocks[0]["text"].as_str().unwrap().to_string()
}

/// Is `raw` a Python literal and not JSON? Strings are removed first; what is left must not hold
/// JSON-only tokens and the brackets must balance (stands in for ast.literal_eval).
fn assert_python_literal(raw: &str) {
    let (mut rest, mut quote, mut escaped) = (String::new(), None, false);
    for c in raw.chars() {
        match (quote, c) {
            (Some(_), _) if escaped => escaped = false,
            (Some(_), '\\') => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, c) => rest.push(c),
        }
    }
    assert!(quote.is_none(), "unterminated string in {raw}");
    for json_only in ["true", "false", "null"] {
        assert!(!rest.contains(json_only), "JSON token {json_only} in {raw}");
    }
    let mut depth = 0i32;
    for c in rest.chars() {
        depth += i32::from(matches!(c, '{' | '[' | '(')) - i32::from(matches!(c, '}' | ']' | ')'));
        assert!(depth >= 0, "unbalanced {raw}");
    }
    assert_eq!(depth, 0, "unbalanced {raw}");
    assert!(rest.starts_with('{'), "a rule value is a dict: {raw}");
}

#[tokio::test]
async fn the_value_parses_as_a_python_literal() {
    for (tool, args, _) in checks() {
        let (server, h) = setup().await;
        h.handle(tool, &args).await;
        assert_python_literal(sent_rule(&server).await["value_raw"].as_str().unwrap());
    }
}

#[tokio::test]
async fn it_targets_the_ruleset_checkmk_serves() {
    for (tool, args, ruleset) in checks() {
        let (server, h) = setup().await;
        h.handle(tool, &args).await;
        assert_eq!(sent_rule(&server).await["ruleset"], ruleset, "{tool}");
    }
}

#[tokio::test]
async fn the_rule_is_conditioned_on_the_host_it_was_asked_for() {
    for (tool, args, _) in checks() {
        let (server, h) = setup().await;
        h.handle(tool, &args).await;
        let cond = &sent_rule(&server).await["conditions"];
        assert_eq!(
            cond["host_name"]["match_on"],
            json!([args["hostname"]]),
            "{tool}"
        );
        assert_eq!(cond["host_name"]["operator"], "one_of");
    }
}

#[tokio::test]
async fn no_check_is_created_disabled() {
    for (tool, args, _) in checks() {
        let (server, h) = setup().await;
        h.handle(tool, &args).await;
        assert_eq!(
            sent_rule(&server).await["properties"]["disabled"],
            false,
            "{tool}"
        );
    }
}

#[tokio::test]
async fn tcp_needs_a_port() {
    let (server, h) = setup().await;
    let out = h
        .handle("vibemk_create_tcp_check", &json!({"hostname": "web01"}))
        .await;
    assert!(text(&out).starts_with('❌'));
    assert!(requests(&server, "POST").await.is_empty());
}

#[tokio::test]
async fn icmp_needs_a_hostname() {
    let (server, h) = setup().await;
    let out = h.handle("vibemk_create_icmp_check", &json!({})).await;
    assert!(text(&out).starts_with('❌'));
    assert!(requests(&server, "POST").await.is_empty());
}

#[tokio::test]
async fn an_unknown_tool_is_reported_not_silently_ignored() {
    let (server, h) = setup().await;
    let out = h
        .handle("vibemk_create_quantum_check", &json!({"hostname": "web01"}))
        .await;
    assert!(text(&out).starts_with('❌'));
    assert!(requests(&server, "POST").await.is_empty());
}

#[tokio::test]
async fn an_explicit_folder_wins() {
    let (server, h) = setup().await;
    h.handle(
        "vibemk_create_icmp_check",
        &json!({"hostname": "web01", "folder": "~custom"}),
    )
    .await;
    assert_eq!(sent_rule(&server).await["folder"], "~custom");
    assert!(requests(&server, "GET").await.is_empty());
}

#[tokio::test]
async fn otherwise_it_follows_the_host() {
    let (server, h) = setup().await;
    h.handle("vibemk_create_icmp_check", &json!({"hostname": "web01"}))
        .await;
    assert_eq!(sent_rule(&server).await["folder"], "~servers~web");
    assert!(requests(&server, "GET").await[0]
        .url
        .path()
        .ends_with("/objects/host_config/web01"));
}

#[tokio::test]
async fn a_host_in_the_root_folder_resolves_to_the_root() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(RULES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"extensions": {"folder": "/"}})),
        )
        .mount(&server)
        .await;
    ActiveChecksHandler::new(client)
        .handle("vibemk_create_icmp_check", &json!({"hostname": "web01"}))
        .await;
    assert_eq!(sent_rule(&server).await["folder"], "~");
}

#[tokio::test]
async fn a_failed_lookup_falls_back_to_the_root_rather_than_raising() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(RULES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "r"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"title": "host not found"})))
        .mount(&server)
        .await;
    let out = ActiveChecksHandler::new(client)
        .handle("vibemk_create_icmp_check", &json!({"hostname": "ghost"}))
        .await;
    assert!(text(&out).starts_with('✅'), "{out:?}");
    assert_eq!(sent_rule(&server).await["folder"], "~");
}

#[tokio::test]
async fn a_description_is_sent_when_given() {
    let (server, h) = setup().await;
    h.handle(
        "vibemk_create_icmp_check",
        &json!({"hostname": "web01", "description": "why this exists"}),
    )
    .await;
    assert_eq!(
        sent_rule(&server).await["properties"]["description"],
        "why this exists"
    );
}

#[tokio::test]
async fn no_empty_description_is_sent() {
    let (server, h) = setup().await;
    h.handle(
        "vibemk_create_icmp_check",
        &json!({"hostname": "web01", "description": ""}),
    )
    .await;
    assert!(sent_rule(&server).await["properties"]
        .get("description")
        .is_none());
}

#[tokio::test]
async fn rules_go_to_the_collection_endpoint() {
    let (server, h) = setup().await;
    h.handle("vibemk_create_icmp_check", &json!({"hostname": "web01"}))
        .await;
    assert_eq!(
        requests(&server, "POST").await.last().unwrap().url.path(),
        RULES
    );
}

#[tokio::test]
async fn the_host_condition_carries_the_empty_groups_checkmk_expects() {
    let (server, h) = setup().await;
    h.handle("vibemk_create_icmp_check", &json!({"hostname": "web01"}))
        .await;
    let cond = &sent_rule(&server).await["conditions"];
    for key in ["host_tags", "host_label_groups", "service_label_groups"] {
        assert_eq!(cond[key], json!([]), "{key}");
    }
}

#[tokio::test]
async fn listing_asks_for_every_active_check_ruleset() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(RULES))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    ActiveChecksHandler::new(client)
        .handle("vibemk_list_active_checks", &json!({}))
        .await;
    let mut asked: Vec<String> = requests(&server, "GET")
        .await
        .iter()
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "ruleset_name")
                .unwrap()
                .1
                .to_string()
        })
        .collect();
    asked.sort();
    let mut expected: Vec<String> = checks().iter().map(|c| c.2.to_string()).collect();
    expected.sort();
    assert_eq!(asked, expected);
}

#[tokio::test]
async fn deleting_needs_a_rule_id() {
    let (server, h) = setup().await;
    let out = h.handle("vibemk_delete_active_check", &json!({})).await;
    assert!(text(&out).starts_with('❌'));
    assert!(requests(&server, "DELETE").await.is_empty());
}

#[tokio::test]
async fn deleting_addresses_the_rule_by_id() {
    let (server, h) = setup().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let out = h
        .handle("vibemk_delete_active_check", &json!({"rule_id": "rule-42"}))
        .await;
    assert!(requests(&server, "DELETE").await[0]
        .url
        .path()
        .ends_with("/objects/rule/rule-42"));
    assert_eq!(text(&out), "✅ Active check rule 'rule-42' deleted.");
}

// ---- beyond the Python tests: the exact value_raw literals, listing and deletion by host ----

#[tokio::test]
async fn http_value_raw_is_the_exact_python_literal() {
    let (server, h) = setup().await;
    let out = h
        .handle("vibemk_create_http_check", &json!({"hostname": "web01", "uri": "/health", "ssl": true, "port": "8443", "response_time_warn": 1, "response_time_crit": 2, "method": "get", "address_family": "ipv4"}))
        .await;
    assert_eq!(
        sent_rule(&server).await["value_raw"],
        "{'name': 'HTTP', 'host': {'port': 8443, 'address_family': 'ipv4_enforced', 'virthost': 'web01'}, 'mode': ('url', {'uri': '/health', 'ssl': 'auto', 'response_time': ('fixed', (1.0, 2.0)), 'method': 'GET'})}"
    );
    assert_eq!(text(&out), "✅ HTTP check 'HTTP' created for 'web01'.\nURI: /health, SSL: True, Port: 8443\nMethod: get\nRule-ID: rule-1");
}

#[tokio::test]
async fn http_certificate_mode_and_secrets_use_the_form_spec_shapes() {
    let (server, h) = setup().await;
    h.handle(
        "vibemk_create_http_check",
        &json!({"hostname": "web01", "cert_mode": true}),
    )
    .await;
    assert_eq!(sent_rule(&server).await["value_raw"], "{'name': 'HTTP', 'host': {'virthost': 'web01'}, 'mode': ('cert', {'cert_days': ('fixed', (1209600.0, 604800.0))})}");
    h.handle(
        "vibemk_create_smb_check",
        &json!({"hostname": "fs01", "share": "public", "username": "u", "password": "p"}),
    )
    .await;
    assert_eq!(
        sent_rule(&server).await["value_raw"],
        "{'share': 'public', 'host': ('use_parent_host', ''), 'levels': ('fixed', (85.0, 95.0)), 'auth': {'user': 'u', 'password': ('cmk_postprocessed', 'explicit_password', ('', 'p'))}}"
    );
}

#[tokio::test]
async fn tcp_and_mkevents_values_match_the_python_ones() {
    let (server, h) = setup().await;
    let out = h
        .handle(
            "vibemk_create_tcp_check",
            &json!({"hostname": "web01", "port": 443, "expect": "OK", "ssl": true}),
        )
        .await;
    assert_eq!(
        sent_rule(&server).await["value_raw"],
        "{'port': 443, 'ssl': True, 'expect': ['OK']}"
    );
    assert_eq!(text(&out), "✅ TCP check on port 443 created for 'web01'.\nService: TCP Port 443, SSL: True\nExpect: OK\nRule-ID: rule-1");
    h.handle(
        "vibemk_create_mkevents_check",
        &json!({"hostname": "log01", "show_last_log": "long", "remote_ec_host": "ec1"}),
    )
    .await;
    assert_eq!(
        sent_rule(&server).await["value_raw"],
        "{'hostspec': ['$HOSTNAME$', '$HOSTADDRESS$', '$HOSTALIAS$'], 'ignore_acknowledged': True, 'remote': ('ec1', 6558), 'show_last_log': 'details'}"
    );
}

#[tokio::test]
async fn a_failing_api_call_is_an_api_error_and_a_bad_number_an_unexpected_one() {
    let (server, client) = common::fake_checkmk().await;
    Mock::given(method("POST"))
        .and(path(RULES))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"title": "Bad", "detail": "nope"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let h = ActiveChecksHandler::new(client);
    let out = h
        .handle("vibemk_create_icmp_check", &json!({"hostname": "web01"}))
        .await;
    assert!(
        text(&out).starts_with("❌ **CheckMK API Error**\n\n"),
        "{out:?}"
    );
    assert_eq!(out[1]["type"], "_error");
    let out = h
        .handle(
            "vibemk_create_tcp_check",
            &json!({"hostname": "web01", "port": "abc"}),
        )
        .await;
    assert_eq!(
        text(&out),
        "❌ **Unexpected Error**\n\ninvalid literal for int() with base 10: 'abc'"
    );
}

#[tokio::test]
async fn listing_shows_rules_of_the_host_and_deleting_by_host_needs_one_match() {
    let (server, client) = common::fake_checkmk().await;
    let rule = |id: &str, host: &str, disabled: bool| json!({"id": id, "extensions": {"value_raw": "{'port': 443}", "conditions": {"host_name": {"match_on": [host]}}, "properties": {"disabled": disabled}}});
    Mock::given(method("GET"))
        .and(path(RULES))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"value": [rule("r1", "web01", false), rule("r2", "db01", true)]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let h = ActiveChecksHandler::new(client);

    let out = text(
        &h.handle(
            "vibemk_list_active_checks",
            &json!({"hostname": "web01", "check_type": "tcp"}),
        )
        .await,
    );
    assert_eq!(out, "🔍 **Active Checks**\n\n• [🟢 active] **active_checks:tcp** — web01\n  ID: r1\n  Value: `{'port': 443}`");

    let out = text(
        &h.handle(
            "vibemk_delete_active_check",
            &json!({"hostname": "ghost", "check_type": "tcp"}),
        )
        .await,
    );
    assert_eq!(
        out,
        "❌ **No rule found**\n\nNo active check rule for 'ghost'."
    );

    let out = text(
        &h.handle(
            "vibemk_delete_active_check",
            &json!({"hostname": "web01", "check_type": "tcp"}),
        )
        .await,
    );
    assert_eq!(out, "✅ Active check rule 'r1' deleted.");
    assert!(requests(&server, "DELETE").await[0]
        .url
        .path()
        .ends_with("/objects/rule/r1"));
}
