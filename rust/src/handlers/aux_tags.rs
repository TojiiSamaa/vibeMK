//! Port of src/vibemk/handlers/aux_tags.py (AuxTagsHandler).
//!
//! `Err` from the client is the Python `CheckMKError`; an unsafe tag id is the outer "Unexpected Error".

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
/// A string argument, "" when absent or not a string (Python `.get(k, "")` on a string schema).
fn s<'a>(a: &'a Value, k: &str) -> &'a str {
    a.get(k).and_then(Value::as_str).unwrap_or("")
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

pub struct AuxTagsHandler {
    base: Base,
}

impl AuxTagsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn get_aux_tags(&self) -> Result<Vec<Value>, Fail> {
        let result = self
            .base
            .client
            .get("domain-types/aux_tag/collections/all", None, &[])
            .await?;
        let tags = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if tags.is_empty() {
            return Ok(text("🏷️ No auxiliary tags defined.".into()));
        }
        let mut lines = vec![format!("🏷️ **Aux Tags ({})**\n", tags.len())];
        for tag in &tags {
            let id = tag.get("id").map_or_else(|| "?".to_string(), ps);
            let title = tag.get("title").map_or_else(String::new, ps);
            let topic = tag
                .pointer("/extensions/topic")
                .map_or_else(String::new, ps);
            let mut line = format!("• **{id}** — {title}");
            if !topic.is_empty() {
                line.push_str(&format!(" [{topic}]"));
            }
            lines.push(line);
        }
        Ok(text(lines.join("\n")))
    }

    async fn create_aux_tag(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let (tag_id, title) = (s(a, "tag_id"), s(a, "title"));
        if tag_id.is_empty() || title.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("tag_id and title are required")));
        }
        let mut body = Map::new();
        body.insert("aux_tag_id".into(), json!(tag_id));
        body.insert("title".into(), json!(title));
        for k in ["topic", "help"] {
            if !s(a, k).is_empty() {
                body.insert(k.into(), json!(s(a, k)));
            }
        }
        self.base
            .client
            .post(
                "domain-types/aux_tag/collections/all",
                Some(&Value::Object(body)),
                &[],
            )
            .await?;
        Ok(text(format!(
            "✅ Auxiliary tag '{tag_id}' ({title}) created."
        )))
    }

    async fn update_aux_tag(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let tag_id = s(a, "tag_id");
        if tag_id.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("tag_id is required")));
        }
        let mut body = Map::new();
        for k in ["title", "topic", "help"] {
            if !s(a, k).is_empty() {
                body.insert(k.into(), json!(s(a, k)));
            }
        }
        if body.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("At least one field to update is required"),
            ));
        }
        let endpoint = format!("objects/aux_tag/{}", path_segment(tag_id)?);
        self.base
            .client
            .put(&endpoint, Some(&Value::Object(body)), &[])
            .await?;
        Ok(text(format!("✅ Auxiliary tag '{tag_id}' updated.")))
    }

    async fn delete_aux_tag(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let tag_id = s(a, "tag_id");
        if tag_id.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("tag_id is required")));
        }
        let endpoint = format!(
            "objects/aux_tag/{}/actions/delete/invoke",
            path_segment(tag_id)?
        );
        self.base
            .client
            .post(&endpoint, Some(&json!({})), &[])
            .await?;
        Ok(text(format!("✅ Auxiliary tag '{tag_id}' deleted.")))
    }
}

#[async_trait]
impl Handler for AuxTagsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_aux_tags" => self.get_aux_tags().await,
            "vibemk_create_aux_tag" => self.create_aux_tag(args).await,
            "vibemk_update_aux_tag" => self.update_aux_tag(args).await,
            "vibemk_delete_aux_tag" => self.delete_aux_tag(args).await,
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
