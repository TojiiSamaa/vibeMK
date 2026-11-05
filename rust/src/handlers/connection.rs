//! Port of src/vibemk/handlers/connection.py (ConnectionHandler).
//!
//! `Err` from the client is the Python `CheckMKError`; the `if not success` branches are dead there too.
//! AUDIT S4: the direct-URL filter also refuses a backslash and a double-encoded path.
//! SSRF: `vibemk_test_direct_url` carries the CheckMK credentials, so it only ever calls the configured
//! API host (same scheme, host and port), under the API path, without following a redirect. The URL that
//! is checked is the URL that is sent (parsed once), so a parser difference cannot slip a foreign host in.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Url;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

pub struct ConnectionHandler {
    base: Base,
}

fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

/// `json.dumps(v, indent=2)`: ensure_ascii escapes everything above U+007F.
fn json_dumps_indent2(v: &Value) -> String {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = String::with_capacity(pretty.len());
    let mut buf = [0u16; 2];
    for c in pretty.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            c.encode_utf16(&mut buf)
                .iter()
                .for_each(|u| out.push_str(&format!("\\u{u:04x}")));
        }
    }
    out
}

/// urllib.parse.unquote: decode %XX, leave malformed escapes as they are.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is there a `%XX` escape left in `s`?
fn has_escape(s: &str) -> bool {
    s.as_bytes()
        .windows(3)
        .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit())
}

fn origin(u: &Url) -> (String, Option<String>, Option<u16>) {
    (
        u.scheme().to_ascii_lowercase(),
        u.host_str().map(str::to_ascii_lowercase),
        u.port_or_known_default(),
    )
}

/// The URL to send when `test_url` lies on the CheckMK API; else why not.
fn check_url(api_base_url: &str, test_url: &str) -> Result<Url, String> {
    let not_api = || format!("Only URLs on the configured CheckMK API are allowed: {api_base_url}");
    let base = Url::parse(api_base_url).map_err(|_| not_api())?;
    // Before parsing: the parser turns `\` into `/` for http(s), which the server may not.
    if test_url.contains('\\') {
        return Err("Backslashes in the path are not allowed".into());
    }
    let target = Url::parse(test_url).map_err(|_| not_api())?;
    if origin(&target) != origin(&base) || !matches!(target.scheme(), "http" | "https") {
        return Err(not_api());
    }
    if !target.username().is_empty() || target.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }
    // The path as the server will resolve it: decoded once, `..` refused, a second layer of encoding refused.
    let decoded = unquote(target.path());
    if decoded.contains('\\') {
        return Err("Backslashes in the path are not allowed".into());
    }
    if has_escape(&decoded) {
        return Err("Double-encoded paths are not allowed".into());
    }
    if decoded.split('/').any(|s| s == "..") {
        return Err("Path segments '..' are not allowed".into());
    }
    let collapsed = decoded
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let path = format!("/{collapsed}");
    let base_path = base.path().trim_end_matches('/');
    if path != base_path && !path.starts_with(&format!("{base_path}/")) {
        return Err(format!(
            "Only URLs under the CheckMK API are allowed: {api_base_url}"
        ));
    }
    Ok(target)
}

impl ConnectionHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn c(&self) -> &CheckMkClient {
        &self.base.client
    }

    async fn debug_connection(&self) -> Vec<Value> {
        let cfg = &self.c().config;
        match self.c().get("version", None, &[]).await {
            Ok(r) => text(format!(
                "✅ **CheckMK Connection Successful**\n\n🌐 Server: {}\n🏢 Site: {}\n👤 User: {}\n🔒 SSL Verify: {}\n🔗 API Base URL: {}\n📊 Version: {}\n📦 Edition: {}",
                cfg.server_url,
                cfg.site,
                cfg.username,
                if cfg.verify_ssl { "True" } else { "False" },
                self.c().api_base_url,
                r.data.pointer("/versions/checkmk").map_or_else(|| "Unknown".to_string(), ps),
                r.data.get("edition").map_or_else(|| "Unknown".to_string(), ps),
            )),
            Err(e) => self.base.error_response("Connection Failed", Some(&format!("Error: {e}"))),
        }
    }

    fn debug_url_detection(&self) -> Vec<Value> {
        let cfg = &self.c().config;
        text(format!(
            "🔍 **URL Detection Debug Results**\n\n🌐 Server URL: {}\n🏢 Site: {}\n🔗 Selected API URL: {}\n\n**Test Results:**\n{}",
            cfg.server_url,
            cfg.site,
            self.c().api_base_url,
            self.c().get_debug_results().join("\n")
        ))
    }

    async fn test_direct_url(&self, args: &Value) -> Vec<Value> {
        let test_url = args.get("test_url").and_then(Value::as_str).unwrap_or("");
        if test_url.is_empty() {
            return self
                .base
                .error_response("Missing URL", Some("test_url parameter is required"));
        }
        let target = match check_url(&self.c().api_base_url, test_url) {
            Ok(t) => t,
            Err(refusal) => return self.base.error_response("URL not allowed", Some(&refusal)),
        };
        let failed = |e: &dyn std::fmt::Display| {
            self.base.error_text(&format!(
                "❌ **Request Failed**\n\nURL: {test_url}\nError: {e}"
            ))
        };
        let cfg = &self.c().config;
        let http = match reqwest::Client::builder()
            .danger_accept_invalid_certs(!cfg.verify_ssl)
            .timeout(std::time::Duration::from_secs(cfg.timeout))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(h) => h,
            Err(e) => return failed(&e),
        };
        let sent = http
            .get(target)
            .basic_auth(&cfg.username, Some(&cfg.password))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                format!("vibeMK/{}", crate::config::MCP_VERSION),
            )
            .send()
            .await;
        let response = match sent {
            Ok(r) => r,
            Err(e) => return failed(&e),
        };
        let status = response.status();
        let reason = status.canonical_reason().unwrap_or("");
        let body = response.text().await;
        if status.is_success() {
            let body = match body {
                Ok(b) => b,
                Err(e) => return failed(&e),
            };
            let parsed = if body.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&body).unwrap_or_else(|_| json!({"raw": body}))
            };
            return text(format!(
                "✅ **Direct URL Test Successful**\n\nURL: {test_url}\nStatus: {}\nResponse: {}",
                status.as_u16(),
                json_dumps_indent2(&parsed)
            ));
        }
        // A body that cannot be read or parsed falls back to the reason (Python: bare except).
        let error_data = body
            .ok()
            .and_then(|b| serde_json::from_str::<Value>(&b).ok())
            .unwrap_or_else(|| json!({"error": reason}));
        self.base.error_text(&format!(
            "❌ **HTTP Error {}**\n\nURL: {test_url}\nError: {reason}\nResponse: {}",
            status.as_u16(),
            json_dumps_indent2(&error_data)
        ))
    }

    async fn test_all_endpoints(&self) -> Vec<Value> {
        let endpoints = [
            ("version", "Version info"),
            ("domain-types/host_config/collections/all", "Host Configs"),
            ("domain-types/service/collections/all", "Services"),
            ("domain-types/folder_config/collections/all", "Folders"),
            ("domain-types/downtime/collections/all", "Downtimes"),
            (
                "domain-types/acknowledge/collections/all",
                "Acknowledgments",
            ),
            ("domain-types/activation_run/collections/all", "Activations"),
            ("domain-types/user_config/collections/all", "Users"),
            (
                "domain-types/host_group_config/collections/all",
                "Host Groups",
            ),
            (
                "domain-types/service_group_config/collections/all",
                "Service Groups",
            ),
        ];
        let mut results = Vec::new();
        for (endpoint, desc) in endpoints {
            results.push(match self.c().get(endpoint, None, &[]).await {
                Ok(r) => format!("✅ {endpoint} - {desc} (HTTP {})", r.status),
                Err(e) => format!("❌ {endpoint} - {desc} (Error: {e})"),
            });
        }
        text(format!(
            "🧪 **API Endpoint Test Results**\n\n{}",
            results.join("\n")
        ))
    }

    async fn get_version(&self) -> Result<Vec<Value>, ApiError> {
        let r = self.c().get("version", None, &[]).await?;
        let cfg = &self.c().config;
        Ok(text(format!(
            "📋 **CheckMK Version Information**\n\nVersion: {}\nEdition: {}\nSite: {}\nServer: {}",
            r.data
                .pointer("/versions/checkmk")
                .map_or_else(|| "Unknown".to_string(), ps),
            r.data
                .get("edition")
                .map_or_else(|| "Unknown".to_string(), ps),
            cfg.site,
            cfg.server_url
        )))
    }
}

#[async_trait]
impl Handler for ConnectionHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        match tool {
            "vibemk_debug_checkmk_connection" => self.debug_connection().await,
            "vibemk_debug_url_detection" => self.debug_url_detection(),
            "vibemk_test_direct_url" => self.test_direct_url(args).await,
            "vibemk_test_all_endpoints" => self.test_all_endpoints().await,
            "vibemk_get_checkmk_version" => match self.get_version().await {
                Ok(v) => v,
                Err(e) => self
                    .base
                    .error_response("CheckMK API Error", Some(&e.to_string())),
            },
            _ => self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            ),
        }
    }
}
