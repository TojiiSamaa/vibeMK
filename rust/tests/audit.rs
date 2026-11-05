//! AUDIT.md « À reporter dans le port Rust » : one test per finding not already pinned elsewhere.
//! Already covered: S1 tests/client.rs `redirect_to_another_host_never_carries_the_authorization_header`,
//! E6 tests/hosts.rs `update_host_412_gives_the_concurrency_advice`, S4 tests/connection.rs, E7 tests/user_roles.rs.
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{fake_checkmk, fake_checkmk_retrying, API};
use serde_json::{json, Value};
use vibemk::client::{retry_delay, ApiResponse, Method, RequestOptions};
use vibemk::dispatch::Dispatcher;
use vibemk::handlers::base::time_ago;
use vibemk::http::router_at;
use vibemk::registry::ToolRegistry;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const PENDING: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/collections/pending_changes";
const ACTIVATE: &str =
    "/cmk/check_mk/api/1.0/domain-types/activation_run/actions/activate-changes/invoke";

fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// E3: a retried request keeps follow_redirects=false, so the 3xx after a 503 comes back as is.
#[tokio::test]
async fn e3_a_retry_keeps_follow_redirects_false() {
    let (server, client) = fake_checkmk_retrying(1).await;
    let p = format!("{API}/version");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/elsewhere"))
        .mount(&server)
        .await;
    let r = client
        .request(
            Method::Get,
            "version",
            None,
            None,
            &[],
            RequestOptions {
                use_api_prefix: true,
                follow_redirects: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.header("location"), Some("/elsewhere"));
}

/// E4: Retry-After is capped at 30 s.
#[test]
fn e4_retry_after_is_capped() {
    assert_eq!(retry_delay(Some("3600"), 0), Duration::from_secs(30));
}

/// E1: response headers are looked up whatever their casing.
#[test]
fn e1_header_lookup_ignores_case() {
    let r = ApiResponse {
        headers: [("etag".to_string(), "\"abc\"".to_string())].into(),
        ..Default::default()
    };
    assert_eq!(r.header("ETag"), Some("\"abc\""));
}

/// E10a: a future timestamp reads "0s ago", never a negative age.
#[test]
fn e10a_time_ago_of_a_future_date_is_zero() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(time_ago(&json!(now + 3600)).as_deref(), Some("0s ago"));
}

/// S3: a non-ASCII bearer token is a 401, not a 500.
#[tokio::test]
async fn s3_non_ascii_token_is_a_401() {
    let (_fake, client) = fake_checkmk().await;
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(ToolRegistry::from_client(client))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router_at("/mcp", dispatcher, "s3cr3t-token-value".into()),
        )
        .await
    });
    let value =
        reqwest::header::HeaderValue::from_bytes("Bearer s3cr3t-tökén-value".as_bytes()).unwrap();
    let r = reqwest::Client::new()
        .post(&url)
        .header("Authorization", value)
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

/// N1 + E2/E11: a host read whose body is null and a null pending-changes body never lose the write report.
#[tokio::test]
async fn n1_e11_null_bodies_keep_the_write_report() {
    let (server, client) = fake_checkmk().await;
    let host = format!("{API}/objects/host_config/web1");
    Mock::given(method("GET"))
        .and(path(host.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(host))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .mount(&server)
        .await;
    let d = Dispatcher::from_client(client, false);
    let r = d.call_tool("vibemk_update_host", Some(&json!({"host_name": "web1", "attributes": {"alias": "New"}, "activate_changes": true}))).await;
    let text = text_of(&r);
    assert_eq!(r["isError"], false, "{text}");
    assert!(text.contains("Host Updated Successfully"), "{text}");
    assert!(text.contains("No pending changes to activate"), "{text}");
}

/// E9a: the automatic activation says "started", never "activated".
#[tokio::test]
async fn e9a_activation_is_reported_as_started() {
    let (server, client) = fake_checkmk().await;
    let host = format!("{API}/objects/host_config/web1");
    Mock::given(method("GET"))
        .and(path(host.clone()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"extensions": {"attributes": {}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(host))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PENDING))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{"id": "c1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let d = Dispatcher::from_client(client, false);
    let r = d.call_tool("vibemk_update_host", Some(&json!({"host_name": "web1", "attributes": {"alias": "New"}, "activate_changes": true}))).await;
    let text = text_of(&r);
    assert!(text.contains("Activation started"), "{text}");
    assert!(!text.contains("activated."), "{text}");
}

/// Records WARN event messages; minimal tracing Subscriber so no new crate is needed.
struct WarnRecorder(Arc<std::sync::Mutex<Vec<String>>>);

impl tracing::Subscriber for WarnRecorder {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Msg(String);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{value:?}"));
            }
        }
        if *event.metadata().level() == tracing::Level::WARN {
            let mut msg = Msg(String::new());
            event.record(&mut msg);
            self.0.lock().unwrap().push(msg.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// E8: falling back to If-Match "*" must leave a warning (Python: test_E8_wildcard_fallback_is_logged).
#[test]
fn e8_wildcard_fallback_is_logged() {
    use vibemk::handlers::base::Base;
    let warnings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let no_etag = ApiResponse {
        status: 200,
        data: json!({"extensions": {"meta_data": {"etag": null}}}),
        raw: String::new(),
        headers: Default::default(),
    };
    let with_etag = ApiResponse {
        headers: [("etag".to_string(), "\"abc\"".to_string())].into(),
        ..no_etag.clone()
    };
    let (wild, tagged) = tracing::subscriber::with_default(WarnRecorder(warnings.clone()), || {
        let wild = Base::extract_etag(&no_etag);
        let before = warnings.lock().unwrap().len();
        (wild, (Base::extract_etag(&with_etag), before))
    });
    assert_eq!(wild, "*");
    assert_eq!(tagged.0, "\"abc\"");
    let warnings = warnings.lock().unwrap();
    assert_eq!(
        warnings.len(),
        1,
        "one warning, only for the fallback: {warnings:?}"
    );
    assert_eq!(tagged.1, 1);
    assert!(warnings[0].contains("If-Match '*'"), "{}", warnings[0]);
}
