//! Port of src/vibemk/handlers/folders.py (FolderHandler) and utils/folder_validator.py.
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported.
//! The validator below is the same as the private one in hosts.rs (that file is not ours to touch).

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

fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
fn valid_chars(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}

// ---- folder_validator.py ----

struct Folder {
    valid: bool,
    checkmk_path: String,
    display_path: String,
    error_message: String,
}

fn validate_folder_path(folder: &Value, operation: &str) -> Folder {
    let fail = |errors: Vec<String>, checkmk_path: String, display_path: String| {
        let error_message = match errors.len() {
            0 => String::new(),
            1 => format!("❌ {}", errors[0]),
            _ => format!(
                "❌ **Folder Path Validation Errors:**\n{}",
                errors
                    .iter()
                    .map(|e| format!("• {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        Folder {
            valid: errors.is_empty(),
            checkmk_path,
            display_path,
            error_message,
        }
    };
    let Some(raw) = folder.as_str().filter(|f| !f.is_empty()) else {
        return fail(
            vec!["Folder path cannot be empty".into()],
            String::new(),
            String::new(),
        );
    };
    let path = raw.trim().to_lowercase();
    if matches!(path.as_str(), "/" | "~" | "") {
        return fail(vec![], "~".into(), "/".into());
    }
    let stripped = path
        .trim_matches(|c| c == '/' || c == '~')
        .replace('~', "/");
    let normalized = if stripped.is_empty() {
        "/".to_string()
    } else {
        format!("/{stripped}")
    };
    let display = if normalized == "/" {
        "/ (root)".to_string()
    } else {
        normalized.clone()
    };
    let names: Vec<&str> = if normalized == "/" {
        vec![]
    } else {
        normalized
            .trim_matches('/')
            .split('/')
            .filter(|n| !n.is_empty())
            .collect()
    };
    let mut errors = vec![];
    for name in &names {
        if name.chars().count() > 50 {
            errors.push(format!("Folder name '{name}' exceeds 50 characters"));
        }
        if !valid_chars(name, &['_', '-']) {
            errors.push(format!(
                "Invalid folder name '{name}'. Use only letters, numbers, underscore, and hyphen."
            ));
        }
        if matches!(
            name.to_lowercase().as_str(),
            "main" | "root" | "system" | "tmp" | "temp"
        ) {
            errors.push(format!("'{name}' is a reserved folder name"));
        }
    }
    let checkmk = if normalized == "/" {
        "~".to_string()
    } else {
        format!("~{}", normalized.trim_start_matches('/').replace('/', "~"))
    };
    match operation {
        "create" if names.is_empty() => errors.push("Cannot create root folder".into()),
        "create" if names.len() > 5 => {
            errors.push("Folder hierarchy too deep (maximum 5 levels)".into())
        }
        "delete" if names.is_empty() => errors.push("Cannot delete root folder".into()),
        "move" if names.is_empty() => errors.push("Cannot move root folder".into()),
        _ => {}
    }
    fail(errors, checkmk, display)
}

pub struct FolderHandler {
    base: Base,
}

impl FolderHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn invalid(&self, title: &str, f: &Folder) -> Out {
        Ok(self.base.error_response(title, Some(&f.error_message)))
    }

    async fn get_folders(&self, args: &Value) -> Out {
        let parent = args.get("parent");
        let params = if truthy(parent) {
            json!({"parent": parent})
        } else {
            json!({})
        };
        let data = self
            .base
            .client
            .get(
                "domain-types/folder_config/collections/all",
                Some(&params),
                &[],
            )
            .await?
            .data;
        let folders = data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if folders.is_empty() {
            return Ok(text("📁 No folders found".into()));
        }
        let list: Vec<String> = folders
            .iter()
            .take(50)
            .map(|f| {
                let id = f.get("id").cloned().unwrap_or_else(|| json!("Unknown"));
                let title = f.get("title").unwrap_or(&id);
                let path = f.pointer("/extensions/path").unwrap_or(&id);
                format!("📁 {} - {}", ps(path), ps(title))
            })
            .collect();
        Ok(text(format!(
            "📁 **CheckMK Folders** ({} total, showing first {}):\n\n{}",
            folders.len(),
            list.len(),
            list.join("\n")
        )))
    }

    async fn create_folder(&self, args: &Value) -> Out {
        let folder = args.get("folder").filter(|v| truthy(Some(v)));
        let title = args.get("title").filter(|v| truthy(Some(v)));
        let (Some(folder), Some(title)) = (folder, title) else {
            return Ok(self
                .base
                .error_response("Missing parameters", Some("folder and title are required")));
        };
        let folder = ps(folder).to_lowercase();
        let parent = args.get("parent").cloned().unwrap_or_else(|| json!("/"));
        let pv = validate_folder_path(&parent, "create");
        if !pv.valid {
            return self.invalid("Invalid parent folder", &pv);
        }
        if folder.contains('/') || folder.contains('~') {
            return Ok(self.base.error_response("Invalid folder name", Some("Folder name cannot contain path separators. Use 'parent' parameter for folder hierarchy.")));
        }
        let full_path = format!("{}/{folder}", pv.display_path).replace("//", "/");
        let fv = validate_folder_path(&json!(full_path), "create");
        if !fv.valid {
            return self.invalid("Invalid folder path", &fv);
        }
        let body =
            json!({"name": folder, "title": title, "parent": pv.checkmk_path, "attributes": {}});
        self.base
            .client
            .post(
                "domain-types/folder_config/collections/all",
                Some(&body),
                &[],
            )
            .await?;
        Ok(text(format!(
            "✅ **Folder Created Successfully**\n\n📁 **Folder:** {folder}\n📝 **Title:** {}\n📂 **Parent:** {}\n🔗 **Full Path:** {}\n⚙️ **API Path:** {}\n\n⚠️ **Remember to activate changes!**",
            ps(title), pv.display_path, fv.display_path, fv.checkmk_path
        )))
    }

    async fn delete_folder(&self, args: &Value) -> Out {
        let folder = args.get("folder").cloned().unwrap_or(Value::Null);
        let mode = args
            .get("delete_mode")
            .map_or_else(|| "abort_on_nonempty".to_string(), ps);
        if !truthy(Some(&folder)) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("folder is required")));
        }
        let v = validate_folder_path(&folder, "delete");
        if !v.valid {
            return self.invalid("Invalid folder path", &v);
        }
        let endpoint = format!("objects/folder_config/{}", path_segment(&v.checkmk_path)?);
        self.base
            .client
            .delete(&endpoint, Some(&json!({"delete_mode": mode})), &[])
            .await?;
        Ok(text(format!(
            "✅ **Folder Deleted Successfully**\n\n📁 **Folder:** {}\n⚙️ **API Path:** {}\n🗑️ **Delete Mode:** {mode}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The folder is only marked for deletion until you activate changes!",
            v.display_path, v.checkmk_path
        )))
    }

    async fn update_folder(&self, args: &Value) -> Out {
        let folder = args.get("folder").cloned().unwrap_or(Value::Null);
        let title = args.get("title").filter(|v| truthy(Some(v)));
        let attributes = args.get("attributes").filter(|v| truthy(Some(v)));
        if !truthy(Some(&folder)) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("folder is required")));
        }
        let v = validate_folder_path(&folder, "update");
        if !v.valid {
            return self.invalid("Invalid folder path", &v);
        }
        let mut data = Map::new();
        if let Some(t) = title {
            data.insert("title".into(), t.clone());
        }
        if let Some(a) = attributes {
            data.insert("attributes".into(), a.clone());
        }
        if data.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("At least one of 'title' or 'attributes' is required"),
            ));
        }
        let endpoint = format!("objects/folder_config/{}", path_segment(&v.checkmk_path)?);
        let headers = self.base.if_match_header(&endpoint).await;
        self.base
            .client
            .put(&endpoint, Some(&Value::Object(data)), &headers)
            .await?;
        let count = attributes.map_or(0, |a| {
            a.as_object()
                .map_or_else(|| a.as_array().map_or(0, Vec::len), Map::len)
        });
        Ok(text(format!(
            "✅ **Folder Updated Successfully**\n\n📁 **Folder:** {}\n⚙️ **API Path:** {}\n{}{}\n⚠️ **Remember to activate changes!**",
            v.display_path,
            v.checkmk_path,
            title.map_or_else(String::new, |t| format!("📝 **New Title:** {}\n", ps(t))),
            if attributes.is_some() { format!("⚙️ **Attributes Updated:** {count} items\n") } else { String::new() }
        )))
    }

    async fn move_folder(&self, args: &Value) -> Out {
        let folder = args.get("folder").cloned().unwrap_or(Value::Null);
        let dest = args.get("destination").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&folder)) || !truthy(Some(&dest)) {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("folder and destination are required"),
            ));
        }
        let src = validate_folder_path(&folder, "move");
        if !src.valid {
            return self.invalid("Invalid source folder path", &src);
        }
        let dst = validate_folder_path(&dest, "general");
        if !dst.valid {
            return self.invalid("Invalid destination folder path", &dst);
        }
        let endpoint = format!(
            "objects/folder_config/{}/actions/move/invoke",
            path_segment(&src.checkmk_path)?
        );
        self.base
            .client
            .post(
                &endpoint,
                Some(&json!({"destination": dst.checkmk_path})),
                &[],
            )
            .await?;
        Ok(text(format!(
            "✅ **Folder Moved Successfully**\n\n📁 **Source:** {}\n📂 **Destination:** {}\n⚙️ **API Paths:**\n   • From: {}\n   • To: {}\n\n⚠️ **Remember to activate changes!**",
            src.display_path, dst.display_path, src.checkmk_path, dst.checkmk_path
        )))
    }

    async fn get_folder_hosts(&self, args: &Value) -> Out {
        let folder = args.get("folder").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&folder)) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("folder is required")));
        }
        let v = validate_folder_path(&folder, "general");
        if !v.valid {
            return self.invalid("Invalid folder path", &v);
        }
        let endpoint = format!(
            "objects/folder_config/{}/collections/hosts",
            path_segment(&v.checkmk_path)?
        );
        let data = self.base.client.get(&endpoint, None, &[]).await?.data;
        let hosts = data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if hosts.is_empty() {
            return Ok(text(format!(
                "📁 **Folder '{}' is empty**\n\n⚙️ **API Path:** {}\n\nNo hosts found in this folder.",
                v.display_path, v.checkmk_path
            )));
        }
        let list: Vec<String> = hosts
            .iter()
            .take(50)
            .map(|h| {
                let id = h.get("id").map_or_else(|| "Unknown".to_string(), ps);
                let alias = h.pointer("/extensions/alias");
                format!(
                    "🖥️ {id}{}",
                    if truthy(alias) {
                        format!(" ({})", ps(alias.unwrap_or(&Value::Null)))
                    } else {
                        String::new()
                    }
                )
            })
            .collect();
        Ok(text(format!(
            "📁 **Hosts in Folder '{}'** ({} total{}):\n\n⚙️ **API Path:** {}\n\n{}",
            v.display_path,
            hosts.len(),
            if hosts.len() > 50 {
                ", showing first 50"
            } else {
                ""
            },
            v.checkmk_path,
            list.join("\n")
        )))
    }
}

#[async_trait]
impl Handler for FolderHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_folders" => self.get_folders(args).await,
            "vibemk_create_folder" => self.create_folder(args).await,
            "vibemk_delete_folder" => self.delete_folder(args).await,
            "vibemk_update_folder" => self.update_folder(args).await,
            "vibemk_move_folder" => self.move_folder(args).await,
            "vibemk_get_folder_hosts" => self.get_folder_hosts(args).await,
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
