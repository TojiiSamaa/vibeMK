//! Port of src/vibemk/handlers/base.py.
//!
//! CONTRACT FOR HANDLER PORTERS
//! - One file = one handler: src/handlers/<name>.rs ports src/vibemk/handlers/<name>.py
//!   and touches nothing else (registry.rs, Cargo.toml, base.rs, client.rs are frozen).
//! - Keep `pub struct XHandler { base: Base }`, `XHandler::new(Arc<CheckMkClient>)` and
//!   `impl Handler`; match on the tool name exactly as the Python `handle` does.
//! - Blocks stay JSON values like the Python dicts: {"type":"text","text":..},
//!   {"type":"_structured","data":..}, {"type":"_error"}. Build them with the Base helpers.
//! - Texts (emoji, markdown, wording, ordering) are byte-identical to the Python ones.
//! - Tests go in rust/tests/<name>.rs (`mod common;` then `common::fake_checkmk()`,
//!   wiremock) and replay the expectations of the matching Python tests in tests/.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::client::{ApiError, ApiResponse, CheckMkClient, Headers};

/// Block carrying machine-readable data; the dispatcher lifts it into structuredContent.
pub const STRUCTURED_BLOCK: &str = "_structured";
/// Block saying the call failed: the only thing the dispatcher reads to set isError.
pub const ERROR_BLOCK: &str = "_error";
/// Low on purpose: a production site has better things to do than answer one tool call.
pub const MAX_CONCURRENT_REQUESTS: usize = 4;
/// Activation runs, finished ones included; `extensions.is_running` tells them apart.
pub const ACTIVATION_RUNNING: &str = "domain-types/activation_run/collections/running";
/// How long an activation waits for a run already in progress, and how often it looks.
pub const ACTIVATION_RUNNING_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
pub const ACTIVATION_RUNNING_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// The two refusals of activate-changes that are not plain failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationRefusal {
    /// Other users have pending changes and `force_foreign_changes` was false.
    ForeignChanges,
    /// The site had nothing left to activate.
    NoChanges,
}

/// Measured on CheckMK 2.3.0p52 and 2.5.0p18: changes by another user without
/// force answer 401 (2.3) or 403 (2.5) with "There are changes from other users
/// and foreign changes are not allowed in this API call."; nothing to activate
/// answers 422 "Currently there are no changes to activate." The status alone
/// would read as a login or permission problem.
pub fn activation_refusal(error: &ApiError) -> Option<ActivationRefusal> {
    let detail = error
        .response_data
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    match error.status_code {
        Some(401 | 403) if detail.contains("changes from other users") => {
            Some(ActivationRefusal::ForeignChanges)
        }
        Some(422) if detail.contains("no changes to activate") => {
            Some(ActivationRefusal::NoChanges)
        }
        _ => None,
    }
}

#[async_trait]
pub trait Handler: Send + Sync {
    /// Handle a tool call and return the MCP content blocks.
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value>;
}

/// Python `repr()` of a JSON value (True/None, 'single quotes'): the wording of the Python messages.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::String(s) => {
            let q = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut body = String::new();
            for c in s.chars() {
                match c {
                    '\\' => body.push_str("\\\\"),
                    '\n' => body.push_str("\\n"),
                    '\r' => body.push_str("\\r"),
                    '\t' => body.push_str("\\t"),
                    c if c == q => {
                        body.push('\\');
                        body.push(c);
                    }
                    c => body.push(c),
                }
            }
            format!("{q}{body}{q}")
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(n) => n.to_string(),
    }
}

/// Python `str()` of a JSON value: a string stays as is.
fn py_str(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// Python truthiness: None, False, 0, "", [] and {} are falsy.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Render a Unix timestamp as "42s ago" / "5m ago" / "3h ago" / "2d ago" (base.py l.29).
/// None for missing or zero (CheckMK's "never happened"); an unreadable one comes back as is.
pub fn time_ago(timestamp: &Value) -> Option<String> {
    if !truthy(timestamp) {
        return None;
    }
    let as_secs = match timestamp {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(_) => Some(1.0),
        _ => None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let diff = match as_secs.map(|t| now - t).filter(|d| d.is_finite()) {
        Some(d) => (d as i64).max(0),
        None => return Some(py_str(timestamp)),
    };
    Some(match diff {
        d if d < 60 => format!("{d}s ago"),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86400 => format!("{}h ago", d / 3600),
        d => format!("{}d ago", d / 86400),
    })
}

/// Shared state and helpers of every handler (BaseHandler).
#[derive(Clone)]
pub struct Base {
    pub client: Arc<CheckMkClient>,
}

impl Base {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self { client }
    }

    /// Prose for the reader plus data for the model (base.py l.74).
    pub fn structured_response(&self, text: &str, data: Value) -> Vec<Value> {
        vec![
            json!({"type": "text", "text": text}),
            json!({"type": STRUCTURED_BLOCK, "data": data}),
        ]
    }

    /// Run `f` on each item, at most MAX_CONCURRENT_REQUESTS at once; results keep item order (l.88).
    pub async fn map_concurrently<T, R, F, Fut>(&self, items: Vec<T>, f: F) -> Vec<R>
    where
        F: Fn(T) -> Fut,
        Fut: Future<Output = R>,
    {
        stream::iter(items)
            .map(f)
            .buffered(MAX_CONCURRENT_REQUESTS)
            .collect()
            .await
    }

    /// If-Match from the object's current ETag, wildcard when it cannot be read (l.102).
    pub async fn if_match_header(&self, endpoint: &str) -> Headers {
        let etag = match self.client.get(endpoint, None, &[]).await {
            Ok(current) => Self::extract_etag(&current),
            Err(_) => "*".to_string(),
        };
        vec![("If-Match".to_string(), etag)]
    }

    /// ETag header, else data.extensions.meta_data.etag, else "*" (l.121).
    pub fn extract_etag(response: &ApiResponse) -> String {
        response
            .header("ETag")
            .filter(|e| !e.is_empty())
            .or_else(|| response.data.pointer("/extensions/meta_data/etag").and_then(Value::as_str).filter(|e| !e.is_empty()))
            .unwrap_or_else(|| {
                tracing::warn!("No ETag available: falling back to If-Match '*', concurrent edits will not be detected");
                "*"
            })
            .to_string()
    }

    /// Wait, at most `ACTIVATION_RUNNING_WAIT`, until no activation run is in progress.
    ///
    /// CheckMK 2.3 accepts a second activate-changes while one is running
    /// (HTTP 200, a new run id), but the site refuses it: the run ends at once
    /// as "Complete." with the per-site state "Locked -- cmkadmin is currently
    /// activating changes on this site", and the later changes stay pending.
    /// Write tools that activate on their own (set_process_thresholds) start
    /// such runs. The running collection still lists finished runs, so only
    /// `is_running` counts; an error reading it never blocks the activation.
    pub async fn wait_for_running_activation(&self) {
        let deadline = tokio::time::Instant::now() + ACTIVATION_RUNNING_WAIT;
        loop {
            let Ok(running) = self.client.get(ACTIVATION_RUNNING, None, &[]).await else {
                return;
            };
            let busy = running
                .data
                .get("value")
                .and_then(Value::as_array)
                .is_some_and(|runs| {
                    runs.iter().any(|run| {
                        run.pointer("/extensions/is_running") == Some(&Value::Bool(true))
                    })
                });
            if !busy || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(ACTIVATION_RUNNING_POLL).await;
        }
    }

    /// Activate pending changes, return one status line, never fail (l.132).
    pub async fn run_activation(&self) -> String {
        if self.client.config.never_activate_changes {
            return "🚫 Changes were not activated: NEVER_ACTIVATE_CHANGES is set.".to_string();
        }
        // Same guard as vibemk_activate_changes (see wait_for_running_activation).
        self.wait_for_running_activation().await;
        let pending = match self
            .client
            .get(
                "domain-types/activation_run/collections/pending_changes",
                None,
                &[],
            )
            .await
        {
            Ok(p) => p,
            Err(e) => return format!("⚠️ Activation failed: {e}"),
        };
        if !pending.data.pointer("/value").is_some_and(truthy) {
            return "ℹ️ No pending changes to activate.".to_string();
        }
        // S2 (AUDIT): a write tool never activates another user's changes.
        let body = json!({"redirect": false, "sites": [self.client.config.site], "force_foreign_changes": false});
        let headers: Headers = vec![("If-Match".to_string(), Self::extract_etag(&pending))];
        match self
            .client
            .post(
                "domain-types/activation_run/actions/activate-changes/invoke",
                Some(&body),
                &headers,
            )
            .await
        {
            Ok(_) => "✅ Activation started (CheckMK runs it in the background).".to_string(),
            Err(e) => match activation_refusal(&e) {
                Some(ActivationRefusal::ForeignChanges) => "⚠️ Not activated: other users have pending changes, and vibeMK does not activate them on its own. Review them with vibemk_get_pending_changes, then run vibemk_activate_changes with force_foreign_changes=true.".to_string(),
                Some(ActivationRefusal::NoChanges) => "ℹ️ No pending changes to activate.".to_string(),
                None => format!("⚠️ Activation failed: {e}"),
            },
        }
    }

    /// "✅ **message**" + formatted data (l.168).
    pub fn success_response(&self, message: &str, data: Option<&Value>) -> Vec<Value> {
        vec![json!({"type": "text", "text": self.with_data(format!("✅ **{message}**"), data)})]
    }

    /// "❌ **message**" + details, marked as an error (l.175).
    pub fn error_response(&self, message: &str, error_details: Option<&str>) -> Vec<Value> {
        let mut text = format!("❌ **{message}**");
        if let Some(d) = error_details.filter(|d| !d.is_empty()) {
            text.push_str("\n\n");
            text.push_str(d);
        }
        self.error_text(&text)
    }

    /// A failure in this exact wording; the marker block is what makes it one.
    pub fn error_text(&self, text: &str) -> Vec<Value> {
        vec![
            json!({"type": "text", "text": text}),
            json!({"type": ERROR_BLOCK}),
        ]
    }

    /// "ℹ️ **message**" + formatted data (l.186).
    pub fn info_response(&self, message: &str, data: Option<&Value>) -> Vec<Value> {
        vec![json!({"type": "text", "text": self.with_data(format!("ℹ️ **{message}**"), data)})]
    }

    fn with_data(&self, mut text: String, data: Option<&Value>) -> String {
        if let Some(d) = data.filter(|d| truthy(d)) {
            text.push_str(
                "

",
            );
            text.push_str(&self.format_data(d));
        }
        text
    }

    /// base.py l.193: dict -> "**key**: value" lines (lists counted, dicts "object"); else str().
    pub fn format_data(&self, data: &Value) -> String {
        match data {
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| match v {
                    Value::Array(a) => format!("**{k}**: {}", a.len()),
                    Value::Object(_) => format!("**{k}**: object"),
                    other => format!("**{k}**: {}", py_str(other)),
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                ),
            other => py_str(other),
        }
    }
}
