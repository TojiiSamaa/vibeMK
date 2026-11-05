//! Port of src/vibemk/handlers/service_groups.py (ServiceGroupHandler).
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported; the `Err` paths keep the Python wording.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, ApiErrorKind, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

/// What the Python handler would have raised: a CheckMKError, or any other exception.
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
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}
fn arg<'a>(args: &'a Value, k: &str) -> &'a Value {
    args.get(k).unwrap_or(&NULL)
}
/// `arguments.get(k)` as a string, "" when missing or not a string (Python: falsy).
fn s<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn seg(name: &str) -> Result<String, Fail> {
    path_segment(name).map_err(|e| Fail::Other(e.to_string()))
}

/// `re.match(r"^[a-zA-Z0-9._-]+$", name)`: `$` also matches before one trailing newline.
fn valid_name(name: &Value) -> Result<bool, Fail> {
    let Value::String(n) = name else {
        return if truthy(name) {
            Err(Fail::Other(format!(
                "expected string or bytes-like object, got '{}'",
                py_type_name(name)
            )))
        } else {
            Ok(false)
        };
    };
    let n = n.strip_suffix('\n').unwrap_or(n);
    Ok(!n.is_empty()
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
}

/// `extensions.alias or title or "No alias"` (CheckMK returns the alias as the title).
fn alias_of(group: &Value) -> String {
    [group.pointer("/extensions/alias"), group.get("title")]
        .into_iter()
        .flatten()
        .find(|v| truthy(v))
        .map_or_else(|| "No alias".to_string(), ps)
}

pub struct ServiceGroupHandler {
    base: Base,
}

impl ServiceGroupHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn missing(&self, what: &str) -> Vec<Value> {
        self.base.error_response("Missing parameter", Some(what))
    }
    fn not_found(&self, name: &str) -> Vec<Value> {
        self.base.error_response(
            "Service group not found",
            Some(&format!("Service group '{name}' not found")),
        )
    }

    async fn create(&self, args: &Value) -> Out {
        let (name, alias) = (arg(args, "name"), arg(args, "alias"));
        if !truthy(name) {
            return Ok(self.missing("name is required"));
        }
        if !truthy(alias) {
            return Ok(self.missing("alias is required"));
        }
        if !valid_name(name)? {
            return Ok(self.base.error_response("Invalid name", Some("Service group name must contain only letters, numbers, hyphens, and underscores")));
        }
        let name = ps(name);
        match self
            .base
            .client
            .get(
                &format!("objects/service_group_config/{}", seg(&name)?),
                None,
                &[],
            )
            .await
        {
            Ok(_) => {
                return Ok(self.base.error_response(
                    "Service group already exists",
                    Some(&format!(
                        "Service group '{name}' already exists. Use update to modify it."
                    )),
                ))
            }
            Err(ApiError {
                kind: ApiErrorKind::NotFound,
                ..
            }) => {}
            Err(e) => return Err(e.into()),
        }
        let body = json!({"name": name, "alias": alias});
        self.base
            .client
            .post(
                "domain-types/service_group_config/collections/all",
                Some(&body),
                &[],
            )
            .await?;
        let alias = ps(alias);
        Ok(text(format!("✅ **Service Group Created Successfully**\n\n**Name:** {name}\n**Alias:** {alias}\n\n⚠️ **Remember to activate changes!**\n\n💡 **Next Steps:**\n1️⃣ Use 'list_service_groups' to view all groups\n2️⃣ Use 'activate_changes' to apply configuration")))
    }

    async fn list(&self) -> Out {
        let result = self
            .base
            .client
            .get(
                "domain-types/service_group_config/collections/all",
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
            return Ok(text(
                "ℹ️ **No Service Groups Found**\n\nUse 'create_service_group' to create one."
                    .to_string(),
            ));
        }
        let mut t = format!("📋 **Service Groups List** ({} total)\n\n", groups.len());
        for (i, g) in groups.iter().enumerate() {
            let id = g.get("id").map_or_else(|| "Unknown".to_string(), ps);
            let alias = alias_of(g);
            t.push_str(&format!(
                "**{}. Service Group: {id}**\n   Alias: {alias}\n   ID: {id}\n\n",
                i + 1
            ));
        }
        t.push_str("💡 Use 'get_service_group' with a name to see detailed information");
        Ok(text(t))
    }

    async fn get(&self, args: &Value) -> Out {
        let name = s(args, "name");
        if name.is_empty() {
            return Ok(self.missing("name is required"));
        }
        let group = self
            .base
            .client
            .get(
                &format!("objects/service_group_config/{}", seg(name)?),
                None,
                &[],
            )
            .await?
            .data;
        let mut t = format!("📋 **Service Group Details**\n\n**Name:** {name}\n**Alias:** {}\n**Type:** Service Group Configuration\n\n", alias_of(&group));
        let links = group
            .get("links")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if !links.is_empty() {
            t.push_str("🔗 **Available Operations:**\n");
            for link in links {
                let rel = link.get("rel").and_then(Value::as_str).unwrap_or("");
                match rel.rsplit('/').next().unwrap_or("") {
                    "update" => t.push_str("• Update\n"),
                    "delete" => t.push_str("• Delete\n"),
                    _ => {}
                }
            }
        }
        t.push_str(
            "\n💡 Use 'update_service_group' or 'delete_service_group' to modify this group",
        );
        Ok(text(t))
    }

    async fn update(&self, args: &Value) -> Out {
        let (name, alias) = (s(args, "name"), arg(args, "alias"));
        if name.is_empty() {
            return Ok(self.missing("name is required"));
        }
        if !truthy(alias) {
            return Ok(self.missing("alias is required"));
        }
        let endpoint = format!("objects/service_group_config/{}", seg(name)?);
        let existing = match self.base.client.get(&endpoint, None, &[]).await {
            Ok(r) => r,
            Err(ApiError {
                kind: ApiErrorKind::NotFound,
                ..
            }) => return Ok(self.not_found(name)),
            Err(e) => return Err(e.into()),
        };
        // Only the ETag header counts here (as in Python), wildcard otherwise.
        let etag = existing.header("ETag").map_or_else(
            || {
                tracing::warn!("No ETag available: falling back to If-Match '*', concurrent edits will not be detected");
                "*".to_string()
            },
            str::to_string,
        );
        let headers = vec![("If-Match".to_string(), etag)];
        self.base
            .client
            .put(&endpoint, Some(&json!({"alias": alias})), &headers)
            .await?;
        Ok(text(format!("✅ **Service Group Updated Successfully**\n\n**Name:** {name}\n**New Alias:** {}\n\n⚠️ **Remember to activate changes!**", ps(alias))))
    }

    async fn delete(&self, args: &Value) -> Out {
        let name = s(args, "name");
        if name.is_empty() {
            return Ok(self.missing("name is required"));
        }
        let endpoint = format!("objects/service_group_config/{}", seg(name)?);
        match self.base.client.get(&endpoint, None, &[]).await {
            Ok(_) => {}
            Err(ApiError {
                kind: ApiErrorKind::NotFound,
                ..
            }) => return Ok(self.not_found(name)),
            Err(e) => return Err(e.into()),
        }
        self.base.client.delete(&endpoint, None, &[]).await?;
        Ok(text(format!("✅ **Service Group Deleted Successfully**\n\n**Deleted:** {name}\n\n⚠️ **Remember to activate changes!**")))
    }

    /// Shared head of the bulk tools: the non-empty list, or the Python error.
    fn entries<'a>(&self, args: &'a Value) -> Result<&'a Vec<Value>, Vec<Value>> {
        let e = arg(args, "entries");
        if !truthy(e) {
            return Err(self.missing("entries list is required"));
        }
        e.as_array().ok_or_else(|| {
            self.base
                .error_response("Invalid parameter", Some("entries must be a list"))
        })
    }

    fn invalid_entry(&self, m: String) -> Out {
        Ok(self.base.error_response("Invalid entry", Some(&m)))
    }

    async fn bulk_create(&self, args: &Value) -> Out {
        let entries = match self.entries(args) {
            Ok(e) => e,
            Err(r) => return Ok(r),
        };
        for (i, e) in entries.iter().enumerate() {
            let n = i + 1;
            if !e.is_object() {
                return self.invalid_entry(format!("Entry {n} must be a dictionary"));
            }
            if !truthy(arg(e, "name")) {
                return self.invalid_entry(format!("Entry {n} is missing 'name'"));
            }
            if !truthy(arg(e, "alias")) {
                return self.invalid_entry(format!("Entry {n} is missing 'alias'"));
            }
            if !valid_name(arg(e, "name"))? {
                return Ok(self.base.error_response(
                    "Invalid name",
                    Some(&format!(
                        "Entry {n}: Service group name '{}' contains invalid characters",
                        ps(arg(e, "name"))
                    )),
                ));
            }
        }
        self.base
            .client
            .post(
                "domain-types/service_group_config/actions/bulk-create/invoke",
                Some(&json!({"entries": entries})),
                &[],
            )
            .await?;
        let mut t = format!("✅ **Bulk Service Groups Created Successfully**\n\n**Created:** {} service groups\n\n📋 **Created Groups:**\n", entries.len());
        for e in entries {
            t.push_str(&format!(
                "• {} ({})\n",
                ps(arg(e, "name")),
                ps(arg(e, "alias"))
            ));
        }
        t.push_str("\n⚠️ **Remember to activate changes!**");
        Ok(text(t))
    }

    async fn bulk_update(&self, args: &Value) -> Out {
        let entries = match self.entries(args) {
            Ok(e) => e,
            Err(r) => return Ok(r),
        };
        for (i, e) in entries.iter().enumerate() {
            let n = i + 1;
            if !e.is_object() {
                return self.invalid_entry(format!("Entry {n} must be a dictionary"));
            }
            if !truthy(arg(e, "name")) {
                return self.invalid_entry(format!("Entry {n} is missing 'name'"));
            }
            let attrs = arg(e, "attributes");
            if truthy(attrs) && !attrs.is_object() {
                return Err(Fail::Other(format!(
                    "'{}' object has no attribute 'get'",
                    py_type_name(attrs)
                )));
            }
            if !truthy(arg(attrs, "alias")) {
                return self.invalid_entry(format!("Entry {n} is missing 'attributes.alias'"));
            }
        }
        self.base
            .client
            .put(
                "domain-types/service_group_config/actions/bulk-update/invoke",
                Some(&json!({"entries": entries})),
                &[],
            )
            .await?;
        let mut t = format!("✅ **Bulk Service Groups Updated Successfully**\n\n**Updated:** {} service groups\n\n📋 **Updated Groups:**\n", entries.len());
        for e in entries {
            t.push_str(&format!(
                "• {} → {}\n",
                ps(arg(e, "name")),
                ps(arg(arg(e, "attributes"), "alias"))
            ));
        }
        t.push_str("\n⚠️ **Remember to activate changes!**");
        Ok(text(t))
    }

    async fn bulk_delete(&self, args: &Value) -> Out {
        let entries = match self.entries(args) {
            Ok(e) => e,
            Err(r) => return Ok(r),
        };
        for (i, e) in entries.iter().enumerate() {
            let Some(name) = e.as_str() else {
                return self.invalid_entry(format!(
                    "Entry {} must be a string (service group name)",
                    i + 1
                ));
            };
            if name.trim().is_empty() {
                return self.invalid_entry(format!("Entry {} cannot be empty", i + 1));
            }
        }
        self.base
            .client
            .post(
                "domain-types/service_group_config/actions/bulk-delete/invoke",
                Some(&json!({"entries": entries})),
                &[],
            )
            .await?;
        let mut t = format!("✅ **Bulk Service Groups Deleted Successfully**\n\n**Deleted:** {} service groups\n\n📋 **Deleted Groups:**\n", entries.len());
        for e in entries {
            t.push_str(&format!("• {}\n", ps(e)));
        }
        t.push_str("\n⚠️ **Remember to activate changes!**");
        Ok(text(t))
    }
}

#[async_trait]
impl Handler for ServiceGroupHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_create_service_group" => self.create(args).await,
            "vibemk_list_service_groups" => self.list().await,
            "vibemk_get_service_group" => self.get(args).await,
            "vibemk_update_service_group" => self.update(args).await,
            "vibemk_delete_service_group" => self.delete(args).await,
            "vibemk_bulk_create_service_groups" => self.bulk_create(args).await,
            "vibemk_bulk_update_service_groups" => self.bulk_update(args).await,
            "vibemk_bulk_delete_service_groups" => self.bulk_delete(args).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!(
                        "Tool '{tool}' not supported by service group handler"
                    )),
                )
            }
        };
        result.unwrap_or_else(|f| match f {
            Fail::Api(e) if e.kind == ApiErrorKind::NotFound => self
                .base
                .error_response("Service group not found", Some(&e.to_string())),
            Fail::Api(e) => self
                .base
                .error_response("CheckMK API error", Some(&e.to_string())),
            Fail::Other(m) => self
                .base
                .error_response("Operation failed", Some(&format!("Unexpected error: {m}"))),
        })
    }
}
