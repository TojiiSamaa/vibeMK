//! The six write tools no other test called, checked against the Python handlers
//! (hosts.py `_bulk_create_hosts`, `_bulk_update_hosts`, `_create_cluster_host`, `_rename_host`;
//! monitoring.py `_add_comment`, `_delete_comment`): request path and body, success text, errors.
mod common;

use common::{fake_checkmk, API};
use serde_json::{json, Value};
use vibemk::dispatch::is_error;
use vibemk::handlers::base::Handler;
use vibemk::handlers::hosts::HostHandler;
use vibemk::handlers::monitoring::MonitoringHandler;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount one expected call: the verb, the API path and the request body must all match, exactly once.
async fn expect(server: &MockServer, verb: &str, p: &str, body: Value, status: u16, reply: Value) {
    Mock::given(method(verb))
        .and(path(format!("{API}/{p}")))
        .and(body_partial_json(body))
        .respond_with(ResponseTemplate::new(status).set_body_json(reply))
        .expect(1)
        .mount(server)
        .await;
}

fn text(r: &[Value]) -> &str {
    r[0]["text"].as_str().unwrap()
}

async fn hosts() -> (MockServer, HostHandler) {
    let (s, c) = fake_checkmk().await;
    (s, HostHandler::new(c))
}

async fn monitoring() -> (MockServer, MonitoringHandler) {
    let (s, c) = fake_checkmk().await;
    (s, MonitoringHandler::new(c))
}

#[tokio::test]
async fn bulk_create_hosts_sends_root_as_tilde_and_bake_agent() {
    let (s, h) = hosts().await;
    expect(
        &s,
        "POST",
        "domain-types/host_config/actions/bulk-create/invoke",
        json!({"entries": [{"host_name": "web01", "folder": "~"}], "bake_agent": true}),
        200,
        json!({"value": [{"id": "web01"}]}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_bulk_create_hosts",
            &json!({"entries": [{"host_name": "web01"}], "bake_agent": true}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("web01"));
}

#[tokio::test]
async fn bulk_create_hosts_lists_every_validation_error_and_calls_nothing() {
    let (s, h) = hosts().await;
    let r = h
        .handle(
            "vibemk_bulk_create_hosts",
            &json!({"entries": [{"host_name": "bad host!"}, {"folder": "/"}, {"host_name": "ok", "attributes": {"ipaddress": "999.1.1.1"}}]}),
        )
        .await;
    assert!(is_error(&r));
    let t = text(&r);
    assert!(t.contains("Validation failed"));
    assert!(t.contains("Entry 1: Invalid host name 'bad host!'"), "{t}");
    assert!(t.contains("Entry 2: host_name is required"), "{t}");
    assert!(t.contains("Entry 3: Invalid IP address '999.1.1.1'"), "{t}");
    assert!(s.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn bulk_create_hosts_needs_entries() {
    let (_s, h) = hosts().await;
    let r = h.handle("vibemk_bulk_create_hosts", &json!({})).await;
    assert!(is_error(&r));
    assert!(text(&r).contains("entries list is required"));
}

#[tokio::test]
async fn bulk_update_hosts_puts_the_entries_and_counts_them() {
    let (s, h) = hosts().await;
    let entries = json!([{"host_name": "web01", "attributes": {"alias": "a"}}, {"host_name": "web02", "attributes": {"alias": "b"}}]);
    expect(
        &s,
        "PUT",
        "domain-types/host_config/actions/bulk-update/invoke",
        json!({ "entries": entries }),
        200,
        json!({"value": []}),
    )
    .await;
    let r = h
        .handle("vibemk_bulk_update_hosts", &json!({ "entries": entries }))
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    assert!(text(&r).contains("Bulk Update Successful"));
    assert!(text(&r).contains("**updated**: 2"));
    let none = h.handle("vibemk_bulk_update_hosts", &json!({})).await;
    assert!(is_error(&none));
}

#[tokio::test]
async fn bulk_update_hosts_reports_a_refusal() {
    let (s, h) = hosts().await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "{API}/domain-types/host_config/actions/bulk-update/invoke"
        )))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"title": "Bad"})))
        .mount(&s)
        .await;
    let r = h
        .handle(
            "vibemk_bulk_update_hosts",
            &json!({"entries": [{"host_name": "x"}]}),
        )
        .await;
    assert!(is_error(&r));
}

#[tokio::test]
async fn create_cluster_host_marks_it_agentless_with_its_nodes() {
    let (s, h) = hosts().await;
    expect(
        &s,
        "POST",
        "domain-types/host_config/collections/clusters",
        json!({"folder": "~", "host_name": "cl01", "nodes": ["n1", "n2"], "attributes": {"tag_agent": "no-agent", "alias": "c"}}),
        200,
        json!({"id": "cl01"}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_create_cluster_host",
            &json!({"host_name": "cl01", "nodes": ["n1", "n2"], "attributes": {"alias": "c"}}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.contains("Cluster Host Created Successfully"));
    assert!(t.contains("**Nodes:** n1, n2"));
    assert!(t.contains("Node Count: 2"));
    // nodes sit beside the attributes, never inside them
    let sent: Value = s.received_requests().await.unwrap()[0].body_json().unwrap();
    assert!(sent["attributes"].get("nodes").is_none(), "{sent}");
}

#[tokio::test]
async fn create_cluster_host_lets_the_caller_override_tag_agent() {
    let (s, h) = hosts().await;
    expect(
        &s,
        "POST",
        "domain-types/host_config/collections/clusters",
        json!({"attributes": {"tag_agent": "cmk-agent"}}),
        200,
        json!({"id": "cl01"}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_create_cluster_host",
            &json!({"host_name": "cl01", "nodes": ["n1"], "attributes": {"tag_agent": "cmk-agent"}}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
}

#[tokio::test]
async fn create_cluster_host_requires_a_name_and_nodes() {
    let (_s, h) = hosts().await;
    let no_name = h
        .handle("vibemk_create_cluster_host", &json!({"nodes": ["n"]}))
        .await;
    assert!(is_error(&no_name) && text(&no_name).contains("host_name is required"));
    let no_nodes = h
        .handle("vibemk_create_cluster_host", &json!({"host_name": "c"}))
        .await;
    assert!(is_error(&no_nodes) && text(&no_nodes).contains("nodes list is required"));
}

#[tokio::test]
async fn rename_host_starts_a_job_and_shows_its_id() {
    let (s, h) = hosts().await;
    expect(
        &s,
        "PUT",
        "objects/host_config/web01/actions/rename/invoke",
        json!({"new_name": "web02"}),
        200,
        json!({"id": "job-7"}),
    )
    .await;
    let r = h
        .handle(
            "vibemk_rename_host",
            &json!({"host_name": "web01", "new_name": "web02"}),
        )
        .await;
    assert!(!is_error(&r), "{}", text(&r));
    let t = text(&r);
    assert!(t.contains("Host Rename Started"));
    assert!(t.contains("Old name: web01") && t.contains("New name: web02"));
    assert!(t.contains("Job ID: job-7"));
    assert!(t.contains("activate changes after the job completes"));
}

/// 2.3 answers 302, 2.5 answers 303: the redirect is the success and is not followed.
#[tokio::test]
async fn rename_host_takes_a_redirect_as_the_started_job() {
    for status in [302, 303] {
        let (s, h) = hosts().await;
        Mock::given(method("PUT"))
            .and(path(format!(
                "{API}/objects/host_config/web01/actions/rename/invoke"
            )))
            .and(body_partial_json(json!({"new_name": "web02"})))
            .respond_with(ResponseTemplate::new(status).insert_header(
                "Location",
                format!("{API}/objects/background_job/rename/actions/wait-for-completion/invoke"),
            ))
            .expect(1)
            .mount(&s)
            .await;
        let r = h
            .handle(
                "vibemk_rename_host",
                &json!({"host_name": "web01", "new_name": "web02"}),
            )
            .await;
        assert!(!is_error(&r), "{status}: {}", text(&r));
        let t = text(&r);
        assert!(
            t.contains("Host Rename Started") && !t.contains("Job ID"),
            "{t}"
        );
        // only the PUT: the redirect was not followed
        assert_eq!(s.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn rename_host_needs_both_names() {
    let (_s, h) = hosts().await;
    let r = h
        .handle("vibemk_rename_host", &json!({"host_name": "web01"}))
        .await;
    assert!(is_error(&r));
    assert!(text(&r).contains("host_name and new_name are required"));
}

#[tokio::test]
async fn add_comment_on_a_host_and_on_a_service() {
    let (s, m) = monitoring().await;
    expect(
        &s,
        "POST",
        "domain-types/comment/collections/host",
        json!({"comment_type": "host", "host_name": "web01", "comment": "hello", "persistent": true}),
        204,
        json!({}),
    )
    .await;
    expect(
        &s,
        "POST",
        "domain-types/comment/collections/service",
        json!({"comment_type": "service", "host_name": "web01", "service_description": "CPU load", "comment": "slow", "persistent": false}),
        204,
        json!({}),
    )
    .await;
    let host = m
        .handle(
            "vibemk_add_comment",
            &json!({"comment_type": "host", "host_name": "web01", "comment": "hello"}),
        )
        .await;
    let t = text(&host);
    assert!(
        t.contains("Comment Added") && t.contains("Target: host 'web01'"),
        "{t}"
    );
    assert!(t.contains("Persistent: True"), "{t}");
    let service = m
        .handle(
            "vibemk_add_comment",
            &json!({"comment_type": "service", "host_name": "web01", "service_description": "CPU load", "comment": "slow", "persistent": false}),
        )
        .await;
    let t = text(&service);
    assert!(t.contains("Target: service 'web01/CPU load'"), "{t}");
    assert!(t.contains("Persistent: False"), "{t}");
}

#[tokio::test]
async fn add_comment_validates_its_arguments() {
    let (s, m) = monitoring().await;
    for (args, needle) in [
        (
            json!({"host_name": "h", "comment": "c"}),
            "comment_type, host_name, and comment are required",
        ),
        (
            json!({"comment_type": "service", "host_name": "h", "comment": "c"}),
            "service_description is required",
        ),
        (
            json!({"comment_type": "other", "host_name": "h", "comment": "c"}),
            "must be 'host' or 'service'",
        ),
    ] {
        let r = m.handle("vibemk_add_comment", &args).await;
        assert!(
            is_error(&r) && text(&r).contains(needle),
            "{args}: {}",
            text(&r)
        );
    }
    assert!(s.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_comment_by_id_and_by_query() {
    let (s, m) = monitoring().await;
    expect(
        &s,
        "POST",
        "domain-types/comment/actions/delete/invoke",
        json!({"delete_type": "by_id", "comment_id": 42, "site_id": "cmk"}),
        204,
        json!({}),
    )
    .await;
    expect(
        &s,
        "POST",
        "domain-types/comment/actions/delete/invoke",
        json!({"delete_type": "params", "host_name": "web01", "service_descriptions": ["CPU load"]}),
        204,
        json!({}),
    )
    .await;
    let by_id = m
        .handle("vibemk_delete_comment", &json!({"comment_id": "42"}))
        .await;
    let t = text(&by_id);
    assert!(
        t.contains("Comment Deleted") && t.contains("comment #42"),
        "{t}"
    );
    let by_query = m
        .handle(
            "vibemk_delete_comment",
            &json!({"delete_type": "by_query", "host_name": "web01", "service_description": "CPU load"}),
        )
        .await;
    assert!(
        text(&by_query).contains("Target: web01/CPU load"),
        "{}",
        text(&by_query)
    );
}

#[tokio::test]
async fn delete_comment_validates_its_arguments() {
    let (s, m) = monitoring().await;
    for (args, needle) in [
        (json!({}), "comment_id is required"),
        (json!({"delete_type": "by_query"}), "host_name is required"),
        (
            json!({"delete_type": "nope"}),
            "must be 'by_id' or 'by_query'",
        ),
    ] {
        let r = m.handle("vibemk_delete_comment", &args).await;
        assert!(
            is_error(&r) && text(&r).contains(needle),
            "{args}: {}",
            text(&r)
        );
    }
    assert!(s.received_requests().await.unwrap().is_empty());
}
