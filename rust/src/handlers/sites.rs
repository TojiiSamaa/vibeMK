//! Port of src/vibemk/handlers/sites.py (SitesHandler).
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::{path_segment, UnsafePathError};

enum Fail {
    Api(ApiError),
    Path(UnsafePathError),
}
impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}
impl From<UnsafePathError> for Fail {
    fn from(e: UnsafePathError) -> Self {
        Self::Path(e)
    }
}

fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

pub struct SitesHandler {
    base: Base,
}

impl SitesHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn get_sites(&self) -> Result<Vec<Value>, Fail> {
        let data = self
            .base
            .client
            .get("domain-types/site_connection/collections/all", None, &[])
            .await?
            .data;
        let sites = data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if sites.is_empty() {
            return Ok(text("🌐 No sites found.".into()));
        }
        let mut lines = vec![format!("🌐 **Monitoring Sites ({})**\n", sites.len())];
        for site in &sites {
            let site_id = site.get("id").map_or_else(|| "?".to_string(), ps);
            let title = site.get("title").cloned().unwrap_or_else(|| json!(""));
            let ext = site.get("extensions").unwrap_or(&Value::Null);
            let alias = ext
                .pointer("/basic_settings/alias")
                .map_or_else(|| ps(&title), ps);
            let status = if truthy(ext.get("logged_in")) {
                "🟢 connected"
            } else {
                "🔴 not connected"
            };
            let cfg = ext.get("configuration_connection");
            let url = cfg.and_then(|c| c.get("url_of_remote_site"));
            let replication = truthy(cfg.and_then(|c| c.get("enable_replication")));
            let mut line = format!("• **{site_id}** — {alias} [{status}]");
            if truthy(url) {
                line.push_str(&format!("\n  URL: {}", ps(url.unwrap_or(&Value::Null))));
            }
            line.push_str(&format!(
                "\n  Replication: {}",
                if replication { "enabled" } else { "disabled" }
            ));
            lines.push(line);
        }
        Ok(text(lines.join("\n")))
    }

    async fn login_site(&self, args: &Value) -> Result<Vec<Value>, Fail> {
        let (site_id, username, password) =
            (s(args, "site_id"), s(args, "username"), s(args, "password"));
        if site_id.is_empty() || username.is_empty() || password.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("site_id, username and password are required"),
            ));
        }
        let endpoint = format!(
            "objects/site_connection/{}/actions/login/invoke",
            path_segment(site_id)?
        );
        self.base
            .client
            .post(
                &endpoint,
                Some(&json!({"username": username, "password": password})),
                &[],
            )
            .await?;
        Ok(text(format!("✅ Logged in to site '{site_id}'.")))
    }

    async fn logout_site(&self, args: &Value) -> Result<Vec<Value>, Fail> {
        let site_id = s(args, "site_id");
        if site_id.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("site_id is required")));
        }
        let endpoint = format!(
            "objects/site_connection/{}/actions/logout/invoke",
            path_segment(site_id)?
        );
        self.base
            .client
            .post(&endpoint, Some(&json!({})), &[])
            .await?;
        Ok(text(format!("✅ Logged out of site '{site_id}'.")))
    }
}

#[async_trait]
impl Handler for SitesHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_sites" => self.get_sites().await,
            "vibemk_login_site" => self.login_site(args).await,
            "vibemk_logout_site" => self.logout_site(args).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|f| match f {
            Fail::Api(e) => self
                .base
                .error_response("CheckMK API Error", Some(&e.to_string())),
            Fail::Path(e) => self
                .base
                .error_response("Unexpected Error", Some(&e.to_string())),
        })
    }
}
