//! Port of src/vibemk/handlers/notifications.py (NotificationHandler).
//!
//! Endpoints follow CheckMK 2.4: deletion is a POST action, no If-Match is needed. `Err` from the
//! client is the Python `CheckMKError`; an unsafe rule id is the outer "Unexpected Error".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::{path_segment, UnsafePathError};

const COLLECTION: &str = "domain-types/notification_rule/collections/all";

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
/// `args.get(k)` when truthy (Python `if not x`).
fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| truthy(v))
}
/// `d.get(k) or fallback`, as text.
fn or_text(v: Option<&Value>, fallback: &str) -> String {
    v.filter(|v| truthy(v))
        .map_or_else(|| fallback.to_string(), ps)
}
fn object(rule_id: &str) -> Result<String, UnsafePathError> {
    Ok(format!(
        "objects/notification_rule/{}",
        path_segment(rule_id)?
    ))
}

pub struct NotificationHandler {
    base: Base,
}

impl NotificationHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn list_rules(&self) -> Result<Vec<Value>, Fail> {
        let result = self.base.client.get(COLLECTION, None, &[]).await?;
        let rules = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rules.is_empty() {
            return Ok(self.base.info_response(
                "No notification rules configured",
                Some(&json!({"hint": "CheckMK falls back to its built-in notification behaviour"})),
            ));
        }
        let mut lines = vec![
            format!("📢 **Notification Rules** ({})", rules.len()),
            String::new(),
        ];
        for rule in &rules {
            let config = rule.pointer("/extensions/rule_config");
            let description = or_text(
                config.and_then(|c| c.pointer("/rule_properties/description")),
                "(no description)",
            );
            let state = if config.and_then(|c| c.get("disabled")).is_some_and(truthy) {
                "disabled"
            } else {
                "enabled"
            };
            let plugin = config
                .and_then(|c| c.pointer("/notification_method/notify_plugin/option"))
                .map_or_else(|| "unknown".to_string(), ps);
            let id = rule.get("id").map_or_else(|| "?".to_string(), ps);
            lines.push(format!(
                "• **{description}** — id `{id}`, {state}, via `{plugin}`"
            ));
        }
        Ok(vec![json!({"type": "text", "text": lines.join("\n")})])
    }

    async fn show_rule(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(rule_id) = arg(a, "rule_id").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        let result = self.base.client.get(&object(&rule_id)?, None, &[]).await?;
        let config = result
            .data
            .pointer("/extensions/rule_config")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let description = or_text(
            config.pointer("/rule_properties/description"),
            "(no description)",
        );
        // ensure_ascii=False, indent=2: what serde_json prints.
        let pretty = serde_json::to_string_pretty(&config).unwrap_or_default();
        Ok(vec![json!({"type": "text", "text": format!(
            "📢 **Notification Rule `{rule_id}`**\n\nDescription: {description}\n\nFull rule_config — reuse this structure when creating or updating a rule:\n\n```json\n{pretty}\n```"
        )})])
    }

    async fn create_rule(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(rule_config) = arg(a, "rule_config") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_config is required")));
        };
        let result = self
            .base
            .client
            .post(COLLECTION, Some(&json!({"rule_config": rule_config})), &[])
            .await?;
        let rule_id = result
            .data
            .get("id")
            .cloned()
            .unwrap_or_else(|| json!("unknown"));
        Ok(self.base.success_response(
            "Notification rule created",
            Some(&json!({"rule_id": rule_id, "next step": "activate changes to apply it"})),
        ))
    }

    async fn update_rule(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(rule_id) = arg(a, "rule_id").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        let Some(rule_config) = arg(a, "rule_config") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_config is required")));
        };
        self.base
            .client
            .put(
                &object(&rule_id)?,
                Some(&json!({"rule_config": rule_config})),
                &[],
            )
            .await?;
        Ok(self.base.success_response(
            &format!("Notification rule '{rule_id}' updated"),
            Some(&json!({"next step": "activate changes to apply it"})),
        ))
    }

    async fn delete_rule(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(rule_id) = arg(a, "rule_id").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        // CheckMK models deletion as an action, not as an HTTP DELETE.
        self.base
            .client
            .post(
                &format!("{}/actions/delete/invoke", object(&rule_id)?),
                None,
                &[],
            )
            .await?;
        Ok(self.base.success_response(
            &format!("Notification rule '{rule_id}' deleted"),
            Some(&json!({"next step": "activate changes to apply it"})),
        ))
    }
}

#[async_trait]
impl Handler for NotificationHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_notification_rules" => self.list_rules().await,
            "vibemk_get_notification_rule" => self.show_rule(args).await,
            "vibemk_create_notification_rule" => self.create_rule(args).await,
            "vibemk_update_notification_rule" => self.update_rule(args).await,
            "vibemk_delete_notification_rule" => self.delete_rule(args).await,
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
