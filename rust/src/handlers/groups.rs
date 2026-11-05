//! Port of src/vibemk/handlers/groups.py (GroupsHandler).
//!
//! Only the 5 tools the Python `handle` dispatches are ported; its `_create/_update/_delete_service_group`
//! are unreachable there (service groups live in service_groups.rs). The Rust client returns `Err`
//! for every non-2xx answer, so the Python `if not result.get("success")` branches (dead since
//! client.py raises) are not ported: any API failure is the outer "CheckMK API Error".

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

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

pub struct GroupsHandler {
    base: Base,
}

impl GroupsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    /// Object endpoint of a group, the name filling exactly one path segment.
    fn endpoint(kind: &str, name: &str) -> Result<String, Fail> {
        path_segment(name)
            .map(|s| format!("objects/{kind}/{s}"))
            .map_err(|e| Fail::Other(e.to_string()))
    }

    /// Host and service group listings share their layout.
    async fn list(&self, collection: &str, emoji: &str, label: &str, singular: &str) -> Out {
        let result = self
            .base
            .client
            .get(
                &format!("domain-types/{collection}/collections/all"),
                None,
                &[],
            )
            .await?;
        let groups = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if groups.is_empty() {
            return Ok(text(format!(
                "{emoji} **No {singular} Groups Found**\n\nNo {} groups are configured.",
                singular.to_lowercase()
            )));
        }
        let lines: Vec<String> = groups
            .iter()
            .map(|g| {
                let id = g.get("id").map_or_else(|| "Unknown".to_string(), ps);
                let alias = g
                    .pointer("/extensions/alias")
                    .map_or_else(|| id.clone(), ps);
                format!("{emoji} **{id}** - {alias}")
            })
            .collect();
        Ok(text(format!(
            "{emoji} **{label}** ({} total):\n\n{}",
            groups.len(),
            lines.join("\n")
        )))
    }

    async fn create_host_group(&self, args: &Value) -> Out {
        let (name, alias) = (arg(args, "name"), arg(args, "alias"));
        if !truthy(name) || !truthy(alias) {
            return Ok(self
                .base
                .error_response("Missing parameters", Some("name and alias are required")));
        }
        self.base
            .client
            .post(
                "domain-types/host_group_config/collections/all",
                Some(&json!({"name": name, "alias": alias})),
                &[],
            )
            .await?;
        Ok(text(format!("✅ **Host Group Created Successfully**\n\nName: {}\nAlias: {}\n\n⚠️ **Remember to activate changes!**", ps(name), ps(alias))))
    }

    async fn update_host_group(&self, args: &Value) -> Out {
        let (name, alias) = (arg(args, "name"), arg(args, "alias"));
        if !truthy(name) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        }
        let name = ps(name);
        let endpoint = Self::endpoint("host_group_config", &name)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        if !truthy(alias) {
            return Ok(self.base.error_response(
                "No data to update",
                Some("At least one field (alias) must be provided"),
            ));
        }
        let headers = self.base.if_match_header(&endpoint).await;
        self.base
            .client
            .put(&endpoint, Some(&json!({"alias": alias})), &headers)
            .await?;
        Ok(text(format!("✅ **Host Group Updated Successfully**\n\nName: {name}\nUpdated fields: alias\n\n⚠️ **Remember to activate changes!**")))
    }

    async fn delete_host_group(&self, args: &Value) -> Out {
        let name = arg(args, "name");
        if !truthy(name) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        }
        let name = ps(name);
        let endpoint = Self::endpoint("host_group_config", &name)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        self.base.client.delete(&endpoint, None, &[]).await?;
        Ok(text(format!("✅ **Host Group Deleted Successfully**\n\nName: {name}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The host group is only marked for deletion until you activate changes!")))
    }
}

#[async_trait]
impl Handler for GroupsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_host_groups" => {
                self.list("host_group_config", "🏠", "Host Groups", "Host")
                    .await
            }
            "vibemk_create_host_group" => self.create_host_group(args).await,
            "vibemk_update_host_group" => self.update_host_group(args).await,
            "vibemk_delete_host_group" => self.delete_host_group(args).await,
            "vibemk_get_service_groups" => {
                self.list("service_group_config", "🔧", "Service Groups", "Service")
                    .await
            }
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
