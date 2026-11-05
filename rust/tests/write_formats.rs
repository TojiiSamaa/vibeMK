//! Port of tests/test_write_formats.py: the request bodies the write tools send,
//! in the shape CheckMK 2.5 accepts. Driven through the Dispatcher.

mod common;

use serde_json::{json, Value};
use vibemk::dispatch::Dispatcher;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, API};

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

async fn bodies(server: &MockServer, p: &str) -> Vec<Value> {
    let full = format!("{API}/{p}");
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == full)
        .map(|r| r.body_json().unwrap_or(Value::Null))
        .collect()
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Python repr of a literal (tuples, True/False/None, single quotes) as JSON; tuples become arrays.
fn literal(s: &str) -> Value {
    let j = s
        .replace('(', "[")
        .replace(')', "]")
        .replace('\'', "\"")
        .replace("True", "true")
        .replace("False", "false")
        .replace("None", "null")
        .replace(",]", "]");
    serde_json::from_str(&j).unwrap_or_else(|e| panic!("not a literal: {s} ({e})"))
}

const RULES: &str = "domain-types/rule/collections/all";

/// Create an active check and return the `value_raw` it posted, parsed.
async fn value_sent(tool: &str, arguments: Value) -> Value {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        "objects/host_config/web01",
        200,
        json!({"extensions": {"folder": "/"}}),
    )
    .await;
    mount(&server, "POST", RULES, 200, json!({"id": "rule-1"})).await;
    let mut args = json!({"hostname": "web01"});
    args.as_object_mut()
        .unwrap()
        .extend(arguments.as_object().unwrap().clone());
    let r = d.call_tool(tool, Some(&args)).await;
    assert_eq!(r["isError"], false, "{}", text(&r));
    let sent = bodies(&server, RULES).await;
    literal(
        sent.last().expect("a rule was posted")["value_raw"]
            .as_str()
            .expect("value_raw"),
    )
}

fn secret() -> Value {
    json!(["cmk_postprocessed", "explicit_password", ["", "s3cret"]])
}

const DAYS: f64 = 86400.0;

#[tokio::test]
async fn http_levels_password_regex_and_address_family() {
    let v = value_sent(
        "vibemk_create_http_check",
        json!({"response_time_warn": 1, "response_time_crit": 2, "auth_user": "probe", "auth_password": "s3cret", "expect_regex": "o.", "address_family": "ipv4"}),
    )
    .await;
    let url = &v["mode"][1];
    assert_eq!(url["response_time"], json!(["fixed", [1.0, 2.0]]));
    assert_eq!(url["auth"], json!({"user": "probe", "password": secret()}));
    assert_eq!(
        url["expect_regex"],
        json!({"regex": "o.", "case_insensitive": false, "crit_if_found": false, "multiline": false})
    );
    assert_eq!(v["host"]["address_family"], "ipv4_enforced");
}

#[tokio::test]
async fn http_certificate_days_are_seconds() {
    let v = value_sent(
        "vibemk_create_http_check",
        json!({"cert_mode": true, "cert_days_warn": 30, "cert_days_crit": 14}),
    )
    .await;
    assert_eq!(
        v["mode"],
        json!(["cert", {"cert_days": ["fixed", [30.0 * DAYS, 14.0 * DAYS]]}])
    );
}

#[tokio::test]
async fn tcp_levels_and_certificate_days() {
    let v = value_sent("vibemk_create_tcp_check", json!({"port": 443, "cert_days_warn": 30, "cert_days_crit": 14, "response_time_warn": 1, "response_time_crit": 2})).await;
    assert_eq!(v["cert_days"], json!(["fixed", [30.0 * DAYS, 14.0 * DAYS]]));
    assert_eq!(v["response_time"], json!(["fixed", [1.0, 2.0]]));
}

#[tokio::test]
async fn icmp_timeout_is_an_integer() {
    // A legacy valuespec: 20.0 was refused as "wrong type float, must be int".
    assert_eq!(
        value_sent("vibemk_create_icmp_check", json!({})).await["timeout"],
        json!(20)
    );
    let t = value_sent("vibemk_create_icmp_check", json!({"timeout": 10})).await["timeout"].clone();
    assert!(t.is_i64() || t.is_u64(), "{t}");
}

#[tokio::test]
async fn smtp_certificate_days_are_seconds() {
    let v = value_sent(
        "vibemk_create_smtp_check",
        json!({"starttls": true, "cert_days_warn": 30, "cert_days_crit": 14}),
    )
    .await;
    assert_eq!(v["cert_days"], json!(["fixed", [30.0 * DAYS, 14.0 * DAYS]]));
}

#[tokio::test]
async fn ftp_sends_no_passive_key() {
    assert!(value_sent("vibemk_create_ftp_check", json!({}))
        .await
        .get("passive")
        .is_none());
}

#[tokio::test]
async fn ldap_response_time_is_seconds_and_authentication_a_dictionary() {
    let v = value_sent(
        "vibemk_create_ldap_check",
        json!({"base_dn": "dc=example,dc=com", "bind_dn": "cn=probe", "password": "s3cret", "response_time_warn_ms": 500, "response_time_crit_ms": 800}),
    )
    .await;
    assert_eq!(v["response_time"], json!(["fixed", [0.5, 0.8]]));
    assert_eq!(
        v["authentication"],
        json!({"bind_dn": "cn=probe", "password": secret()})
    );
}

#[tokio::test]
async fn smb_host_is_a_cascading_choice() {
    for (smb_host, expected) in [
        ("use_parent_host", json!(["use_parent_host", ""])),
        ("fileserver", json!(["define_host", "fileserver"])),
    ] {
        let v = value_sent("vibemk_create_smb_check", json!({"share": "data", "smb_host": smb_host, "username": "probe", "password": "s3cret"})).await;
        assert_eq!(v["host"], expected);
        assert_eq!(v["levels"], json!(["fixed", [85.0, 95.0]]));
        assert_eq!(v["auth"], json!({"user": "probe", "password": secret()}));
    }
}

#[tokio::test]
async fn mkevents_ignore_acknowledged_is_present_or_absent() {
    // A FixedValue(True): False was refused, the key has to be left out.
    assert!(value_sent(
        "vibemk_create_mkevents_check",
        json!({"ignore_acknowledged": false})
    )
    .await
    .get("ignore_acknowledged")
    .is_none());
    assert_eq!(
        value_sent("vibemk_create_mkevents_check", json!({})).await["ignore_acknowledged"],
        json!(true)
    );
}

#[tokio::test]
async fn mkevents_remote_and_last_log_use_the_rulesets_values() {
    let v = value_sent(
        "vibemk_create_mkevents_check",
        json!({"remote_ec_host": "10.0.0.1", "show_last_log": "none"}),
    )
    .await;
    assert_eq!(v["remote"], json!(["10.0.0.1", 6558]));
    assert_eq!(v["show_last_log"], "no");
}

// ------------------------------------------------------------------ event console

#[tokio::test]
async fn event_writes_name_the_site_the_event_lives_on() {
    // An event ID is unique per site only; CheckMK refused the body without site_id.
    for (tool, args, p) in [
        (
            "vibemk_acknowledge_event",
            json!({"event_id": 7}),
            "objects/event_console/7/actions/update_and_acknowledge/invoke",
        ),
        (
            "vibemk_change_event_state",
            json!({"event_id": 7, "new_state": "warning"}),
            "objects/event_console/7/actions/change_state/invoke",
        ),
    ] {
        let (server, d) = dispatcher().await;
        mount(&server, "POST", p, 204, json!({})).await;
        d.call_tool(tool, Some(&args)).await;
        assert_eq!(bodies(&server, p).await[0]["site_id"], "cmk", "{tool}");
    }
}

const EC_DELETE: &str = "domain-types/event_console/actions/delete/invoke";

#[tokio::test]
async fn deleting_by_id_is_one_by_id_call_per_event() {
    let (server, d) = dispatcher().await;
    mount(&server, "POST", EC_DELETE, 204, json!({})).await;
    d.call_tool(
        "vibemk_delete_events",
        Some(&json!({"event_ids": [7, 8], "site_id": "remote"})),
    )
    .await;
    assert_eq!(
        bodies(&server, EC_DELETE).await,
        vec![
            json!({"filter_type": "by_id", "site_id": "remote", "event_id": 7}),
            json!({"filter_type": "by_id", "site_id": "remote", "event_id": 8}),
        ]
    );
}

#[tokio::test]
async fn deleting_by_filter_names_the_filter_type() {
    let (server, d) = dispatcher().await;
    mount(&server, "POST", EC_DELETE, 204, json!({})).await;
    d.call_tool("vibemk_delete_events", Some(&json!({"host": "web01"})))
        .await;
    assert_eq!(
        bodies(&server, EC_DELETE).await[0],
        json!({"filter_type": "params", "filters": {"phase": "open", "host": "web01"}})
    );
}

// ------------------------------------------------------------------ agent bakery

#[tokio::test]
async fn the_download_url_is_the_action_checkmk_serves() {
    let (_server, d) = dispatcher().await;
    let r = d
        .call_tool(
            "vibemk_download_agent_by_host",
            Some(&json!({"host_name": "web01", "os_type": "windows_msi"})),
        )
        .await;
    assert!(
        text(&r).contains("/domain-types/agent/actions/download_by_host/invoke?os_type=windows_msi&host_name=web01&agent_type=host_name"),
        "{}",
        text(&r)
    );
}

#[tokio::test]
async fn the_baking_status_is_read_from_the_job() {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        "domain-types/agent/actions/baking_status/invoke",
        200,
        json!({"resultType": "object", "result": {"value": {"state": "finished", "started": 1790984580.75, "duration": 1.73,
            "loginfo": {"JobProgressUpdate": [], "JobResult": ["Baking successful"], "JobException": []}}}}),
    )
    .await;
    let t = text(&d.call_tool("vibemk_baking_status", Some(&json!({}))).await);
    assert!(t.contains("State: finished"), "{t}");
    assert!(t.contains("Result: Baking successful"), "{t}");
}

#[tokio::test]
async fn a_raw_site_is_told_it_has_no_bakery() {
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "POST",
        "domain-types/agent/actions/bake/invoke",
        404,
        json!({"title": "Not Found", "status": 404, "detail": "Resource not found"}),
    )
    .await;
    let r = d.call_tool("vibemk_bake_agents", Some(&json!({}))).await;
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("no agent bakery"), "{}", text(&r));
}

// ------------------------------------------------------------------ discovery, service groups

#[tokio::test]
async fn bulk_discovery_status_is_read_from_the_background_job() {
    // The state sits under extensions.status; the tool reported UNKNOWN for every job.
    let (server, d) = dispatcher().await;
    mount(
        &server,
        "GET",
        "objects/background_job/bulk_discovery-1",
        200,
        json!({"extensions": {"site_id": "cmk", "active": false, "status": {"state": "finished", "log_info": {
            "JobProgressUpdate": ["Acquired lock", "[1/1] web01: discovery successful", "Hosts: 1 total (1 succeeded, 0 skipped, 0 failed)"],
            "JobResult": ["Bulk discovery finished"], "JobException": []}}}}),
    )
    .await;
    let t = text(
        &d.call_tool(
            "vibemk_get_bulk_discovery_status",
            Some(&json!({"job_id": "bulk_discovery-1"})),
        )
        .await,
    );
    assert!(t.contains("State: FINISHED"), "{t}");
    assert!(t.contains("Hosts: 1 total (1 succeeded"), "{t}");
    assert!(!t.contains("Acquired lock"), "{t}");
}

#[tokio::test]
async fn a_service_groups_alias_is_its_title() {
    // CheckMK returns the alias as the title and leaves extensions empty.
    let (server, d) = dispatcher().await;
    mount(&server, "GET", "objects/service_group_config/web", 200, json!({"domainType": "service_group_config", "id": "web", "title": "Web services", "extensions": {}})).await;
    let t = text(
        &d.call_tool("vibemk_get_service_group", Some(&json!({"name": "web"})))
            .await,
    );
    assert!(t.contains("**Alias:** Web services"), "{t}");
}
