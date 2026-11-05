//! Port of src/vibemk/handlers/debug.py (DebugHandler).
//!
//! `Err` from the client is the Python exception (the Python client raises on non-2xx), so the
//! "success is false" branches cannot happen and are not ported. Python's `_test_all_host_endpoints`,
//! `_debug_host_data_structure` and `_debug_service_data_structure` are never dispatched by `handle`,
//! so they are not ported either.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::CheckMkClient;
use crate::handlers::base::{py_repr, Base, Handler};

pub struct DebugHandler {
    base: Base,
}

/// `list(d.keys())` as Python prints it.
fn keys_repr(map: &Map<String, Value>) -> String {
    py_repr(&Value::Array(
        map.keys().map(|k| Value::String(k.clone())).collect(),
    ))
}

/// `str(x)[:200]`.
fn clip(v: &Value, n: usize) -> String {
    py_repr(v).chars().take(n).collect()
}

/// `type(x)` as Python prints it.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "<class 'NoneType'>",
        Value::Bool(_) => "<class 'bool'>",
        Value::Number(n) if n.is_f64() => "<class 'float'>",
        Value::Number(_) => "<class 'int'>",
        Value::String(_) => "<class 'str'>",
        Value::Array(_) => "<class 'list'>",
        Value::Object(_) => "<class 'dict'>",
    }
}

impl DebugHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn debug_api_endpoints(&self) -> Vec<Value> {
        let mut results = Vec::new();
        for endpoint in [
            "domain-types",
            "domain-types/host",
            "domain-types/host/collections/all",
            "domain-types/service",
            "domain-types/service/collections/all",
            "objects",
        ] {
            results.push(match self.base.client.get(endpoint, None, &[]).await {
                Ok(r) => match &r.data {
                    Value::Object(data) => {
                        let first_items = ["value", "domain_type"]
                            .iter()
                            .find_map(|k| {
                                data.get(*k)
                                    .and_then(Value::as_array)
                                    .filter(|a| !a.is_empty())
                            })
                            .map_or_else(Vec::new, |a| a.iter().take(2).cloned().collect());
                        format!(
                            "✅ **{endpoint}**\n   Keys: {}\n   Sample: {}...",
                            keys_repr(data),
                            clip(&Value::Array(first_items), 200)
                        )
                    }
                    other => format!(
                        "✅ **{endpoint}**\n   Data type: {}\n   Content: {}...",
                        py_type(other),
                        clip(other, 200)
                    ),
                },
                Err(e) => format!("💥 **{endpoint}**\n   Exception: {e}"),
            });
        }
        vec![
            json!({"type": "text", "text": format!("🔍 **CheckMK API Endpoints Debug**\n\n{}", results.join("\n\n"))}),
        ]
    }

    async fn debug_permissions(&self) -> Vec<Value> {
        let mut results = Vec::new();
        for (description, endpoint) in [
            ("API Version", "version"),
            ("Domain Types", "domain-types"),
            ("User Info", "objects/user_config"),
        ] {
            results.push(format!("\n🧪 **{description}**"));
            results.push(match self.base.client.get(endpoint, None, &[]).await {
                Ok(_) => "   ✅ Access granted".to_string(),
                Err(e) => format!("   💥 Exception: {e}"),
            });
        }
        for (description, endpoint) in [
            ("Host Monitoring", "domain-types/host/collections/all"),
            ("Service Monitoring", "domain-types/service/collections/all"),
            ("Host Config", "domain-types/host_config/collections/all"),
        ] {
            results.push(format!("\n🔒 **{description} Permissions**"));
            results.push(match self.base.client.get(endpoint, None, &[]).await {
                Ok(r) => match &r.data {
                    Value::Object(data) => match data.get("value") {
                        Some(v) => format!(
                            "   ✅ Access granted - {} items",
                            v.as_array()
                                .map_or_else(|| "unknown".to_string(), |a| a.len().to_string())
                        ),
                        None => format!("   ✅ Access granted - structure: {}", keys_repr(data)),
                    },
                    other => format!("   ✅ Access granted - structure: {}", py_type(other)),
                },
                Err(e) => format!("   💥 Exception: {e}"),
            });
        }
        vec![
            json!({"type": "text", "text": format!("🔐 **Permissions Debug**\n{}", results.join("\n"))}),
        ]
    }
}

#[async_trait]
impl Handler for DebugHandler {
    async fn handle(&self, tool: &str, _args: &Value) -> Vec<Value> {
        match tool {
            "vibemk_debug_api_endpoints" => self.debug_api_endpoints().await,
            "vibemk_debug_permissions" => self.debug_permissions().await,
            _ => self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            ),
        }
    }
}
