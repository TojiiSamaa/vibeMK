//! Port of src/vibemk/handlers/user_roles.py (UserRolesHandler).
//!
//! The Python catches `CheckMKError` inside each tool and answers by HTTP status; `Err` from the
//! client is that exception. Anything else (an unsafe path segment) is the outer "Unexpected Error".
//! No If-Match on role updates, as in Python (AUDIT E7: requirement not proven).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

const BUILTIN: [&str; 3] = ["admin", "user", "guest"];
const FORBIDDEN_USERS: &str =
    "Access denied. You need 'wato.users' permission for user role management.";

fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
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
/// Python `len()` of a list or dict.
fn len(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => 0,
    }
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn role_icon(id: &str) -> &'static str {
    match id {
        "admin" => "👑",
        "user" => "👤",
        "guest" => "👁️",
        _ => "🎭",
    }
}
fn role_description(id: &str) -> &'static str {
    match id {
        "admin" => "Full CheckMK administrator with all permissions",
        "user" => "Normal user - can only see own hosts/services, limited changes",
        "guest" => "Read-only access - can see everything but change nothing",
        _ => "Custom user role",
    }
}

pub struct UserRolesHandler {
    base: Base,
}

impl UserRolesHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn c(&self) -> &CheckMkClient {
        &self.base.client
    }

    fn err(&self, title: &str, details: &str) -> Vec<Value> {
        self.base.error_response(title, Some(details))
    }

    /// The Python answer by HTTP status; `table` holds the statuses a tool names, the rest is `fallback`.
    fn by_status(&self, e: &ApiError, table: &[(u16, &str, String)], fallback: &str) -> Vec<Value> {
        let status = e.status_code.unwrap_or(0);
        table.iter().find(|(c, _, _)| *c == status).map_or_else(
            || self.err(fallback, &e.to_string()),
            |(_, t, d)| self.err(t, d),
        )
    }

    async fn list_user_roles(&self, a: &Value) -> Vec<Value> {
        let show_builtin = a.get("show_builtin").is_none_or(truthy);
        let result = match self
            .c()
            .get("domain-types/user_role/collections/all", None, &[])
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let table = [
                    (403, "Permission Denied", FORBIDDEN_USERS.to_string()),
                    (
                        406,
                        "Accept Header Error",
                        "API cannot satisfy the requested content type.".to_string(),
                    ),
                ];
                return self.by_status(&e, &table, "Failed to list user roles");
            }
        };
        let roles = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if roles.is_empty() {
            return text("👥 **No User Roles Found**\n\nNo user roles are available in this CheckMK instance.\n\n💡 This might indicate a permissions issue or API access problem.".into());
        }
        text(format_roles_list(&roles, show_builtin))
    }

    async fn show_user_role(&self, a: &Value) -> Vec<Value> {
        let role_id = s(a, "role_id");
        if role_id.is_empty() {
            return self.err("Missing parameter", "role_id is required");
        }
        let endpoint = match path_segment(role_id) {
            Ok(p) => format!("objects/user_role/{p}"),
            Err(e) => return self.err("Unexpected Error", &e.to_string()),
        };
        match self.c().get(&endpoint, None, &[]).await {
            Ok(r) => text(format_role_details(role_id, &r.data)),
            Err(e) => {
                let table = [
                    (403, "Permission Denied", FORBIDDEN_USERS.to_string()),
                    (
                        404,
                        "Role Not Found",
                        format!(
                            "User role '{role_id}' not found. Check the role ID and try again."
                        ),
                    ),
                ];
                self.by_status(&e, &table, "Failed to retrieve user role")
            }
        }
    }

    async fn create_user_role(&self, a: &Value) -> Vec<Value> {
        let (base_id, new_id, alias) =
            (s(a, "base_role_id"), s(a, "new_role_id"), s(a, "new_alias"));
        if base_id.is_empty() || new_id.is_empty() {
            return self.err(
                "Missing parameters",
                "base_role_id and new_role_id are required",
            );
        }
        let mut data = json!({"role_id": base_id, "new_role_id": new_id});
        if !alias.is_empty() {
            data["new_alias"] = json!(alias);
        }
        if let Err(e) = self
            .c()
            .post("domain-types/user_role/collections/all", Some(&data), &[])
            .await
        {
            let table = [
                (400, "Invalid Parameters", format!("Invalid role parameters: {e}")),
                (403, "Permission Denied", "Access denied. You need 'wato.edit' and 'wato.users' permissions for role creation.".to_string()),
                (409, "Role Already Exists", format!("Role '{new_id}' already exists. Choose a different role ID.")),
            ];
            return self.by_status(&e, &table, "Failed to create user role");
        }
        text(format!(
            "✅ **User Role Created Successfully**\n\n**New Role Details:**\n• **Role ID**: `{new_id}`\n• **Cloned from**: `{base_id}`\n• **Alias**: {}\n\n**Inherited Permissions:**\nThe new role inherits all permissions from the '{base_id}' role.\n\n💡 **Next Steps:**\n• Use `vibemk_show_user_role` to view detailed permissions\n• Use `vibemk_update_user_role` to customize permissions\n• Assign this role to users in user management",
            if alias.is_empty() { "Not specified" } else { alias }
        ))
    }

    async fn update_user_role(&self, a: &Value) -> Vec<Value> {
        let (role_id, alias) = (s(a, "role_id"), s(a, "alias"));
        let permissions = a.get("permissions").cloned().unwrap_or_else(|| json!({}));
        if role_id.is_empty() {
            return self.err("Missing parameter", "role_id is required");
        }
        let mut data = json!({});
        if !alias.is_empty() {
            data["new_alias"] = json!(alias); // the API expects new_alias, not alias
        }
        if truthy(&permissions) {
            data["permissions"] = permissions.clone();
        }
        if data.as_object().is_some_and(|o| o.is_empty()) {
            return self.err(
                "No updates specified",
                "At least one of 'alias' or 'permissions' must be provided",
            );
        }
        let endpoint = match path_segment(role_id) {
            Ok(p) => format!("objects/user_role/{p}"),
            Err(e) => return self.err("Unexpected Error", &e.to_string()),
        };
        if let Err(e) = self.c().put(&endpoint, Some(&data), &[]).await {
            let table = [
                (400, "Invalid Parameters", format!("Invalid role parameters: {e}")),
                (403, "Permission Denied", "Access denied. You need 'wato.edit' and 'wato.users' permissions for role updates.".to_string()),
                (404, "Role Not Found", format!("User role '{role_id}' not found.")),
            ];
            return self.by_status(&e, &table, "Failed to update user role");
        }
        let alias_line = if alias.is_empty() {
            String::new()
        } else {
            format!("• **Alias**: {alias}")
        };
        let perm_line = if truthy(&permissions) {
            format!(
                "• **Permissions**: Updated {} permissions",
                len(&permissions)
            )
        } else {
            String::new()
        };
        text(format!(
            "✅ **User Role Updated Successfully**\n\n**Updated Role**: `{role_id}`\n\n**Changes Applied:**\n{alias_line}\n{perm_line}\n\n💡 **Use `vibemk_show_user_role` to view the updated role details**"
        ))
    }

    async fn delete_user_role(&self, a: &Value) -> Vec<Value> {
        let role_id = s(a, "role_id");
        if role_id.is_empty() {
            return self.err("Missing parameter", "role_id is required");
        }
        if BUILTIN.contains(&role_id) {
            return self.err(
                "Cannot Delete Built-in Role",
                &format!("The role '{role_id}' is a built-in role and cannot be deleted. Only custom roles can be deleted."),
            );
        }
        let endpoint = match path_segment(role_id) {
            Ok(p) => format!("objects/user_role/{p}"),
            Err(e) => return self.err("Unexpected Error", &e.to_string()),
        };
        if let Err(e) = self.c().delete(&endpoint, None, &[]).await {
            let table = [
                (400, "Cannot Delete Role", format!("Role '{role_id}' cannot be deleted. It may still be in use by users.")),
                (403, "Permission Denied", "Access denied. You need 'wato.edit' and 'wato.users' permissions for role deletion.".to_string()),
                (404, "Role Not Found", format!("User role '{role_id}' not found.")),
            ];
            return self.by_status(&e, &table, "Failed to delete user role");
        }
        text(format!(
            "✅ **User Role Deleted Successfully**\n\n**Deleted Role**: `{role_id}`\n\n⚠️ **Important Notes:**\n• Users with this role will need to be reassigned to other roles\n• Built-in roles (admin, user, guest) cannot be deleted\n• This action cannot be undone\n\n💡 **Use `vibemk_list_user_roles` to view remaining roles**"
        ))
    }
}

fn format_roles_list(roles: &[Value], show_builtin: bool) -> String {
    let mut out = "👥 **User Roles**\n\n".to_string();
    // (id, alias, permissions count)
    let (mut builtin, mut custom) = (Vec::new(), Vec::new());
    for role in roles {
        let id = role.get("id").map_or("Unknown".to_string(), ps);
        let ext = |k: &str| role.pointer(&format!("/extensions/{k}"));
        let alias = ext("alias").map_or_else(|| id.clone(), ps);
        let info = (id.clone(), alias, ext("permissions").map_or(0, len));
        if ext("builtin").is_some_and(truthy) || BUILTIN.contains(&id.as_str()) {
            builtin.push(info);
        } else {
            custom.push(info);
        }
    }
    if show_builtin && !builtin.is_empty() {
        out.push_str("**🏗️ Built-in Roles** (cannot be deleted):\n");
        for (id, alias, n) in &builtin {
            out.push_str(&format!(
                "{} **{id}** - {alias}\n   📊 {n} permissions\n   💡 {}\n\n",
                role_icon(id),
                role_description(id)
            ));
        }
    }
    if !custom.is_empty() {
        out.push_str("**🎨 Custom Roles**:\n");
        for (id, alias, n) in &custom {
            out.push_str(&format!("✏️ **{id}** - {alias}\n   📊 {n} permissions\n\n"));
        }
    } else if builtin.is_empty() {
        out.push_str("No custom roles found.\n\n");
    }
    out.push_str("**💡 Available Operations:**\n");
    out.push_str("• `vibemk_show_user_role` - View detailed role information\n");
    out.push_str("• `vibemk_create_user_role` - Clone an existing role\n");
    out.push_str("• `vibemk_update_user_role` - Modify custom role permissions\n");
    out.push_str("• `vibemk_delete_user_role` - Delete custom roles\n");
    out
}

fn format_role_details(role_id: &str, role_data: &Value) -> String {
    let ext = |k: &str| role_data.pointer(&format!("/extensions/{k}"));
    let alias = ext("alias").map_or_else(|| role_id.to_string(), ps);
    let builtin = ext("builtin").is_some_and(truthy) || BUILTIN.contains(&role_id);
    let empty = json!({});
    let permissions = ext("permissions").unwrap_or(&empty);

    let mut out = format!("👥 **User Role Details: {role_id}**\n\n");
    out.push_str(if builtin {
        "🏗️ **Built-in Role** (cannot be deleted)\n\n"
    } else {
        "🎨 **Custom Role**\n\n"
    });
    out.push_str("**Role Information:**\n");
    out.push_str(&format!("• **ID**: `{role_id}`\n• **Alias**: {alias}\n• **Type**: {}\n• **Permissions**: {} total\n\n", if builtin { "Built-in" } else { "Custom" }, len(permissions)));
    if BUILTIN.contains(&role_id) {
        out.push_str(&format!(
            "**Description:**\n{}\n\n",
            role_description(role_id)
        ));
    }
    if truthy(permissions) {
        out.push_str("**Key Permissions** (showing first 10):\n");
        // A list of names, or (older API versions) a name -> enabled map.
        match permissions {
            Value::Array(list) => list
                .iter()
                .take(10)
                .for_each(|p| out.push_str(&format!("✅ `{}`\n", ps(p)))),
            Value::Object(map) => map.iter().take(10).for_each(|(k, v)| {
                out.push_str(&format!("{} `{k}`\n", if truthy(v) { "✅" } else { "❌" }))
            }),
            _ => {}
        }
        if len(permissions) > 10 {
            out.push_str(&format!(
                "... and {} more permissions\n",
                len(permissions) - 10
            ));
        }
    }
    out.push_str("\n**Available Operations:**\n");
    if !builtin {
        out.push_str("• `vibemk_update_user_role` - Modify this role\n• `vibemk_delete_user_role` - Delete this role\n");
    }
    out.push_str("• `vibemk_create_user_role` - Clone this role to create a new one\n");
    out
}

#[async_trait]
impl Handler for UserRolesHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        match tool {
            "vibemk_list_user_roles" => self.list_user_roles(args).await,
            "vibemk_show_user_role" => self.show_user_role(args).await,
            "vibemk_create_user_role" => self.create_user_role(args).await,
            "vibemk_update_user_role" => self.update_user_role(args).await,
            "vibemk_delete_user_role" => self.delete_user_role(args).await,
            _ => self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            ),
        }
    }
}
