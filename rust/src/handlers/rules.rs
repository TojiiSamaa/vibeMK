//! Port of src/vibemk/handlers/rules.py (RulesHandler).
//!
//! A failed CheckMK call is `Err` here (Python raised `CheckMKError`), so the Python
//! "success is false" branches of the reads and writes cannot happen and are not ported.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

const RULESET_DISPLAY_LIMIT: usize = 20;
const RULE_DISPLAY_LIMIT: usize = 10;

/// Every position name accepted, sorted: the list quoted in the "not a position" error.
const ACCEPTED_POSITIONS: &str = "after, after_specific_rule, before, before_specific_rule, bottom, bottom_of_folder, top, top_of_folder";

const ACTIVATE_REMINDER: &str = "⚠️ **Remember to activate changes!**";

pub struct RulesHandler {
    base: Base,
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

/// Python `str()`.
fn py_str(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// `str(d.get(key, default))`: a key present with null prints "None", as in Python.
fn get_or(v: &Value, key: &str, default: &str) -> String {
    v.get(key).map_or_else(|| default.to_string(), py_str)
}

/// `d.get(key)` when truthy, else None.
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| truthy(v))
}

/// Translate a requested position into the move endpoint's body, or say why not.
fn move_body(
    position: &str,
    folder: Option<&str>,
    target_rule_id: Option<&str>,
) -> Result<Value, String> {
    let folder_position = match position {
        "top" | "top_of_folder" => Some("top_of_folder"),
        "bottom" | "bottom_of_folder" => Some("bottom_of_folder"),
        _ => None,
    };
    if let Some(p) = folder_position {
        return match folder.filter(|f| !f.is_empty()) {
            Some(f) => Ok(json!({"position": p, "folder": f})),
            None => Err("the folder the rule lives in could not be determined".into()),
        };
    }
    let relative_position = match position {
        "before" | "before_specific_rule" => Some("before_specific_rule"),
        "after" | "after_specific_rule" => Some("after_specific_rule"),
        _ => None,
    };
    if let Some(p) = relative_position {
        return match target_rule_id.filter(|t| !t.is_empty()) {
            Some(t) => Ok(json!({"position": p, "rule_id": t})),
            None => Err("target_rule_id is required for 'before' and 'after'".into()),
        };
    }
    Err(format!(
        "'{position}' is not a position CheckMK accepts. Use one of: {ACCEPTED_POSITIONS}"
    ))
}

/// The value as the Python literal CheckMK stores in value_raw; a one-item list is flattened.
fn value_raw(value: &Value) -> String {
    match value {
        Value::Array(a) if a.len() == 1 => py_repr(&a[0]),
        other => py_repr(other),
    }
}

/// "/" -> "~", "/hosts/linux" -> "~hosts~linux"; a path already in tilde notation passes through.
fn api_folder(folder: &str) -> String {
    if folder.starts_with('~') {
        folder.to_string()
    } else if folder == "/" {
        "~".to_string()
    } else if let Some(rest) = folder.strip_prefix('/') {
        format!("~{}", rest.replace('/', "~"))
    } else {
        format!("~{}", folder.replace('/', "~"))
    }
}

fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

type R = Result<Vec<Value>, ApiError>;

impl RulesHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn get_rulesets(&self, args: &Value) -> R {
        let params = arg(args, "search").map(|s| json!({"search": s}));
        let result = self
            .base
            .client
            .get("domain-types/ruleset/collections/all", params.as_ref(), &[])
            .await?;

        let empty = vec![];
        let rulesets = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        if rulesets.is_empty() {
            return Ok(text("📋 **No Rulesets Found**\n\nNo rulesets are available or match the search criteria.".into()));
        }

        let list: Vec<String> = rulesets
            .iter()
            .take(RULESET_DISPLAY_LIMIT)
            .map(|rs| {
                let name = get_or(rs, "id", "Unknown");
                let ext = rs.get("extensions").cloned().unwrap_or_else(|| json!({}));
                let title = get_or(&ext, "title", &name);
                let help: String = get_or(&ext, "help", "No description")
                    .chars()
                    .take(100)
                    .collect();
                format!("📋 **{name}**\n   Title: {title}\n   Help: {help}...")
            })
            .collect();

        let mut out = format!(
            "📋 **Available Rulesets** ({} total):\n\n{}",
            rulesets.len(),
            list.join("\n\n")
        );
        if rulesets.len() > RULESET_DISPLAY_LIMIT {
            out.push_str(&format!(
                "\n\n... and {} more rulesets",
                rulesets.len() - RULESET_DISPLAY_LIMIT
            ));
        }
        Ok(text(out))
    }

    async fn get_ruleset(&self, args: &Value) -> R {
        let Some(name) = arg(args, "ruleset_name") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ruleset_name is required")));
        };
        let name_s = py_str(name);
        let result = self
            .base
            .client
            .get(
                "domain-types/rule/collections/all",
                Some(&json!({"ruleset_name": name})),
                &[],
            )
            .await?;

        let empty = vec![];
        let rules = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let list: Vec<String> = rules
            .iter()
            .take(RULE_DISPLAY_LIMIT)
            .enumerate()
            .map(|(i, rule)| {
                let id = rule.get("id").map_or_else(|| format!("Rule {}", i + 1), py_str);
                let ext = rule.get("extensions").cloned().unwrap_or_else(|| json!({}));
                let props = ext.get("properties").cloned().unwrap_or_else(|| json!({}));
                let comment = get_or(&props, "comment", "No comment");
                let status = if props.get("disabled").is_some_and(truthy) { "🔒 Disabled" } else { "✅ Active" };
                let value = get_or(&ext, "value_raw", "No value");
                let folder = get_or(&ext, "folder", "/");
                let cond = ext.get("conditions").cloned().unwrap_or_else(|| json!({}));

                let mut summary = vec![];
                if let Some(h) = cond.get("host_name").filter(|h| truthy(h)) {
                    let on = h.get("match_on").map_or_else(|| "[]".to_string(), py_repr);
                    summary.push(format!("Hosts: {on} ({})", get_or(h, "operator", "unknown")));
                }
                for (key, label) in [("host_tags", "Tags"), ("host_label_groups", "Labels")] {
                    let count = cond.get(key).filter(|c| truthy(c)).and_then(|c| c.as_array().map(Vec::len).or_else(|| c.as_object().map(Map::len)));
                    if let Some(n) = count {
                        summary.push(format!("{label}: {n} conditions"));
                    }
                }
                let conditions = if summary.is_empty() { "All hosts".to_string() } else { summary.join(", ") };

                format!(
                    "🔧 **Rule {}** (ID: {id})\n   Status: {status}\n   Value: {value}\n   Folder: {folder}\n   Conditions: {conditions}\n   Comment: {comment}",
                    i + 1
                )
            })
            .collect();

        let body = if list.is_empty() {
            "No rules configured in this ruleset".to_string()
        } else {
            list.join("\n\n")
        };
        let more = if rules.len() > RULE_DISPLAY_LIMIT {
            format!(
                "\n\n... and {} more rules",
                rules.len() - RULE_DISPLAY_LIMIT
            )
        } else {
            String::new()
        };
        Ok(text(format!(
            "📋 **Ruleset: {name_s}**\n\nRules ({} total):\n\n{body}{more}",
            rules.len()
        )))
    }

    async fn create_rule(&self, args: &Value) -> R {
        let Some(ruleset) = arg(args, "ruleset_name") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ruleset_name is required")));
        };
        let Some(rule_config) = arg(args, "rule_config") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_config is required")));
        };
        let ruleset_s = py_str(ruleset);
        let comment = args.get("comment").map_or_else(String::new, py_str);
        let folder = args.get("folder").map_or_else(|| "/".to_string(), py_str);
        let folder_api = api_folder(&folder);

        let mut props = json!({"disabled": false});
        if let Some(c) = arg(args, "comment") {
            props["comment"] = c.clone();
        }
        let data = json!({
            "properties": props,
            "value_raw": value_raw(rule_config),
            "conditions": arg(args, "conditions").cloned().unwrap_or_else(|| json!({})),
            "ruleset": ruleset,
            "folder": folder_api,
        });
        let result = self
            .base
            .client
            .post("domain-types/rule/collections/all", Some(&data), &[])
            .await?;

        let rule_id = result
            .data
            .get("id")
            .map_or_else(|| "unknown".to_string(), py_str);
        let placement = self
            .place_new_rule(&rule_id, arg(args, "position"), &folder_api, args)
            .await;
        Ok(text(format!(
            "✅ **Rule Created Successfully**\n\nRuleset: {ruleset_s}\nRule ID: {rule_id}\nFolder: {folder}\nComment: {comment}\n{placement}\n{ACTIVATE_REMINDER}"
        )))
    }

    /// Move a freshly created rule into the requested position; a failed move is reported, not swallowed.
    async fn place_new_rule(
        &self,
        rule_id: &str,
        position: Option<&Value>,
        folder: &str,
        args: &Value,
    ) -> String {
        let Some(position) = position else {
            return String::new();
        };
        let position_s = py_str(position);
        let target = arg(args, "target_rule_id").map(py_str);
        let body = match move_body(&position_s, Some(folder), target.as_deref()) {
            Ok(b) => b,
            Err(problem) => return format!("\n⚠️ **Created, but not positioned:** {problem}\n"),
        };
        let moved = async {
            let endpoint = format!("objects/rule/{}", path_segment(rule_id)?);
            let headers = self.base.if_match_header(&endpoint).await;
            self.base
                .client
                .post(
                    &format!("{endpoint}/actions/move/invoke"),
                    Some(&body),
                    &headers,
                )
                .await
        }
        .await;
        match moved {
            Ok(_) => format!("Position: {}\n", py_str(&body["position"])),
            Err(_) => format!("\n⚠️ **Created, but the requested position '{position_s}' could not be applied.**\n"),
        }
    }

    async fn update_rule(&self, args: &Value) -> R {
        let Some(rule_id) = arg(args, "rule_id") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        let rule_id_s = py_str(rule_id);

        let mut data = Map::new();
        if let Some(c) = arg(args, "rule_config") {
            data.insert("value_raw".into(), c.clone());
        }
        if let Some(c) = arg(args, "conditions") {
            data.insert("conditions".into(), c.clone());
        }
        let mut props = Map::new();
        for key in ["comment", "disabled"] {
            if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
                props.insert(key.into(), v.clone());
            }
        }
        if !props.is_empty() {
            data.insert("properties".into(), Value::Object(props));
        }
        if data.is_empty() {
            return Ok(self.base.error_response(
                "No data to update",
                Some("At least one field must be provided"),
            ));
        }

        let endpoint = format!("objects/rule/{}", path_segment(&rule_id_s)?);
        let headers = self.base.if_match_header(&endpoint).await;
        // Python joins data.keys() in insertion order: value_raw, conditions, properties.
        let fields = ["value_raw", "conditions", "properties"]
            .into_iter()
            .filter(|k| data.contains_key(*k))
            .collect::<Vec<_>>()
            .join(", ");
        self.base
            .client
            .put(&endpoint, Some(&Value::Object(data)), &headers)
            .await?;
        Ok(text(format!("✅ **Rule Updated Successfully**\n\nRule ID: {rule_id_s}\nUpdated fields: {fields}\n\n{ACTIVATE_REMINDER}")))
    }

    async fn delete_rule(&self, args: &Value) -> R {
        let Some(rule_id) = arg(args, "rule_id") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        let rule_id = py_str(rule_id);
        self.base
            .client
            .delete(
                &format!("objects/rule/{}", path_segment(&rule_id)?),
                None,
                &[],
            )
            .await?;
        Ok(text(format!(
            "✅ **Rule Deleted Successfully**\n\nRule ID: {rule_id}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The rule is only marked for deletion until you activate changes!"
        )))
    }

    async fn move_rule(&self, args: &Value) -> R {
        let Some(rule_id) = arg(args, "rule_id") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        };
        let rule_id = py_str(rule_id);
        let position = args
            .get("position")
            .map_or_else(|| "top".to_string(), py_str);
        let target = arg(args, "target_rule_id").map(py_str);

        // One read serves two purposes: the ETag CheckMK demands, and the rule's current folder.
        let endpoint = format!("objects/rule/{}", path_segment(&rule_id)?);
        let current = match self.base.client.get(&endpoint, None, &[]).await {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::debug!("Could not read rule {rule_id} before moving it: {e}");
                None
            }
        };
        let folder = current
            .as_ref()
            .and_then(|c| c.data.pointer("/extensions/folder"))
            .and_then(Value::as_str);
        let body = match move_body(&position, folder, target.as_deref()) {
            Ok(b) => b,
            Err(problem) => {
                return Ok(self.base.error_response("Invalid position", Some(&problem)))
            }
        };

        let etag = current
            .as_ref()
            .map_or_else(|| "*".to_string(), Base::extract_etag);
        let headers = vec![("If-Match".to_string(), etag)];
        self.base
            .client
            .post(
                &format!("{endpoint}/actions/move/invoke"),
                Some(&body),
                &headers,
            )
            .await?;

        let target_line = target
            .map(|t| format!("Target Rule: {t}\n"))
            .unwrap_or_default();
        Ok(text(format!(
            "✅ **Rule Moved Successfully**\n\nRule ID: {rule_id}\nNew Position: {}\n{target_line}\n{ACTIVATE_REMINDER}",
            py_str(&body["position"])
        )))
    }
}

#[async_trait]
impl Handler for RulesHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_rulesets" => self.get_rulesets(args).await,
            "vibemk_get_ruleset" => self.get_ruleset(args).await,
            "vibemk_create_rule" => self.create_rule(args).await,
            "vibemk_update_rule" => self.update_rule(args).await,
            "vibemk_delete_rule" => self.delete_rule(args).await,
            "vibemk_move_rule" => self.move_rule(args).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|e| {
            self.base
                .error_response("CheckMK API Error", Some(&e.to_string()))
        })
    }
}
