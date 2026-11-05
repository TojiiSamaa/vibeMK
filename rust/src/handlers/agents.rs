//! Port of src/vibemk/handlers/agents.py (AgentHandler): agent bakery, commercial editions only.

use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use serde_json::{json, Value};

use crate::client::{ApiError, ApiErrorKind, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

/// The Raw/Community edition has no bakery: its endpoints answer a bare 404.
const NO_BAKERY: &str = "This CheckMK edition has no agent bakery -- it is part of the commercial editions only. On Raw/Community, install the vanilla agent from Setup > Agents > Linux / Windows.";

/// Python `str()`.
fn txt(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// urllib.parse.urlencode of the pairs (quote_plus).
fn urlencode(pairs: &[(&str, &str)]) -> String {
    let quote = |t: &str| {
        t.bytes().fold(String::new(), |mut o, b| {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    o.push(b as char)
                }
                b' ' => o.push('+'),
                _ => write!(o, "%{b:02X}").unwrap_or_default(),
            }
            o
        })
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote(k), quote(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub struct AgentHandler {
    base: Base,
}

impl AgentHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn bake_agents(&self) -> Result<Vec<Value>, ApiError> {
        self.base
            .client
            .post("domain-types/agent/actions/bake/invoke", None, &[])
            .await?;
        Ok(vec![
            json!({"type": "text", "text": "🍞 **Agent Baking Started**\n\nUse vibemk_baking_status to check progress."}),
        ])
    }

    async fn baking_status(&self) -> Result<Vec<Value>, ApiError> {
        let data = self
            .base
            .client
            .get("domain-types/agent/actions/baking_status/invoke", None, &[])
            .await?
            .data;
        // The job sits in result.value; reading the top level found no state at all.
        let job = data
            .pointer("/result/value")
            .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
            .unwrap_or(&data);
        let mut lines = vec![
            "🍞 **Agent Baking Status**\n".to_string(),
            format!(
                "State: {}",
                job.get("state").map_or_else(|| "unknown".into(), txt)
            ),
        ];
        if let Some(t) = job
            .get("started")
            .and_then(Value::as_f64)
            .and_then(|t| DateTime::from_timestamp(t as i64, 0))
        {
            lines.push(format!("Started: {} UTC", t.format("%Y-%m-%d %H:%M:%S")));
        }
        if let Some(d) = job.get("duration").and_then(Value::as_f64) {
            lines.push(format!("Duration: {d:.1}s"));
        }
        for (label, key) in [("Result", "JobResult"), ("Error", "JobException")] {
            let entries = job
                .pointer(&format!("/loginfo/{key}"))
                .and_then(Value::as_array);
            lines.extend(
                entries
                    .into_iter()
                    .flatten()
                    .map(|l| format!("{label}: {}", txt(l))),
            );
        }
        Ok(vec![json!({"type": "text", "text": lines.join("\n")})])
    }

    fn download_agent_by_host(&self, args: &Value) -> Vec<Value> {
        let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
        let host_name = arg("host_name").map_or_else(String::new, txt);
        let os_type = arg("os_type").map_or_else(|| "linux_deb".to_string(), txt);
        if host_name.is_empty() {
            return self
                .base
                .error_response("Missing parameter", Some("host_name is required"));
        }
        // The API returns the binary directly, so only the URL can be given back.
        // agent_type=host_name asks for the agent baked for this host, not the vanilla one.
        let query = urlencode(&[
            ("os_type", &os_type),
            ("host_name", &host_name),
            ("agent_type", "host_name"),
        ]);
        let url = format!(
            "{}/domain-types/agent/actions/download_by_host/invoke?{query}",
            self.base.client.api_base_url
        );
        vec![json!({"type": "text", "text": format!(
            "📦 **Agent Download URL**\n\nHost: {host_name}\nOS type: {os_type}\nURL: {url}\n\nUse curl with your bearer token to download:\n  curl -H 'Authorization: Bearer USER PASS' '{url}' -o agent.{os_type}\n\nℹ️ Commercial editions only; bake first with vibemk_bake_agents if the host's agent changed.")})]
    }
}

#[async_trait]
impl Handler for AgentHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_bake_agents" => self.bake_agents().await,
            "vibemk_baking_status" => self.baking_status().await,
            "vibemk_download_agent_by_host" => Ok(self.download_agent_by_host(args)),
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|e| match e.kind {
            ApiErrorKind::NotFound => self
                .base
                .error_response("Agent bakery not available", Some(NO_BAKERY)),
            _ => self
                .base
                .error_response("CheckMK API Error", Some(&e.to_string())),
        })
    }
}
