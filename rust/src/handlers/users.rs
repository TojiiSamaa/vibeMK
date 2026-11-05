//! Port of src/vibemk/handlers/users.py (UserHandler).
//!
//! `Err` from the client is the Python exception (the Python client raises on non-2xx), so every
//! `if not result.get("success")` branch of the Python is dead there and is the `Err` arm here.
//! The dead create-failure branches echoed the request body (password included): not ported.

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
type Out = Result<Vec<Value>, Fail>;

const NEXT_ACTIVATE: &str = "⚠️ **Remember to activate changes!**";

fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
/// Argument as a string, "" when missing or not a string (Python: falsy).
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(x) => !x.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
/// `", ".join(list)`.
fn join(v: &Value) -> String {
    v.as_array()
        .map(|a| a.iter().map(ps).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn items(data: &Value) -> Vec<Value> {
    data.get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
fn deletion_steps(what: &str) -> String {
    format!("📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The {what} is only marked for deletion until you activate changes!")
}

pub struct UserHandler {
    base: Base,
}

impl UserHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn c(&self) -> &CheckMkClient {
        &self.base.client
    }

    async fn get_users(&self) -> Out {
        let result = self
            .c()
            .get("domain-types/user_config/collections/all", None, &[])
            .await?;
        let users = items(&result.data);
        if users.is_empty() {
            return Ok(text(
                "👥 **No Users Found**\n\nNo CheckMK users are configured.".into(),
            ));
        }
        let list: Vec<String> = users
            .iter()
            .map(|u| {
                let id = u.get("id").map_or("Unknown".to_string(), ps);
                let get = |p: &str| u.pointer(&format!("/extensions{p}"));
                let fullname = get("/fullname").map_or_else(|| id.clone(), ps);
                let email = get("/contact_options/email").map_or("No email".to_string(), ps);
                let roles = get("/roles").map(join).unwrap_or_default();
                let status = if get("/disable_login").is_some_and(truthy) {
                    "🔒 Disabled"
                } else {
                    "✅ Active"
                };
                format!(
                    "👤 **{id}** ({fullname})\n   📧 {email}\n   🔑 Roles: {roles}\n   {status}"
                )
            })
            .collect();
        Ok(text(format!(
            "👥 **CheckMK Users** ({} total):\n\n{}",
            users.len(),
            list.join("\n\n")
        )))
    }

    async fn create_user(&self, a: &Value) -> Out {
        let (username, fullname, email, password) = (
            s(a, "username"),
            s(a, "fullname"),
            s(a, "email"),
            s(a, "password"),
        );
        let roles = a.get("roles").cloned().unwrap_or_else(|| json!(["user"]));
        let groups = a
            .get("contactgroups")
            .cloned()
            .unwrap_or_else(|| json!(["all"]));
        if username.is_empty() || fullname.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("username and fullname are required"),
            ));
        }
        let mut data = json!({"username": username, "fullname": fullname, "disable_notifications": {}, "interface_options": {}});
        if !password.is_empty() {
            data["auth_option"] = json!({"auth_type": "password", "password": password});
        }
        if !email.is_empty() {
            data["contact_options"] = json!({"email": email});
        }
        if truthy(&roles) {
            data["roles"] = roles.clone();
        }
        if truthy(&groups) {
            data["contactgroups"] = groups.clone();
        }
        self.c()
            .post("domain-types/user_config/collections/all", Some(&data), &[])
            .await?;
        Ok(text(format!(
            "✅ **User Created Successfully**\n\nUsername: {username}\nFull Name: {fullname}\nEmail: {}\nRoles: {}\nContact Groups: {}\nLogin: {}\n\n{NEXT_ACTIVATE}",
            if email.is_empty() { "Not set" } else { email },
            join(&roles),
            join(&groups),
            if password.is_empty() { "Disabled" } else { "Enabled" },
        )))
    }

    async fn update_user(&self, a: &Value) -> Out {
        let username = s(a, "username");
        if username.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("username is required")));
        }
        let endpoint = format!("objects/user_config/{}", path_segment(username)?);
        self.c().get(&endpoint, None, &[]).await?;
        let mut data = Map::new();
        if !s(a, "fullname").is_empty() {
            data.insert("fullname".into(), json!(s(a, "fullname")));
        }
        if !s(a, "email").is_empty() {
            data.insert("contact_options".into(), json!({"email": s(a, "email")}));
        }
        if let Some(r) = a.get("roles").filter(|r| truthy(r)) {
            data.insert("roles".into(), r.clone());
        }
        if let Some(g) = a.get("contactgroups").filter(|g| !g.is_null()) {
            data.insert("contactgroups".into(), g.clone());
        }
        let headers = self.base.if_match_header(&endpoint).await;
        let fields = data.keys().cloned().collect::<Vec<_>>().join(", ");
        self.c()
            .put(&endpoint, Some(&Value::Object(data)), &headers)
            .await?;
        Ok(text(format!("✅ **User Updated Successfully**\n\nUsername: {username}\nUpdated fields: {fields}\n\n{NEXT_ACTIVATE}")))
    }

    async fn delete_user(&self, a: &Value) -> Out {
        let username = s(a, "username");
        if username.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("username is required")));
        }
        let endpoint = format!("objects/user_config/{}", path_segment(username)?);
        self.c().get(&endpoint, None, &[]).await?;
        self.c().delete(&endpoint, None, &[]).await?;
        Ok(text(format!(
            "✅ **User Deleted Successfully**\n\nUsername: {username}\n\n{}",
            deletion_steps("user")
        )))
    }

    async fn get_contact_groups(&self) -> Out {
        let result = self
            .c()
            .get(
                "domain-types/contact_group_config/collections/all",
                None,
                &[],
            )
            .await?;
        let groups = items(&result.data);
        if groups.is_empty() {
            return Ok(text(
                "👥 **No Contact Groups Found**\n\nNo contact groups are configured.".into(),
            ));
        }
        let list: Vec<String> = groups
            .iter()
            .map(|g| {
                let id = g.get("id").map_or("Unknown".to_string(), ps);
                let alias = g
                    .pointer("/extensions/alias")
                    .map_or_else(|| id.clone(), ps);
                format!("👥 **{id}** - {alias}")
            })
            .collect();
        Ok(text(format!(
            "👥 **Contact Groups** ({} total):\n\n{}",
            groups.len(),
            list.join("\n")
        )))
    }

    async fn create_contact_group(&self, a: &Value) -> Out {
        let (name, alias) = (s(a, "name"), s(a, "alias"));
        if name.is_empty() || alias.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameters", Some("name and alias are required")));
        }
        // Members are managed through user configurations, never sent here.
        let members = a
            .get("members")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let data = json!({"name": name, "alias": alias});
        self.c()
            .post(
                "domain-types/contact_group_config/collections/all",
                Some(&data),
                &[],
            )
            .await?;
        Ok(text(format!("✅ **Contact Group Created Successfully**\n\nName: {name}\nAlias: {alias}\nMembers: {members} users\n\n{NEXT_ACTIVATE}")))
    }

    async fn update_contact_group(&self, a: &Value) -> Out {
        let (name, alias) = (s(a, "name"), s(a, "alias"));
        if name.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        }
        let endpoint = format!("objects/contact_group_config/{}", path_segment(name)?);
        self.c().get(&endpoint, None, &[]).await?;
        if alias.is_empty() {
            return Ok(self
                .base
                .error_response("No data to update", Some("At least alias must be provided")));
        }
        let headers = self.base.if_match_header(&endpoint).await;
        self.c()
            .put(&endpoint, Some(&json!({"alias": alias})), &headers)
            .await?;
        Ok(text(format!("✅ **Contact Group Updated Successfully**\n\nName: {name}\nUpdated fields: alias\n\n{NEXT_ACTIVATE}")))
    }

    async fn delete_contact_group(&self, a: &Value) -> Out {
        let name = s(a, "name");
        if name.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("name is required")));
        }
        let endpoint = format!("objects/contact_group_config/{}", path_segment(name)?);
        self.c().get(&endpoint, None, &[]).await?;
        self.c().delete(&endpoint, None, &[]).await?;
        Ok(text(format!(
            "✅ **Contact Group Deleted Successfully**\n\nName: {name}\n\n{}",
            deletion_steps("contact group")
        )))
    }

    /// add_user_to_group / remove_user_from_group: read the groups, change the list, PUT with If-Match.
    async fn change_membership(&self, a: &Value, add: bool) -> Out {
        let (username, group) = (s(a, "username"), s(a, "group_name"));
        if username.is_empty() || group.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("username and group_name are required"),
            ));
        }
        let endpoint = format!("objects/user_config/{}", path_segment(username)?);
        let user = self.c().get(&endpoint, None, &[]).await?;
        let current: Vec<Value> = user
            .data
            .pointer("/extensions/contactgroups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let member = current.contains(&json!(group));
        if member == add {
            let (title, tail) = if add {
                (
                    "User Already in Group",
                    "User is already a member of this contact group.",
                )
            } else {
                (
                    "User Not in Group",
                    "User is not a member of this contact group.",
                )
            };
            return Ok(text(format!(
                "ℹ️ **{title}**\n\nUsername: {username}\nGroup: {group}\n\n{tail}"
            )));
        }
        let new_groups: Vec<Value> = if add {
            [current, vec![json!(group)]].concat()
        } else {
            current.into_iter().filter(|g| g != group).collect()
        };
        let headers = self.base.if_match_header(&endpoint).await;
        self.c()
            .put(
                &endpoint,
                Some(&json!({"contactgroups": new_groups})),
                &headers,
            )
            .await?;
        let joined = join(&Value::Array(new_groups));
        Ok(text(if add {
            format!("✅ **User Added to Contact Group**\n\nUsername: {username}\nAdded to group: {group}\nTotal groups: {joined}\n\n{NEXT_ACTIVATE}")
        } else {
            let remaining = if joined.is_empty() {
                "None".to_string()
            } else {
                joined
            };
            format!("✅ **User Removed from Contact Group**\n\nUsername: {username}\nRemoved from group: {group}\nRemaining groups: {remaining}\n\n{NEXT_ACTIVATE}")
        }))
    }
}

#[async_trait]
impl Handler for UserHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_users" => self.get_users().await,
            "vibemk_create_user" => self.create_user(args).await,
            "vibemk_update_user" => self.update_user(args).await,
            "vibemk_delete_user" => self.delete_user(args).await,
            "vibemk_get_contact_groups" => self.get_contact_groups().await,
            "vibemk_create_contact_group" => self.create_contact_group(args).await,
            "vibemk_update_contact_group" => self.update_contact_group(args).await,
            "vibemk_delete_contact_group" => self.delete_contact_group(args).await,
            "vibemk_add_user_to_group" => self.change_membership(args, true).await,
            "vibemk_remove_user_from_group" => self.change_membership(args, false).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|e| match e {
            Fail::Api(e) => self
                .base
                .error_response("CheckMK API Error", Some(&e.to_string())),
            Fail::Path(e) => self
                .base
                .error_response("Unexpected Error", Some(&e.to_string())),
        })
    }
}
