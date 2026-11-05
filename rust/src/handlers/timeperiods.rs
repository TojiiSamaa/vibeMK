//! Port of src/vibemk/handlers/timeperiods.py (TimePeriodsHandler).
//!
//! `Err` from the client is the Python `CheckMKError` (so the "not found" check of update/delete is a
//! GET that fails with "CheckMK API Error", as in Python); an unsafe name is the outer "Unexpected Error".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

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
/// `args.get(k)` unless absent or null (Python `is not None`).
fn given<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null())
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn len(v: Option<&Value>) -> usize {
    match v {
        Some(Value::Array(a)) => a.len(),
        Some(Value::Object(o)) => o.len(),
        _ => 0,
    }
}
/// Python `str.capitalize()`.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase()
            .chain(c.flat_map(char::to_lowercase))
            .collect()
    })
}
fn object(name: &str) -> Result<String, UnsafePathError> {
    Ok(format!("objects/time_period/{}", path_segment(name)?))
}

/// `_validate_time_range`: a dict with `day` and a `time_ranges` list of dicts with `start` and `end`.
fn valid_time_range(tr: &Value) -> bool {
    let (Some(_), Some(ranges)) = (tr.get("day"), tr.get("time_ranges")) else {
        return false;
    };
    ranges.as_array().is_some_and(|r| {
        r.iter()
            .all(|x| x.is_object() && x.get("start").is_some() && x.get("end").is_some())
    })
}

pub struct TimePeriodsHandler {
    base: Base,
}

impl TimePeriodsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn invalid_range(&self) -> Vec<Value> {
        self.base.error_response(
            "Invalid time range",
            Some("Time ranges must have 'day' and 'time_ranges' fields"),
        )
    }

    async fn get_timeperiods(&self) -> Result<Vec<Value>, Fail> {
        let result = self
            .base
            .client
            .get("domain-types/time_period/collections/all", None, &[])
            .await?;
        let periods = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if periods.is_empty() {
            return Ok(text(
                "⏰ **No Time Periods Found**\n\nNo time periods are configured.".into(),
            ));
        }
        let list: Vec<String> = periods
            .iter()
            .map(|p| {
                let id = p.get("id").map_or_else(|| "Unknown".to_string(), ps);
                let ext = p.get("extensions");
                let alias = ext
                    .and_then(|e| e.get("alias"))
                    .map_or_else(|| id.clone(), ps);
                let mut summary = format!(
                    "{} active ranges",
                    len(ext.and_then(|e| e.get("active_time_ranges")))
                );
                let exceptions = len(ext.and_then(|e| e.get("exceptions")));
                if exceptions > 0 {
                    summary.push_str(&format!(", {exceptions} exceptions"));
                }
                format!("⏰ **{id}** - {alias}\n   {summary}")
            })
            .collect();
        Ok(text(format!(
            "⏰ **Time Periods** ({} total):\n\n{}",
            periods.len(),
            list.join("\n\n")
        )))
    }

    async fn create_timeperiod(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let name = a.get("name").filter(|v| truthy(v)).map(ps);
        let alias = a.get("alias").filter(|v| truthy(v)).map(ps);
        let ranges = a
            .get("active_time_ranges")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some(name) = name else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        };
        let mut data = Map::new();
        data.insert("name".into(), json!(name));
        data.insert("active_time_ranges".into(), json!(ranges));
        if let Some(al) = &alias {
            data.insert("alias".into(), json!(al));
        }
        for k in ["exceptions", "exclude"] {
            if let Some(v) = a.get(k).filter(|v| truthy(v)) {
                data.insert(k.into(), v.clone());
            }
        }
        if !ranges.iter().all(valid_time_range) {
            return Ok(self.invalid_range());
        }
        self.base
            .client
            .post(
                "domain-types/time_period/collections/all",
                Some(&Value::Object(data)),
                &[],
            )
            .await?;
        let mut shown = Vec::new();
        for tr in &ranges {
            let day = tr.get("day").map_or_else(|| "unknown".to_string(), ps);
            for r in tr
                .get("time_ranges")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let field = |k: &str| r.get(k).map_or_else(String::new, ps);
                shown.push(format!(
                    "  • {}: {}-{}",
                    capitalize(&day),
                    field("start"),
                    field("end")
                ));
            }
        }
        Ok(text(format!(
            "✅ **Time Period Created Successfully**\n\nName: **{name}**\nAlias: {}\nActive time ranges:\n{}\n\n⚠️ **Remember to activate changes!**",
            alias.unwrap_or_else(|| name.clone()),
            shown.join("\n")
        )))
    }

    async fn update_timeperiod(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(name) = a.get("name").filter(|v| truthy(v)).map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        };
        let endpoint = object(&name)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        let mut data = Map::new();
        if let Some(v) = given(a, "alias") {
            data.insert("alias".into(), v.clone());
        }
        if let Some(v) = given(a, "active_time_ranges") {
            if !v.as_array().is_some_and(|r| r.iter().all(valid_time_range)) {
                return Ok(self.invalid_range());
            }
            data.insert("active_time_ranges".into(), v.clone());
        }
        for k in ["exceptions", "exclude"] {
            if let Some(v) = given(a, k) {
                data.insert(k.into(), v.clone());
            }
        }
        if data.is_empty() {
            return Ok(self.base.error_response(
                "No data to update",
                Some("At least one field must be provided"),
            ));
        }
        let headers = self.base.if_match_header(&endpoint).await;
        self.base
            .client
            .put(&endpoint, Some(&Value::Object(data.clone())), &headers)
            .await?;
        let fields: Vec<&str> = data.keys().map(String::as_str).collect();
        Ok(text(format!("✅ **Time Period Updated Successfully**\n\nName: {name}\nUpdated fields: {}\n\n⚠️ **Remember to activate changes!**", fields.join(", "))))
    }

    async fn delete_timeperiod(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(name) = a.get("name").filter(|v| truthy(v)).map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        };
        let endpoint = object(&name)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        let headers = self.base.if_match_header(&endpoint).await;
        self.base.client.delete(&endpoint, None, &headers).await?;
        Ok(text(format!(
            "✅ **Time Period Deleted Successfully**\n\nName: {name}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The time period is only marked for deletion until you activate changes!"
        )))
    }
}

#[async_trait]
impl Handler for TimePeriodsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_timeperiods" => self.get_timeperiods().await,
            "vibemk_create_timeperiod" => self.create_timeperiod(args).await,
            "vibemk_update_timeperiod" => self.update_timeperiod(args).await,
            "vibemk_delete_timeperiod" => self.delete_timeperiod(args).await,
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
