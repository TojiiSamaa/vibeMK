//! Port of src/vibemk/handlers/passwords.py (PasswordsHandler).
//!
//! Like the Python, no answer ever contains the stored secret: the list shows ids, titles, owners and
//! comments; create/update only name the fields. `Err` from the client is the Python `CheckMKError`;
//! an unsafe ident is the outer "Unexpected Error".

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
/// `args.get(k)` when truthy.
fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| truthy(v))
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
fn object(ident: &str) -> Result<String, UnsafePathError> {
    Ok(format!("objects/password/{}", path_segment(ident)?))
}

pub struct PasswordsHandler {
    base: Base,
}

impl PasswordsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn get_passwords(&self) -> Result<Vec<Value>, Fail> {
        let result = self
            .base
            .client
            .get("domain-types/password/collections/all", None, &[])
            .await?;
        let passwords = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if passwords.is_empty() {
            return Ok(text(
                "🔐 **No Passwords Found**\n\nNo stored passwords are configured.".into(),
            ));
        }
        let list: Vec<String> = passwords
            .iter()
            .map(|p| {
                let id = p.get("id").map_or_else(|| "Unknown".to_string(), ps);
                let ext = p.get("extensions");
                let field = |k: &str, fallback: &str| ext.and_then(|e| e.get(k)).map_or_else(|| fallback.to_string(), ps);
                let title = ext.and_then(|e| e.get("title")).map_or_else(|| id.clone(), ps);
                let comment: String = field("comment", "No description").chars().take(50).collect();
                let shared = len(ext.and_then(|e| e.get("shared")));
                let shared_info = if shared > 0 { format!("Shared with {shared} users/groups") } else { "Not shared".to_string() };
                format!("🔐 **{id}** - {title}\n   Owner: {}\n   {shared_info}\n   Comment: {comment}...", field("owner", "Unknown"))
            })
            .collect();
        Ok(text(format!(
            "🔐 **Stored Passwords** ({} total):\n\n{}",
            passwords.len(),
            list.join("\n\n")
        )))
    }

    async fn create_password(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(ident) = arg(a, "ident").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ident is required")));
        };
        let Some(password) = arg(a, "password") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("password is required")));
        };
        let owner = a.get("owner").map_or_else(|| "admin".to_string(), ps);
        let shared = a.get("shared").cloned().unwrap_or_else(|| json!([]));
        let mut data = Map::new();
        data.insert("ident".into(), json!(ident));
        data.insert("password".into(), password.clone());
        data.insert("owner".into(), json!(owner));
        for k in ["title", "comment", "documentation_url", "shared"] {
            if let Some(v) = arg(a, k) {
                data.insert(k.into(), v.clone());
            }
        }
        self.base
            .client
            .post(
                "domain-types/password/collections/all",
                Some(&Value::Object(data)),
                &[],
            )
            .await?;
        let shown = |k: &str| arg(a, k).map_or_else(|| "None".to_string(), ps);
        Ok(text(format!(
            "✅ **Password Created Successfully**\n\nIdentifier: {ident}\nTitle: {}\nOwner: {owner}\nShared with: {} users/groups\nComment: {}\n\n🔒 **Security Note:** The password is now securely stored in CheckMK\n\n⚠️ **Remember to activate changes!**",
            shown("title"),
            len(Some(&shared)),
            shown("comment")
        )))
    }

    async fn update_password(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(ident) = arg(a, "ident").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ident is required")));
        };
        let endpoint = object(&ident)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        let mut data = Map::new();
        for k in [
            "title",
            "password",
            "comment",
            "documentation_url",
            "owner",
            "shared",
        ] {
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
        Ok(text(format!(
            "✅ **Password Updated Successfully**\n\nIdentifier: {ident}\nUpdated fields: {}\n\n🔒 **Security Note:** Changes are securely stored in CheckMK\n\n⚠️ **Remember to activate changes!**",
            fields.join(", ")
        )))
    }

    async fn delete_password(&self, a: &Value) -> Result<Vec<Value>, Fail> {
        let Some(ident) = arg(a, "ident").map(ps) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ident is required")));
        };
        let endpoint = object(&ident)?;
        self.base.client.get(&endpoint, None, &[]).await?;
        // CheckMK declares If-Match required on this delete and answers 428 without it.
        let headers = self.base.if_match_header(&endpoint).await;
        self.base.client.delete(&endpoint, None, &headers).await?;
        Ok(text(format!(
            "✅ **Password Deleted Successfully**\n\nIdentifier: {ident}\n\n🔒 **Security Note:** The password has been securely removed\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The password is only marked for deletion until you activate changes!"
        )))
    }
}

#[async_trait]
impl Handler for PasswordsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_passwords" => self.get_passwords().await,
            "vibemk_create_password" => self.create_password(args).await,
            "vibemk_update_password" => self.update_password(args).await,
            "vibemk_delete_password" => self.delete_password(args).await,
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
