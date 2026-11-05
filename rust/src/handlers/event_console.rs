//! Port of src/vibemk/handlers/event_console.py (EventConsoleHandler).
//!
//! `Err` from the client is the Python `CheckMKError`; an unsafe event id or a non-numeric id in
//! `event_ids` (Python `int()` raising) is the outer "Unexpected Error".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::{path_segment, UnsafePathError};

enum Fail {
    Api(ApiError),
    Other(String),
}
impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}
impl From<UnsafePathError> for Fail {
    fn from(e: UnsafePathError) -> Self {
        Self::Other(e.to_string())
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
/// `args.get(k)` when truthy.
fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| truthy(v))
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
/// The Event Console's numeric states (`_STATE`); anything else prints as is.
fn state_label(v: &Value) -> String {
    match v.as_f64() {
        Some(0.0) => "OK".into(),
        Some(1.0) => "⚠️ WARN".into(),
        Some(2.0) => "🔴 CRIT".into(),
        Some(3.0) => "❓ UNKNOWN".into(),
        _ => ps(v),
    }
}
/// Python `int(x)` on an event id.
fn to_int(v: &Value) -> Result<i64, Fail> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| Fail::Other(format!("cannot convert {} to int", ps(v)))),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| {
            Fail::Other(format!(
                "invalid literal for int() with base 10: {}",
                py_repr(v)
            ))
        }),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => Err(Fail::Other(format!(
            "int() argument must be a string or a number, not '{}'",
            if v.is_null() { "NoneType" } else { "list" }
        ))),
    }
}

pub struct EventConsoleHandler {
    base: Base,
}

impl EventConsoleHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    /// The site an event lives on: the one given, else the connected one.
    fn site(&self, a: &Value) -> String {
        arg(a, "site_id").map_or_else(|| self.base.client.config.site.clone(), ps)
    }

    async fn get_events(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let mut params = Map::new();
        for k in ["phase", "state", "host", "application", "site_id"] {
            if let Some(v) = arg(a, k) {
                params.insert(k.into(), v.clone());
            }
        }
        let result = self
            .base
            .client
            .get(
                "domain-types/event_console/collections/all",
                Some(&Value::Object(params)),
                &[],
            )
            .await?;
        let events = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if events.is_empty() {
            return Ok(text("📭 No Event Console events found.".into()));
        }
        let mut lines = vec![format!("🗃️ **Event Console — {} Events**\n", events.len())];
        for ev in events.iter().take(50) {
            let ext = ev.get("extensions").unwrap_or(ev);
            let get = |k: &str, fallback: &str| ext.get(k).map_or_else(|| fallback.to_string(), ps);
            let eid = ev
                .get("id")
                .or_else(|| ext.get("id"))
                .map_or_else(|| "?".to_string(), ps);
            let app = get("application", "");
            let body: String = get("text", "").chars().take(200).collect();
            let state = ext
                .get("state")
                .map_or_else(|| "OK".to_string(), state_label);
            let mut line = format!(
                "• **{eid}** [{state}] [{}] `{}`",
                get("phase", "?"),
                get("host", "?")
            );
            if !app.is_empty() {
                line.push_str(&format!(" — {app}"));
            }
            if !body.is_empty() {
                line.push_str(&format!("\n  {body}"));
            }
            lines.push(line);
        }
        if events.len() > 50 {
            lines.push(format!("\n…and {} more events.", events.len() - 50));
        }
        Ok(text(lines.join("\n")))
    }

    async fn acknowledge_event(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(event_id) = arg(a, "event_id").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("event_id is required")));
        };
        let comment = a
            .get("comment")
            .cloned()
            .unwrap_or_else(|| json!("Acknowledged via vibeMK"));
        let endpoint = format!(
            "objects/event_console/{}/actions/update_and_acknowledge/invoke",
            path_segment(&event_id)?
        );
        self.base
            .client
            .post(
                &endpoint,
                Some(&json!({"change_comment": comment, "site_id": self.site(a)})),
                &[],
            )
            .await?;
        Ok(text(format!("✅ Event {event_id} acknowledged.")))
    }

    async fn change_event_state(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let event_id = arg(a, "event_id").map(ps);
        let new_state = a.get("new_state").filter(|v| !v.is_null());
        let (Some(event_id), Some(new_state)) = (event_id, new_state) else {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("event_id and new_state are required"),
            ));
        };
        let endpoint = format!(
            "objects/event_console/{}/actions/change_state/invoke",
            path_segment(&event_id)?
        );
        self.base
            .client
            .post(
                &endpoint,
                Some(&json!({"new_state": new_state, "site_id": self.site(a)})),
                &[],
            )
            .await?;
        Ok(text(format!(
            "✅ Event {event_id}: state set to '{}'.",
            ps(new_state)
        )))
    }

    async fn delete_events(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let endpoint = "domain-types/event_console/actions/delete/invoke";
        let ids = arg(a, "event_ids").and_then(Value::as_array);
        // by_id deletes a single event, so a list of IDs is one call each.
        let count = if let Some(ids) = ids {
            for id in ids {
                let body = json!({"filter_type": "by_id", "site_id": self.site(a), "event_id": to_int(id)?});
                self.base.client.post(endpoint, Some(&body), &[]).await?;
            }
            ids.len().to_string()
        } else {
            let mut filters = Map::new();
            filters.insert(
                "phase".into(),
                a.get("phase").cloned().unwrap_or_else(|| json!("open")),
            );
            if let Some(h) = arg(a, "host") {
                filters.insert("host".into(), h.clone());
            }
            self.base
                .client
                .post(
                    endpoint,
                    Some(&json!({"filter_type": "params", "filters": filters})),
                    &[],
                )
                .await?;
            "All".to_string()
        };
        Ok(text(format!("✅ {count} event(s) deleted.")))
    }
}

#[async_trait]
impl Handler for EventConsoleHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_events" => self.get_events(args).await,
            "vibemk_acknowledge_event" => self.acknowledge_event(args).await,
            "vibemk_change_event_state" => self.change_event_state(args).await,
            "vibemk_delete_events" => self.delete_events(args).await,
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
            Fail::Other(m) => self.base.error_response("Unexpected Error", Some(&m)),
        })
    }
}
