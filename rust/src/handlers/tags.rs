//! Port of src/vibemk/handlers/tags.py (TagsHandler).
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not result.get("success")`
//! branches (dead since client.py raises) are not ported: any API failure is "CheckMK API Error".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

enum Fail {
    Api(ApiError),
    Other(String),
}
impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}
type Out = Result<Vec<Value>, Fail>;

static NULL: Value = Value::Null;

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
fn arg<'a>(args: &'a Value, k: &str) -> &'a Value {
    args.get(k).unwrap_or(&NULL)
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn endpoint(tag_id: &str) -> Result<String, Fail> {
    path_segment(tag_id)
        .map(|s| format!("objects/host_tag_group/{s}"))
        .map_err(|e| Fail::Other(e.to_string()))
}

/// `for tag in tags: if not isinstance(tag, dict) or "id" not in tag or "title" not in tag`.
/// Ok(false) = invalid structure; a non-iterable (number, bool) is the Python TypeError.
fn tags_valid(tags: &Value) -> Result<bool, Fail> {
    match tags {
        Value::Array(a) => Ok(a
            .iter()
            .all(|t| t.get("id").is_some() && t.get("title").is_some())),
        // Iterating a str or a dict yields strings: never a valid tag.
        Value::String(_) | Value::Object(_) => Ok(false),
        Value::Number(n) => Err(Fail::Other(format!(
            "'{}' object is not iterable",
            if n.is_f64() { "float" } else { "int" }
        ))),
        _ => Err(Fail::Other("'bool' object is not iterable".into())),
    }
}

pub struct TagsHandler {
    base: Base,
}

impl TagsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn invalid_structure(&self) -> Vec<Value> {
        self.base.error_response(
            "Invalid tag structure",
            Some("Each tag must have 'id' and 'title' fields"),
        )
    }

    async fn get(&self) -> Out {
        let result = self
            .base
            .client
            .get("domain-types/host_tag_group/collections/all", None, &[])
            .await?;
        let groups = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if groups.is_empty() {
            return Ok(text(
                "🏷️ **No Host Tag Groups Found**\n\nNo host tag groups are configured.".to_string(),
            ));
        }
        let list: Vec<String> = groups
            .iter()
            .map(|g| {
                let id = g.get("id").map_or_else(|| "Unknown".to_string(), ps);
                let title = g
                    .pointer("/extensions/title")
                    .map_or_else(|| id.clone(), ps);
                let tags = g
                    .pointer("/extensions/tags")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                let names: Vec<String> = tags
                    .iter()
                    .take(3)
                    .map(|t| {
                        t.get("title")
                            .or_else(|| t.get("id"))
                            .map_or_else(|| "Unknown".to_string(), ps)
                    })
                    .collect();
                let mut preview = names.join(", ");
                if tags.len() > 3 {
                    preview.push_str(&format!(", ... (+{} more)", tags.len() - 3));
                }
                format!(
                    "🏷️ **{id}** - {title}\n   Tags: {preview}\n   Total: {} tags",
                    tags.len()
                )
            })
            .collect();
        Ok(text(format!(
            "🏷️ **Host Tag Groups** ({} total):\n\n{}",
            groups.len(),
            list.join("\n\n")
        )))
    }

    async fn create(&self, args: &Value) -> Out {
        let (tag_id, title) = (arg(args, "tag_id"), arg(args, "title"));
        let topic = args.get("topic").unwrap_or(&NULL);
        let tags = arg(args, "tags");
        let help = arg(args, "help");
        if !truthy(tag_id) || !truthy(title) {
            return Ok(self
                .base
                .error_response("Missing parameters", Some("tag_id and title are required")));
        }
        if !truthy(tags) {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("tags list is required (at least one tag)"),
            ));
        }
        if !tags_valid(tags)? {
            return Ok(self.invalid_structure());
        }
        // Werk 16364 removed the older "ident" spelling in 2.4.0b1; "id" covers every supported version.
        let mut body = json!({"id": tag_id, "title": title, "tags": tags});
        if truthy(topic) {
            body["topic"] = topic.clone();
        }
        if truthy(help) {
            body["help"] = help.clone();
        }
        self.base
            .client
            .post(
                "domain-types/host_tag_group/collections/all",
                Some(&body),
                &[],
            )
            .await?;
        let topic = if truthy(topic) {
            ps(topic)
        } else {
            "None".to_string()
        };
        let n = tags.as_array().map_or(0, Vec::len);
        Ok(text(format!("✅ **Host Tag Group Created Successfully**\n\nTag ID: {}\nTitle: {}\nTopic: {topic}\nTags: {n} configured\n\n⚠️ **Remember to activate changes!**", ps(tag_id), ps(title))))
    }

    async fn update(&self, args: &Value) -> Out {
        let tag_id = arg(args, "tag_id");
        if !truthy(tag_id) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("tag_id is required")));
        }
        let tag_id = ps(tag_id);
        let ep = endpoint(&tag_id)?;
        self.base.client.get(&ep, None, &[]).await?;

        let mut data = Map::new();
        let (title, topic, tags, help, repair) = (
            arg(args, "title"),
            arg(args, "topic"),
            arg(args, "tags"),
            arg(args, "help"),
            arg(args, "repair"),
        );
        if truthy(title) {
            data.insert("title".into(), title.clone());
        }
        if !topic.is_null() {
            data.insert("topic".into(), topic.clone());
        }
        if truthy(tags) {
            if !tags_valid(tags)? {
                return Ok(self.invalid_structure());
            }
            data.insert("tags".into(), tags.clone());
        }
        if !help.is_null() {
            data.insert("help".into(), help.clone());
        }
        if truthy(repair) {
            data.insert("repair".into(), repair.clone());
        }
        if data.is_empty() {
            return Ok(self.base.error_response(
                "No data to update",
                Some("At least one field must be provided"),
            ));
        }
        let fields = data.keys().cloned().collect::<Vec<_>>().join(", ");
        let headers = self.base.if_match_header(&ep).await;
        self.base
            .client
            .put(&ep, Some(&Value::Object(data)), &headers)
            .await?;
        Ok(text(format!("✅ **Host Tag Group Updated Successfully**\n\nTag ID: {tag_id}\nUpdated fields: {fields}\n\n⚠️ **Remember to activate changes!**")))
    }

    async fn delete(&self, args: &Value) -> Out {
        let (tag_id, repair) = (arg(args, "tag_id"), truthy(arg(args, "repair")));
        if !truthy(tag_id) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("tag_id is required")));
        }
        let tag_id = ps(tag_id);
        let ep = endpoint(&tag_id)?;
        self.base.client.get(&ep, None, &[]).await?;
        let params = repair.then(|| json!({"repair": "true"}));
        self.base.client.delete(&ep, params.as_ref(), &[]).await?;
        let mode = if repair { "Enabled" } else { "Disabled" };
        Ok(text(format!("✅ **Host Tag Group Deleted Successfully**\n\nTag ID: {tag_id}\nRepair mode: {mode}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The tag group is only marked for deletion until you activate changes!")))
    }
}

#[async_trait]
impl Handler for TagsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_host_tags" => self.get().await,
            "vibemk_create_host_tag" => self.create(args).await,
            "vibemk_update_host_tag" => self.update(args).await,
            "vibemk_delete_host_tag" => self.delete(args).await,
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
