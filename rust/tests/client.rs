//! Port of tests/test_api_client.py, test_client_retry.py, test_client_transport.py
//! (headers, user agent) and test_error_detail.py, plus redirects and URL detection.
//! urllib mocks become a wiremock server: the observable behaviour is asserted.

mod common;

use std::time::Duration;

use serde_json::{json, Value};
use vibemk::client::{retry_delay, ApiError, ApiErrorKind, CheckMkClient, Method, RequestOptions};
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{fake_checkmk, fake_checkmk_retrying, test_config, API};

fn url(endpoint: &str) -> String {
    format!("{API}/{endpoint}")
}

fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code)
}

/// A 5xx/429 that asks for no waiting, so retry tests stay fast.
fn failing(code: u16) -> ResponseTemplate {
    status(code).insert_header("Retry-After", "0")
}

async fn hits(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

// ---------- test_api_client.py ----------

#[tokio::test]
async fn client_initialization() {
    let config = test_config("http://test-checkmk.local:8080");
    let client = CheckMkClient::new(config.clone());
    assert_eq!(client.config, config);
    assert_eq!(
        client.api_base_url,
        "http://test-checkmk.local:8080/cmk/check_mk/api/1.0"
    );

    // the headers the client sets on every request
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .and(header_exists("authorization"))
        .and(header("content-type", "application/json"))
        .and(header("accept", "application/json"))
        .respond_with(status(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    client.get("version", None, &[]).await.unwrap();
}

#[tokio::test]
async fn successful_get_request() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(status(200).set_body_json(json!({"site": "test_site"})))
        .mount(&server)
        .await;
    let result = client.get("version", None, &[]).await.unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.data["site"], "test_site");
    assert_eq!(result.raw, r#"{"site":"test_site"}"#);
}

#[tokio::test]
async fn successful_post_request() {
    let (server, client) = fake_checkmk().await;
    let host = json!({"host_name": "test-server-01", "folder": "/servers/test"});
    Mock::given(method("POST"))
        .and(path(url("domain-types/host/collections/all")))
        .and(body_json(host.clone()))
        .respond_with(status(200).set_body_json(json!({"id": "test-server-01"})))
        .expect(1)
        .mount(&server)
        .await;
    let result = client
        .post("domain-types/host/collections/all", Some(&host), &[])
        .await
        .unwrap();
    assert_eq!(result.data["id"], "test-server-01");
}

#[tokio::test]
async fn authentication_error() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(
            status(401)
                .set_body_json(json!({"title": "Unauthorized", "detail": "Invalid credentials"})),
        )
        .mount(&server)
        .await;
    let err = client.get("version", None, &[]).await.unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Authentication);
    assert_eq!(err.status_code, Some(401));
    assert_eq!(
        err.to_string(),
        "Authentication failed: Unauthorized — Invalid credentials"
    );
}

#[tokio::test]
async fn connection_error() {
    let config = test_config(&common::dead_url());
    let client = CheckMkClient::new(config);
    let err = client.get("version", None, &[]).await.unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Connection);
    assert!(
        err.message.starts_with("Connection error:"),
        "{}",
        err.message
    );
}

#[tokio::test]
async fn retry_mechanism() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(failing(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(status(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    let result = client.get("version", None, &[]).await.unwrap();
    assert_eq!(result.data["ok"], true);
    assert_eq!(hits(&server).await, 3, "1 original + 2 retries");
}

#[tokio::test]
async fn url_encoding() {
    let (server, client) = fake_checkmk().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(status(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let params = json!({
        "host_name": "test-server with spaces",
        "service": "CPU%usage",
        "columns": ["name", "state of x"],
        "query": {"op": "=", "left": "name", "right": "a b"},
        "flag": true,
        "folder": "/a/b",
    });
    client
        .get("objects/host/test", Some(&params), &[])
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let query = requests[0].url.query().unwrap().to_string();
    assert!(
        query.contains("host_name=test-server%20with%20spaces"),
        "{query}"
    );
    assert!(query.contains("service=CPU%25usage"), "{query}");
    assert!(
        query.contains("columns=name&columns=state%20of%20x"),
        "{query}"
    );
    assert!(query.contains("query=%7B%22op%22%3A%22%3D%22%2C%22left%22%3A%22name%22%2C%22right%22%3A%22a%20b%22%7D"), "{query}");
    assert!(
        query.contains("flag=True"),
        "Python str() of a bool: {query}"
    );
    assert!(query.contains("folder=/a/b"), "quote() keeps '/': {query}");
    // decoded, every value arrives intact
    let pairs: Vec<(String, String)> = requests[0]
        .url
        .query_pairs()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    assert!(pairs.contains(&("host_name".into(), "test-server with spaces".into())));
    assert!(pairs.contains(&("service".into(), "CPU%usage".into())));
}

#[tokio::test]
async fn json_parsing_error() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(status(200).set_body_string(r#"{"invalid": json}"#))
        .mount(&server)
        .await;
    let err = client.get("version", None, &[]).await.unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Api);
    assert!(
        err.message.starts_with("Invalid JSON response"),
        "{}",
        err.message
    );
    assert_eq!(err.response_data["raw"], r#"{"invalid": json}"#);
}

#[tokio::test]
async fn timeout_handling() {
    let server = MockServer::start().await;
    let mut config = test_config(&server.uri());
    config.timeout = 1;
    let client = CheckMkClient::with_api_base_url(config, format!("{}{API}", server.uri()));
    Mock::given(method("GET"))
        .and(path(url("version")))
        .respond_with(status(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let err = client.get("version", None, &[]).await.unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Connection);
    assert!(err.message.contains("timeout"), "{}", err.message);
}

// ---------- test_client_retry.py ----------

#[tokio::test]
async fn get_is_retried_on_server_error() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("GET"))
        .respond_with(failing(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    client.get("version", None, &[]).await.unwrap();
    assert_eq!(hits(&server).await, 3);
}

#[tokio::test]
async fn delete_is_retried_on_server_error() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("DELETE"))
        .respond_with(failing(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(status(204))
        .mount(&server)
        .await;
    client
        .delete("objects/host_config/gone", None, &[])
        .await
        .unwrap();
    assert_eq!(hits(&server).await, 2);
}

/// E5: the first attempt deleted the object, then a proxy answered 502.
#[tokio::test]
async fn a_404_on_a_replayed_delete_is_the_delete_succeeding() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("DELETE"))
        .respond_with(failing(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(status(404))
        .mount(&server)
        .await;
    client
        .delete("objects/host_config/web01", None, &[])
        .await
        .unwrap();
    assert_eq!(hits(&server).await, 2);
}

#[tokio::test]
async fn a_404_on_the_first_delete_or_a_replayed_get_is_still_not_found() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("DELETE"))
        .respond_with(status(404))
        .mount(&server)
        .await;
    let e = client
        .delete("objects/host_config/never-there", None, &[])
        .await
        .unwrap_err();
    assert_eq!(e.status_code, Some(404));

    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("GET"))
        .respond_with(failing(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(status(404))
        .mount(&server)
        .await;
    let e = client
        .get("objects/host_config/web01", None, &[])
        .await
        .unwrap_err();
    assert_eq!(e.status_code, Some(404));
}

#[tokio::test]
async fn put_is_retried_on_server_error() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("PUT"))
        .respond_with(failing(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    client
        .put("objects/host_config/x", Some(&json!({})), &[])
        .await
        .unwrap();
    assert_eq!(hits(&server).await, 2);
}

#[tokio::test]
async fn post_is_not_retried_on_server_error() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("POST"))
        .respond_with(failing(500))
        .mount(&server)
        .await;
    let err = client
        .post(
            "domain-types/downtime/actions/delete/invoke",
            Some(&json!({"delete_type": "by_id"})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Api);
    assert_eq!(
        hits(&server).await,
        1,
        "a POST must not be repeated after a 5xx"
    );
}

#[tokio::test]
async fn patch_is_not_retried_on_server_error() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("PATCH"))
        .respond_with(failing(500))
        .mount(&server)
        .await;
    let err = client
        .patch(
            "objects/host_config/web01",
            Some(&json!({"attributes": {}})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Api);
    assert_eq!(hits(&server).await, 1);
}

#[tokio::test]
async fn retries_stop_after_max_retries() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("GET"))
        .respond_with(failing(502))
        .mount(&server)
        .await;
    let err = client.get("version", None, &[]).await.unwrap_err();
    assert_eq!(err.status_code, Some(502));
    assert_eq!(hits(&server).await, 3);
}

#[tokio::test]
async fn rate_limited_request_is_retried() {
    let (server, client) = fake_checkmk_retrying(2).await;
    Mock::given(method("GET"))
        .respond_with(failing(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    client.get("version", None, &[]).await.unwrap();
    assert_eq!(hits(&server).await, 2);
}

#[test]
fn retry_after_sets_the_delay() {
    assert_eq!(retry_delay(Some("7"), 0), Duration::from_secs(7));
    assert_eq!(retry_delay(Some(" 7 "), 3), Duration::from_secs(7));
    assert_eq!(retry_delay(Some("-5"), 2), Duration::ZERO, "never negative");
    assert_eq!(
        retry_delay(Some("3600"), 0),
        Duration::from_secs(30),
        "capped at 30 s"
    );
}

#[test]
fn unparseable_retry_after_falls_back_to_backoff() {
    assert_eq!(
        retry_delay(Some("Wed, 21 Oct 2026 07:28:00 GMT"), 0),
        Duration::from_secs(1)
    );
    assert_eq!(retry_delay(None, 2), Duration::from_secs(4));
    assert_eq!(retry_delay(Some(""), 1), Duration::from_secs(2));
    assert_eq!(retry_delay(Some("inf"), 0), Duration::from_secs(1));
}

#[tokio::test]
async fn empty_object_is_sent_as_an_empty_object() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    client
        .post(
            "domain-types/folder_config/actions/bulk-update/invoke",
            Some(&json!({})),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap()[0].body, b"{}");
}

#[tokio::test]
async fn no_data_sends_no_body() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    client
        .post(
            "domain-types/activation_run/actions/activate-changes/invoke",
            None,
            &[],
        )
        .await
        .unwrap();
    assert!(server.received_requests().await.unwrap()[0].body.is_empty());
}

// ---------- test_client_transport.py (per-request headers, user agent) ----------

#[tokio::test]
async fn delete_forwards_custom_headers() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("DELETE"))
        .and(header("if-match", "\"abc123\""))
        .respond_with(status(204))
        .expect(1)
        .mount(&server)
        .await;
    client
        .delete(
            "objects/folder_config/test",
            None,
            &[("If-Match".into(), "\"abc123\"".into())],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn post_forwards_custom_headers() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("POST"))
        .and(header("if-match", "\"etag\""))
        .respond_with(status(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    client
        .post(
            "objects/host_config/x/actions/move/invoke",
            Some(&json!({})),
            &[("If-Match".into(), "\"etag\"".into())],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delete_still_works_without_headers() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("DELETE"))
        .respond_with(status(204))
        .mount(&server)
        .await;
    let result = client
        .delete("objects/host_config/test", None, &[])
        .await
        .unwrap();
    assert_eq!(result.status, 204);
    assert_eq!(result.data, json!({}), "an empty body is an empty object");
    assert!(server.received_requests().await.unwrap()[0]
        .headers
        .get("if-match")
        .is_none());
}

#[tokio::test]
async fn user_agent_reports_the_server_version_and_auth_is_basic() {
    let (server, client) = fake_checkmk().await;
    // automation:test-secret
    Mock::given(method("GET"))
        .and(header("user-agent", "vibeMK/0.6.4"))
        .and(header(
            "authorization",
            "Basic YXV0b21hdGlvbjp0ZXN0LXNlY3JldA==",
        ))
        .respond_with(status(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    client.get("version", None, &[]).await.unwrap();
}

#[tokio::test]
async fn invalid_custom_header_is_an_error_not_a_panic() {
    let (_server, client) = fake_checkmk().await;
    let err = client
        .get("version", None, &[("bad header".into(), "x".into())])
        .await
        .unwrap_err();
    assert!(err.message.contains("Invalid HTTP header"));
}

// ---------- redirects, view API prefix ----------

fn no_follow() -> RequestOptions {
    RequestOptions {
        follow_redirects: false,
        ..RequestOptions::default()
    }
}

#[tokio::test]
async fn unfollowed_redirect_comes_back_as_a_result() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(status(303).insert_header("Location", "/cmk/check_mk/api/1.0/objects/x"))
        .mount(&server)
        .await;
    let result = client
        .request(
            Method::Post,
            "domain-types/service_discovery_run/actions/start/invoke",
            None,
            Some(&json!({})),
            &[],
            no_follow(),
        )
        .await
        .unwrap();
    assert_eq!(result.status, 303);
    assert_eq!(
        result.header("location"),
        Some("/cmk/check_mk/api/1.0/objects/x")
    );
}

#[tokio::test]
async fn same_host_redirect_is_followed() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path(url("a")))
        .respond_with(status(302).insert_header("Location", url("b")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(url("b")))
        .and(header_exists("authorization"))
        .respond_with(status(200).set_body_json(json!({"at": "b"})))
        .mount(&server)
        .await;
    assert_eq!(client.get("a", None, &[]).await.unwrap().data["at"], "b");
}

#[tokio::test]
async fn redirect_to_another_host_never_carries_the_authorization_header() {
    let (server, client) = fake_checkmk().await;
    let other = MockServer::start().await; // another port = another origin
    Mock::given(method("GET"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&other)
        .await;
    Mock::given(method("GET"))
        .and(path(url("a")))
        .respond_with(status(302).insert_header("Location", format!("{}/leak", other.uri())))
        .mount(&server)
        .await;
    let err = client.get("a", None, &[]).await.unwrap_err();
    assert_eq!(err.status_code, Some(302), "not followed, reported");
    assert_eq!(
        hits(&other).await,
        0,
        "the other host must not be contacted with our credentials"
    );
}

#[tokio::test]
async fn view_api_endpoint_skips_the_rest_prefix() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("GET"))
        .and(path("/cmk/view.py"))
        .and(query_param("view_name", "allhosts"))
        .respond_with(status(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;
    let opts = RequestOptions {
        use_api_prefix: false,
        ..RequestOptions::default()
    };
    let result = client
        .request(
            Method::Get,
            "view.py",
            Some(&json!({"view_name": "allhosts"})),
            None,
            &[],
            opts,
        )
        .await
        .unwrap();
    assert_eq!(result.data, json!([]));
}

// ---------- error mapping ----------

#[tokio::test]
async fn http_statuses_map_to_error_kinds() {
    let (server, client) = fake_checkmk().await;
    for (code, kind, message) in [
        (
            403,
            ApiErrorKind::Permission,
            "Permission denied: Forbidden",
        ),
        (404, ApiErrorKind::NotFound, "Resource not found: Not Found"),
        (400, ApiErrorKind::Api, "HTTP 400: Bad Request"),
        (409, ApiErrorKind::Api, "HTTP 409: Conflict"),
    ] {
        Mock::given(method("GET"))
            .and(path(url(&format!("s{code}"))))
            .respond_with(status(code))
            .mount(&server)
            .await;
        let err = client
            .get(&format!("s{code}"), None, &[])
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str(), err.status_code),
            (kind, message, Some(code))
        );
        assert_eq!(
            err.response_data,
            json!({"error": err.message.split(": ").nth(1).unwrap()}),
            "no JSON body -> {{error: reason}}"
        );
    }
}

// ---------- URL detection (client.py _detect_api_url) ----------

#[tokio::test]
async fn connect_picks_the_first_pattern_that_answers_200() {
    let server = MockServer::start().await;
    // the canonical site path redirects to a login page (SSO): refused; the second works
    Mock::given(method("GET"))
        .and(path("/cmk/check_mk/api/1.0/version"))
        .respond_with(status(302).insert_header("Location", "/login"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/check_mk/api/1.0/version"))
        .respond_with(status(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = CheckMkClient::connect(test_config(&server.uri()))
        .await
        .unwrap();
    assert_eq!(
        client.api_base_url,
        format!("{}/check_mk/api/1.0", server.uri())
    );
    let debug = client.get_debug_results();
    assert!(debug[0].starts_with("Testing: "), "{debug:?}");
    assert!(debug.iter().any(|l| l.starts_with("HTTP 302")), "{debug:?}");
    assert_eq!(
        debug.last().unwrap(),
        &format!("SUCCESS: {}/check_mk/api/1.0", server.uri())
    );
}

#[tokio::test]
async fn connect_falls_back_when_nothing_answers() {
    let server = MockServer::start().await; // every path answers 404
    let client = CheckMkClient::connect(test_config(&server.uri()))
        .await
        .unwrap();
    let fallback = format!("{}/cmk/check_mk/api/1.0", server.uri());
    assert_eq!(client.api_base_url, fallback);
    let debug = client.get_debug_results();
    assert_eq!(debug.len(), 4 * 2 + 1, "{debug:?}");
    assert!(debug
        .iter()
        .any(|l| l.starts_with("HTTP 404 (Not Found): ")));
    assert_eq!(debug.last().unwrap(), &format!("FALLBACK: {fallback}"));
}

#[tokio::test]
async fn connect_records_connection_errors_and_falls_back() {
    let client = CheckMkClient::connect(test_config(&common::dead_url()))
        .await
        .unwrap();
    assert!(client
        .get_debug_results()
        .iter()
        .any(|l| l.starts_with("ERROR (")));
    assert!(client.api_base_url.ends_with("/cmk/check_mk/api/1.0"));
}

#[tokio::test]
async fn debug_results_default_when_never_detected() {
    let (_server, client) = fake_checkmk().await;
    assert_eq!(
        client.get_debug_results(),
        vec!["No debug info available".to_string()]
    );
}

// ---------- test_error_detail.py ----------

fn api_error(message: &str, status: u16, data: Value) -> ApiError {
    ApiError::new(ApiErrorKind::Api, message)
        .with_status(status)
        .with_data(data)
}

#[test]
fn the_detail_is_part_of_the_string() {
    let e = api_error(
        "HTTP 400: Bad Request",
        400,
        json!({"title": "Bad Request", "detail": "These fields have problems: ipaddress"}),
    );
    assert!(e.to_string().contains("ipaddress"));
}

#[test]
fn the_original_message_is_kept() {
    assert!(
        api_error("HTTP 400: Bad Request", 400, json!({"detail": "nope"}))
            .to_string()
            .contains("HTTP 400")
    );
}

#[test]
fn a_title_is_used_when_there_is_no_detail() {
    assert!(api_error(
        "HTTP 409: Conflict",
        409,
        json!({"title": "The host is already known"})
    )
    .to_string()
    .contains("already known"));
}

#[test]
fn a_detail_that_merely_repeats_the_message_is_not_doubled() {
    let e = api_error("HTTP 404: Not Found", 404, json!({"title": "Not Found"}));
    assert_eq!(e.to_string().matches("Not Found").count(), 1);
}

#[test]
fn an_error_without_a_body_reads_as_before() {
    assert_eq!(
        ApiError::new(ApiErrorKind::Generic, "Connection refused").to_string(),
        "Connection refused"
    );
}

#[test]
fn a_body_without_detail_does_not_crash_the_message() {
    let e = api_error(
        "HTTP 502: Bad Gateway",
        502,
        json!({"raw": "<html>gateway</html>"}),
    );
    assert!(e.to_string().contains("502"));
    assert_eq!(
        api_error("HTTP 500: x", 500, json!(["not", "a", "dict"])).to_string(),
        "HTTP 500: x"
    );
}

/// The client raises on every HTTP error and never returns a failed result: handlers
/// need no `success` branch. Every status of 300 and above (unfollowed redirects aside) is Err.
#[tokio::test]
async fn the_client_errs_rather_than_reporting_failure() {
    let (server, client) = fake_checkmk().await;
    for code in [400u16, 401, 403, 404, 409, 500, 503] {
        Mock::given(method("GET"))
            .and(path(url(&format!("e{code}"))))
            .respond_with(status(code))
            .mount(&server)
            .await;
        assert!(
            client.get(&format!("e{code}"), None, &[]).await.is_err(),
            "{code}"
        );
    }
}

#[tokio::test]
async fn the_error_body_detail_reaches_the_message() {
    let (server, client) = fake_checkmk().await;
    Mock::given(method("POST"))
        .respond_with(status(400).set_body_json(
            json!({"title": "Bad Request", "detail": "These fields have problems: ipaddress"}),
        ))
        .mount(&server)
        .await;
    let err = client
        .post(
            "domain-types/host_config/collections/all",
            Some(&json!({})),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "HTTP 400: Bad Request — These fields have problems: ipaddress"
    );
}
