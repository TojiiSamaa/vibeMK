//! Port of src/vibemk/api/client.py (CheckMKClient) and api/exceptions.py.
//! Interfaces are frozen; the request/retry bodies are filled by C1.

use std::collections::HashMap;

use serde_json::Value;

use crate::config::Config;
use crate::paths::UnsafePathError;

/// Extra request headers, e.g. `[("If-Match".into(), etag)]`.
pub type Headers = Vec<(String, String)>;

/// The one shape of a successful result (client.py `_result`); failures are `Err`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub data: Value,
    /// Raw body, kept for the view API parsing.
    pub raw: String,
    /// Response headers (ETag, Location...), names as sent by the server.
    pub headers: HashMap<String, String>,
}

impl ApiResponse {
    /// Header lookup, case-insensitive (Python's dict(headers) kept the server's casing).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Which CheckMKError subclass an error is (exceptions.py).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// CheckMKError itself.
    Generic,
    Connection,
    Authentication,
    Permission,
    Validation,
    NotFound,
    Api,
}

/// Mirror of CheckMKError: message, optional HTTP status, parsed error body.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub message: String,
    pub status_code: Option<u16>,
    pub response_data: Value,
}

impl ApiError {
    pub fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status_code: None,
            response_data: Value::Object(Default::default()),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status_code = Some(status);
        self
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.response_data = data;
        self
    }
}

/// CheckMKError.__str__: message plus CheckMK's own detail/title when it adds something.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pick = |k: &str| {
            self.response_data
                .get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        let detail = pick("detail").or_else(|| pick("title"));
        match detail {
            Some(d) if !d.trim().is_empty() && !self.message.contains(d) => {
                write!(f, "{} — {}", self.message, d)
            }
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<UnsafePathError> for ApiError {
    fn from(e: UnsafePathError) -> Self {
        Self::new(ApiErrorKind::Validation, e.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

/// The less common switches of client.py `request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestOptions {
    /// false: `{server_url}/{site}/{endpoint}` (view API) instead of the REST base.
    pub use_api_prefix: bool,
    /// false: a 3xx comes back as Ok with its status and Location header.
    pub follow_redirects: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            use_api_prefix: true,
            follow_redirects: true,
        }
    }
}

pub struct CheckMkClient {
    pub config: Config,
    pub api_base_url: String,
    http: reqwest::Client,
    /// Same client, never following a 3xx (URL detection, `follow_redirects: false`).
    http_no_redirect: reqwest::Client,
    debug_results: Vec<String>,
}

const RETRYABLE_STATUS: [u16; 5] = [429, 500, 502, 503, 504];

/// Only idempotent methods may be repeated: re-issuing a POST after a gateway
/// timeout can act twice (client.py `_IDEMPOTENT_METHODS`).
fn is_idempotent(method: Method) -> bool {
    matches!(method, Method::Get | Method::Put | Method::Delete)
}

/// Same origin (scheme, host, port) or stop: a redirect to another host must never
/// carry our Authorization header, so it is not followed at all.
fn same_origin_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same = attempt
            .previous()
            .last()
            .is_some_and(|from| from.origin() == attempt.url().origin());
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if same {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn build_http(config: &Config, policy: reqwest::redirect::Policy) -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(!config.verify_ssl)
        .timeout(std::time::Duration::from_secs(config.timeout))
        .redirect(policy)
        // shortcut: a failed TLS backend init falls back to a default (verifying) client, not a panic.
        .build()
        .unwrap_or_default()
}

/// urllib.parse.quote(text) with its default safe="/".
fn quote(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Python str() of a parameter value.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// CheckMK parameter encoding: `columns` list -> repeated columns=, `query` object -> JSON.
fn query_string(params: &Value) -> String {
    let mut parts = Vec::new();
    for (key, value) in params.as_object().into_iter().flatten() {
        match (key.as_str(), value) {
            ("columns", Value::Array(cols)) => parts.extend(
                cols.iter()
                    .map(|c| format!("columns={}", quote(&py_str(c)))),
            ),
            ("query", Value::Object(_)) => {
                parts.push(format!("query={}", quote(&value.to_string())))
            }
            _ => parts.push(format!("{key}={}", quote(&py_str(value)))),
        }
    }
    parts.join("&")
}

/// Delay before the next attempt: Retry-After in seconds if the server gave one
/// (an HTTP-date falls back), else 2**retry_count (client.py `_retry_delay`).
pub fn retry_delay(retry_after: Option<&str>, retry_count: u32) -> std::time::Duration {
    let backoff = 2f64.powi(retry_count.min(30) as i32);
    let secs = retry_after
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite())
        .map_or(backoff, |s| s.clamp(0.0, 30.0)); // cap like client.py:334 (AUDIT E4)
    std::time::Duration::try_from_secs_f64(secs).unwrap_or_default()
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// client.py `_handle_general_error`.
fn transport_error(e: &reqwest::Error) -> ApiError {
    let text = error_chain(e);
    let message = if e.is_timeout() {
        format!("Request timeout: {text}")
    } else if e.is_connect() {
        format!("Connection error: {text}")
    } else {
        format!("Connection failed: {text}")
    };
    ApiError::new(ApiErrorKind::Connection, message)
}

fn collect_headers(map: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    map.iter()
        .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
        .collect()
}

/// client.py `_handle_http_error` mapping.
fn http_error(status: u16, reason: &str, data: Value) -> ApiError {
    let (kind, message) = match status {
        401 => (
            ApiErrorKind::Authentication,
            format!("Authentication failed: {reason}"),
        ),
        403 => (
            ApiErrorKind::Permission,
            format!("Permission denied: {reason}"),
        ),
        404 => (
            ApiErrorKind::NotFound,
            format!("Resource not found: {reason}"),
        ),
        _ => (ApiErrorKind::Api, format!("HTTP {status}: {reason}")),
    };
    ApiError::new(kind, message)
        .with_status(status)
        .with_data(data)
}

impl CheckMkClient {
    /// Client on the fallback REST base `{server_url}/{site}/check_mk/api/1.0`.
    /// URL detection (client.py `_detect_api_url`) is `connect`.
    pub fn new(config: Config) -> Self {
        let base = format!("{}/{}/check_mk/api/1.0", config.server_url, config.site);
        Self::with_api_base_url(config, base)
    }

    /// Client on a known REST base, no detection (tests, fake server).
    pub fn with_api_base_url(config: Config, api_base_url: impl Into<String>) -> Self {
        let http = build_http(&config, same_origin_policy());
        let http_no_redirect = build_http(&config, reqwest::redirect::Policy::none());
        Self {
            config,
            api_base_url: api_base_url.into(),
            http,
            http_no_redirect,
            debug_results: Vec::new(),
        }
    }

    /// Probe the candidate API URLs like client.py `_detect_api_url`: the first base
    /// whose `/version` answers 200 (redirects count as failure) wins, else the fallback.
    pub async fn connect(config: Config) -> Result<Self, ApiError> {
        let mut client = Self::new(config);
        let (url, site) = (client.config.server_url.clone(), client.config.site.clone());
        let patterns = [
            format!("{url}/{site}/check_mk/api/1.0"),
            format!("{url}/check_mk/api/1.0"),
            format!("{url}/api/1.0"),
            format!("{url}/{site}/cmk/check_mk/api/1.0"),
        ];
        let timeout = std::time::Duration::from_secs(client.config.timeout.min(10));
        let headers = client.headers(&[])?;
        let mut debug = Vec::new();
        for base in patterns {
            let test_url = format!("{base}/version");
            debug.push(format!("Testing: {test_url}"));
            let sent = client
                .auth(client.http_no_redirect.get(&test_url))
                .headers(headers.clone())
                .timeout(timeout)
                .send()
                .await;
            match sent {
                Ok(r) if r.status().as_u16() == 200 => {
                    debug.push(format!("SUCCESS: {base}"));
                    client.api_base_url = base;
                    client.debug_results = debug;
                    return Ok(client);
                }
                Ok(r) => {
                    let status = r.status();
                    debug.push(match status.canonical_reason() {
                        Some(reason) if !status.is_success() => {
                            format!("HTTP {} ({reason}): {base}", status.as_u16())
                        }
                        _ => format!("HTTP {}: {base}", status.as_u16()),
                    });
                }
                Err(e) => debug.push(format!("ERROR ({}): {base}", error_chain(&e))),
            }
        }
        debug.push(format!("FALLBACK: {}", client.api_base_url));
        client.debug_results = debug;
        Ok(client)
    }

    /// client.py `get_debug_results`.
    pub fn get_debug_results(&self) -> Vec<String> {
        if self.debug_results.is_empty() {
            vec!["No debug info available".to_string()]
        } else {
            self.debug_results.clone()
        }
    }

    /// Basic auth (marked sensitive by reqwest, so it is also kept out of debug output).
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(&self.config.username, Some(&self.config.password))
    }

    /// JSON + User-Agent defaults, overridden by `custom`.
    fn headers(&self, custom: &[(String, String)]) -> Result<reqwest::header::HeaderMap, ApiError> {
        use reqwest::header::{HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
        let bad = |what: &str| {
            ApiError::new(ApiErrorKind::Generic, format!("Invalid HTTP header {what}"))
        };
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(ACCEPT, HeaderValue::from_static("application/json"));
        map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let agent = HeaderValue::from_str(&format!("vibeMK/{}", crate::config::MCP_VERSION))
            .map_err(|_| bad("User-Agent"))?;
        map.insert(USER_AGENT, agent);
        for (name, value) in custom {
            let n = HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad(name))?;
            map.insert(n, HeaderValue::from_str(value).map_err(|_| bad(name))?);
        }
        Ok(map)
    }

    /// client.py `request` (l.153): params is a JSON object (`columns` list and
    /// `query` dict get CheckMK's encoding), body is sent for POST/PUT/PATCH when Some.
    pub async fn request(
        &self,
        method: Method,
        endpoint: &str,
        params: Option<&Value>,
        body: Option<&Value>,
        headers: &[(String, String)],
        opts: RequestOptions,
    ) -> Result<ApiResponse, ApiError> {
        crate::paths::check_endpoint(endpoint)?;
        let mut url = if opts.use_api_prefix {
            format!("{}/{endpoint}", self.api_base_url)
        } else {
            format!("{}/{}/{endpoint}", self.config.server_url, self.config.site)
        };
        let query = params.map(query_string).unwrap_or_default();
        if !query.is_empty() {
            url = format!("{url}?{query}");
        }
        let header_map = self.headers(headers)?;
        let http = if opts.follow_redirects {
            &self.http
        } else {
            &self.http_no_redirect
        };
        let verb = match method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
            Method::Patch => reqwest::Method::PATCH,
        };
        let sends_body = matches!(method, Method::Post | Method::Put | Method::Patch);

        let mut retry_count = 0u32;
        loop {
            // `Some({})` is an empty JSON object, not "no body".
            let mut req = self
                .auth(http.request(verb.clone(), &url))
                .headers(header_map.clone());
            if let (true, Some(b)) = (sends_body, body) {
                req = req.body(b.to_string());
            }
            let response = req.send().await.map_err(|e| transport_error(&e))?;
            let status = response.status();
            let reply_headers = collect_headers(response.headers());
            if status.is_redirection() && !opts.follow_redirects {
                return Ok(ApiResponse {
                    status: status.as_u16(),
                    data: Value::Object(Default::default()),
                    raw: String::new(),
                    headers: reply_headers,
                });
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let text = response.text().await.map_err(|e| transport_error(&e))?;
            if status.is_success() {
                let data = if text.is_empty() {
                    Value::Object(Default::default())
                } else {
                    serde_json::from_str(&text).map_err(|e| {
                        ApiError::new(ApiErrorKind::Api, format!("Invalid JSON response: {e}"))
                            .with_status(status.as_u16())
                            .with_data(serde_json::json!({ "raw": text }))
                    })?
                };
                return Ok(ApiResponse {
                    status: status.as_u16(),
                    data,
                    raw: text,
                    headers: reply_headers,
                });
            }
            if RETRYABLE_STATUS.contains(&status.as_u16())
                && is_idempotent(method)
                && retry_count < self.config.max_retries
            {
                tokio::time::sleep(retry_delay(retry_after.as_deref(), retry_count)).await;
                retry_count += 1;
                continue;
            }
            // E5: a DELETE is only replayed after a 5xx/429, which does not say whether
            // the first attempt went through. A 404 on the replay means the object is
            // gone, which is what was asked: the delete succeeded.
            if status.as_u16() == 404 && retry_count > 0 && matches!(method, Method::Delete) {
                tracing::warn!(
                    "DELETE {endpoint}: 404 on the retry, the first attempt had deleted it"
                );
                return Ok(ApiResponse {
                    status: 204,
                    data: Value::Object(Default::default()),
                    raw: String::new(),
                    headers: reply_headers,
                });
            }
            let reason = status.canonical_reason().unwrap_or("");
            let data = serde_json::from_str(&text)
                .unwrap_or_else(|_| serde_json::json!({ "error": reason }));
            return Err(http_error(status.as_u16(), reason, data));
        }
    }

    pub async fn get(
        &self,
        endpoint: &str,
        params: Option<&Value>,
        headers: &[(String, String)],
    ) -> Result<ApiResponse, ApiError> {
        self.request(
            Method::Get,
            endpoint,
            params,
            None,
            headers,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn post(
        &self,
        endpoint: &str,
        body: Option<&Value>,
        headers: &[(String, String)],
    ) -> Result<ApiResponse, ApiError> {
        self.request(
            Method::Post,
            endpoint,
            None,
            body,
            headers,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn put(
        &self,
        endpoint: &str,
        body: Option<&Value>,
        headers: &[(String, String)],
    ) -> Result<ApiResponse, ApiError> {
        self.request(
            Method::Put,
            endpoint,
            None,
            body,
            headers,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn delete(
        &self,
        endpoint: &str,
        params: Option<&Value>,
        headers: &[(String, String)],
    ) -> Result<ApiResponse, ApiError> {
        self.request(
            Method::Delete,
            endpoint,
            params,
            None,
            headers,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn patch(
        &self,
        endpoint: &str,
        body: Option<&Value>,
        headers: &[(String, String)],
    ) -> Result<ApiResponse, ApiError> {
        self.request(
            Method::Patch,
            endpoint,
            None,
            body,
            headers,
            RequestOptions::default(),
        )
        .await
    }
}
