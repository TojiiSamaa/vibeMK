//! Port of src/vibemk/handlers/audit_log.py (AuditLogHandler): CheckMK configuration change history.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{Local, TimeZone};
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

/// Python `str()`.
fn txt(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// Python truthiness of a present, non-null value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Null => false,
    }
}

pub struct AuditLogHandler {
    base: Base,
}

impl AuditLogHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn get_audit_log(&self, args: &Value) -> Result<Vec<Value>> {
        let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
        let on = |k: &str| arg(k).filter(|v| truthy(v));
        let date = on("date").map_or_else(|| Local::now().format("%Y-%m-%d").to_string(), txt);
        let limit = match arg("limit") {
            None => 50,
            Some(Value::String(t)) => t.trim().parse::<i64>().map_err(|_| {
                anyhow!(
                    "invalid literal for int() with base 10: {}",
                    py_repr(&json!(t))
                )
            })?,
            Some(v) => v.as_f64().map_or(50, |f| f as i64),
        };

        let mut params = json!({"date": date});
        for k in ["object_type", "object_id", "user_id", "regexp"] {
            if let Some(v) = on(k) {
                params[k] = v.clone();
            }
        }
        let result = self
            .base
            .client
            .get("domain-types/audit_log/collections/all", Some(&params), &[])
            .await?;

        let entries = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if entries.is_empty() {
            return Ok(vec![
                json!({"type": "text", "text": format!("📋 No audit log entries for {date}.")}),
            ]);
        }
        let mut lines = vec![format!(
            "📋 **Audit Log — {date} ({} entries)**\n",
            entries.len()
        )];
        // Python slice semantics: a negative limit drops entries from the end.
        let shown = if limit < 0 {
            entries.len().saturating_sub(limit.unsigned_abs() as usize)
        } else {
            entries.len().min(limit as usize)
        };
        for entry in &entries[..shown] {
            let ext = |k: &str| {
                entry
                    .pointer(&format!("/extensions/{k}"))
                    .filter(|v| !v.is_null())
            };
            let time_str = ext("time")
                .and_then(Value::as_f64)
                .filter(|t| *t != 0.0)
                .and_then(|t| Local.timestamp_opt(t as i64, 0).single())
                .map_or_else(|| "?".to_string(), |t| t.format("%H:%M:%S").to_string());
            let or_q = |k: &str| ext(k).map_or_else(|| "?".to_string(), txt);
            let obj_type = ext("object_type").map_or_else(String::new, txt);
            let obj_name = ext("object_name").map_or_else(String::new, txt);
            let title = entry
                .get("title")
                .filter(|v| !v.is_null())
                .map_or_else(String::new, txt);
            lines.push(format!(
                "• `{time_str}` **{}** — {}{}{}",
                or_q("user_id"),
                or_q("action"),
                if obj_type.is_empty() {
                    String::new()
                } else {
                    format!(" [{obj_type}: {obj_name}]")
                },
                if title.is_empty() {
                    String::new()
                } else {
                    format!("\n  {title}")
                },
            ));
        }
        if entries.len() as i64 > limit {
            lines.push(format!(
                "\n…{} more entries (raise limit to see them).",
                entries.len() as i64 - limit
            ));
        }
        Ok(vec![json!({"type": "text", "text": lines.join("\n")})])
    }
}

#[async_trait]
impl Handler for AuditLogHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        if tool != "vibemk_get_audit_log" {
            return self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            );
        }
        self.get_audit_log(args)
            .await
            .unwrap_or_else(|e| match e.downcast_ref::<ApiError>() {
                Some(api) => self
                    .base
                    .error_response("CheckMK API Error", Some(&api.to_string())),
                None => {
                    tracing::error!("Error in {tool}: {e}");
                    self.base
                        .error_response("Unexpected Error", Some(&e.to_string()))
                }
            })
    }
}
