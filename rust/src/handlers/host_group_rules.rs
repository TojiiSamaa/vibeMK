//! Port of src/vibemk/handlers/host_group_rules.py (HostGroupRulesHandler).
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported; the `Err` paths keep the Python wording.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::CheckMkClient;
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

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
/// `arguments.get(k, default)`.
fn arg_or(args: &Value, k: &str, default: Value) -> Value {
    args.get(k).cloned().unwrap_or(default)
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
/// `sep.join(v)` as Python does it: a list of str, a str (its characters), else TypeError.
fn py_join(sep: &str, v: &Value) -> Result<String, String> {
    match v {
        Value::Array(a) => {
            let mut parts = Vec::with_capacity(a.len());
            for (i, x) in a.iter().enumerate() {
                match x {
                    Value::String(s) => parts.push(s.clone()),
                    other => {
                        return Err(format!(
                            "sequence item {i}: expected str instance, {} found",
                            py_type_name(other)
                        ))
                    }
                }
            }
            Ok(parts.join(sep))
        }
        Value::String(s) => Ok(s.chars().map(String::from).collect::<Vec<_>>().join(sep)),
        _ => Err("can only join an iterable".to_string()),
    }
}

pub struct HostGroupRulesHandler {
    base: Base,
}

impl HostGroupRulesHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn find_host_grouping_rulesets(&self) -> Vec<Value> {
        let mut results =
            vec!["🔍 **Host Grouping and Contact Assignment Rulesets**\n".to_string()];
        // (id, title), first-seen order like a Python dict; a later hit replaces the title.
        let mut all: Vec<(String, String)> = Vec::new();
        for term in ["contact", "group", "host", "notification", "assignment"] {
            match self
                .base
                .client
                .get(
                    "domain-types/ruleset/collections/all",
                    Some(&json!({"fulltext": term})),
                    &[],
                )
                .await
            {
                Ok(r) => {
                    for rs in r
                        .data
                        .get("value")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|x| x.is_object())
                    {
                        let id = rs.get("id").map_or_else(|| "Unknown".to_string(), ps);
                        let title = rs.get("title").map_or_else(|| "No title".to_string(), ps);
                        let low = id.to_lowercase();
                        if ["host", "contact", "group", "notification"]
                            .iter()
                            .any(|k| low.contains(k))
                        {
                            match all.iter_mut().find(|(i, _)| *i == id) {
                                Some(e) => e.1 = title,
                                None => all.push((id, title)),
                            }
                        }
                    }
                }
                Err(e) => results.push(format!("Search for '{term}' failed: {e}")),
            }
        }
        let class = |id: &str| {
            let l = id.to_lowercase();
            if l.contains("contact") {
                0
            } else if l.contains("hostgroup") || l.contains("host_group") {
                1
            } else if l.contains("notification") {
                2
            } else {
                3
            }
        };
        for (n, header) in [
            "\n📞 **Contact Group Assignment Rules:**",
            "\n🏠 **Host Group Assignment Rules:**",
            "\n📨 **Notification Rules:**",
            "\n🔧 **Other Related Rules:**",
        ]
        .into_iter()
        .enumerate()
        {
            let mut section = all.iter().filter(|(id, _)| class(id) == n).peekable();
            if section.peek().is_some() {
                results.push(header.to_string());
                results.extend(section.map(|(id, title)| format!("   • **{id}**: {title}")));
            }
        }
        results.push(format!(
            "\n📊 **Summary:** Found {} relevant rulesets",
            all.len()
        ));
        text(results.join("\n"))
    }

    async fn create_host_contactgroup_rule(&self, args: &Value) -> Result<Vec<Value>, String> {
        let contact_groups = arg_or(args, "contact_groups", json!([]));
        let host_conditions = arg_or(args, "host_conditions", json!({}));
        let comment = arg_or(args, "comment", json!("Host contact group assignment"));
        let folder = arg_or(args, "folder", json!("/"));
        if !truthy(&contact_groups) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("contact_groups list is required")));
        }
        let ruleset = "host_contactgroups";
        let f = folder.as_str().ok_or_else(|| {
            format!(
                "'{}' object has no attribute 'startswith'",
                py_type_name(&folder)
            )
        })?;
        // "/" -> "~", "/hosts/linux" -> "~hosts~linux"
        let api_folder = match f.strip_prefix('/') {
            Some("") => "~".to_string(),
            Some(rest) => format!("~{}", rest.replace('/', "~")),
            None => format!("~{}", f.replace('/', "~")),
        };
        let value_raw = match &contact_groups {
            Value::Array(a) if a.len() == 1 => format!("'{}'", ps(&a[0])),
            Value::Array(_) => format!("['{}']", py_join("', '", &contact_groups)?),
            other => format!("'{}'", ps(other)),
        };
        let conditions = if truthy(&host_conditions) {
            host_conditions.clone()
        } else {
            json!({})
        };
        let mut properties = Map::new();
        properties.insert("disabled".into(), json!(false));
        if truthy(&comment) {
            properties.insert("comment".into(), comment.clone());
        }
        let rule = json!({"properties": properties, "value_raw": value_raw, "conditions": conditions, "ruleset": ruleset, "folder": api_folder});
        let created = match self
            .base
            .client
            .post("domain-types/rule/collections/all", Some(&rule), &[])
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(self.base.error_response(
                    "Rule creation failed",
                    Some(&format!("Could not create contact group rule: {e}")),
                ))
            }
        };
        let rule_id = created
            .data
            .get("id")
            .map_or_else(|| "unknown".to_string(), ps);
        let cond = if truthy(&host_conditions) {
            py_repr(&host_conditions)
        } else {
            "None (applies to all hosts)".to_string()
        };
        Ok(text(format!(
            "✅ **Host Contact Group Rule Created Successfully**\n\nRuleset: {ruleset}\nRule ID: {rule_id}\nContact Groups: {}\nFolder: {}\nComment: {}\n\n📝 **Conditions:** {cond}\n\n⚠️ **Remember to activate changes!**",
            py_repr(&contact_groups),
            ps(&folder),
            ps(&comment)
        )))
    }

    async fn create_host_hostgroup_rule(&self, args: &Value) -> Result<Vec<Value>, String> {
        let host_groups = arg_or(args, "host_groups", json!([]));
        let host_conditions = arg_or(args, "host_conditions", json!({}));
        let comment = arg_or(args, "comment", json!("Host group assignment"));
        let folder = arg_or(args, "folder", json!("/"));
        if !truthy(&host_groups) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_groups list is required")));
        }
        let candidates = [
            "host_groups",
            "hostgroups",
            "host_group_assignment",
            "host_grouping",
        ];
        let mut ruleset = None;
        for c in candidates {
            let Ok(seg) = path_segment(c) else { continue };
            if self
                .base
                .client
                .get(&format!("objects/ruleset/{seg}"), None, &[])
                .await
                .is_ok()
            {
                ruleset = Some(c);
                break;
            }
        }
        let Some(ruleset) = ruleset else {
            let tried: Vec<String> = candidates.iter().map(|r| format!("• {r}")).collect();
            return Ok(self.base.error_text(&format!(
                "❌ **Host Group Ruleset Not Found**\n\nCould not find a working ruleset for host group assignment.\n\n**Tried rulesets:**\n{}\n\n**Recommendation:**\n1. Use 'find_host_grouping_rulesets' to find available rulesets\n2. Check existing host group rules in CheckMK GUI",
                tried.join("\n")
            )));
        };
        let fail = |e: String| {
            Ok(self.base.error_response(
                "Rule creation failed",
                Some(&format!("Could not create host group rule: {e}")),
            ))
        };
        let host_groups = match host_groups {
            Value::String(_) => json!([host_groups]),
            Value::Object(o) => json!(o.keys().collect::<Vec<_>>()),
            other => other,
        };
        let Some(each) = host_groups.as_array() else {
            return fail(format!(
                "'{}' object is not iterable",
                py_type_name(&host_groups)
            ));
        };
        // A host_groups rule holds ONE group as a Python string literal (a list is
        // rejected: "must be str"), so several groups mean one rule each.
        for group in each {
            let mut rule = json!({
                "ruleset": ruleset,
                "folder": folder,
                "properties": {"disabled": false, "description": "Host group assignment via vibeMK", "comment": comment},
                "value_raw": py_repr(group),
            });
            if truthy(&host_conditions) {
                rule["conditions"] = host_conditions.clone();
            }
            if let Err(e) = self
                .base
                .client
                .post("domain-types/rule/collections/all", Some(&rule), &[])
                .await
            {
                return fail(e.to_string());
            }
        }
        let groups = match py_join(", ", &host_groups) {
            Ok(g) => g,
            Err(e) => return fail(e),
        };
        let cond = if truthy(&host_conditions) {
            py_repr(&host_conditions)
        } else {
            "None (applies to all hosts)".to_string()
        };
        Ok(text(format!(
            "✅ **Host Group Assignment Rule Created**\n\nRuleset: {ruleset}\nHost Groups: {groups}\nFolder: {}\nComment: {}\n\n📝 **Conditions:** {cond}\n\n⚠️ **Remember to activate changes!**",
            ps(&folder),
            ps(&comment)
        )))
    }

    fn example_rule_structures(&self) -> Vec<Value> {
        text(EXAMPLES.join("\n"))
    }
}

const EXAMPLES: &[&str] = &[
    "📚 **Example Rule Structures for Host Grouping**\n",
    "\n🔧 **1. Host Contact Group Assignment (Swagger Format)**",
    "```json",
    "{",
    r#"  "extensions": {"#,
    r#"    "ruleset": "host_contactgroups","#,
    r#"    "folder": "/","#,
    r#"    "properties": {"#,
    r#"      "comment": "Critical hosts to admin teams""#,
    "    },",
    r#"    "value_raw": ["admins", "network-team"],"#,
    r#"    "conditions": {"#,
    r#"      "host_tags": ["#,
    "        {",
    r#"          "key": "criticality","#,
    r#"          "operator": "is","#,
    r#"          "value": "critical""#,
    "        }",
    "      ]",
    "    }",
    "  }",
    "}",
    "```",
    "\n🏠 **2. Host Group Assignment (Swagger Format)**",
    "```json",
    "{",
    r#"  "extensions": {"#,
    r#"    "ruleset": "host_groups","#,
    r#"    "folder": "/","#,
    r#"    "properties": {"#,
    r#"      "comment": "Database servers to appropriate groups""#,
    "    },",
    r#"    "value_raw": ["database-servers", "production"],"#,
    r#"    "conditions": {"#,
    r#"      "host_name": {"#,
    r#"        "match_on": ["db.*"],"#,
    r#"        "operator": "match_regex""#,
    "      }",
    "    }",
    "  }",
    "}",
    "```",
    "\n🔍 **3. Advanced Host Conditions (Swagger Format)**",
    "```json",
    "{",
    r#"  "extensions": {"#,
    r#"    "ruleset": "host_contactgroups","#,
    r#"    "folder": "/","#,
    r#"    "value_raw": ["web-admins"],"#,
    r#"    "conditions": {"#,
    r#"      "host_name": {"#,
    r#"        "match_on": ["web[0-9]+"],"#,
    r#"        "operator": "match_regex""#,
    "      },",
    r#"      "host_tags": ["#,
    "        {",
    r#"          "key": "environment","#,
    r#"          "operator": "is","#,
    r#"          "value": "production""#,
    "        },",
    "        {",
    r#"          "key": "location","#,
    r#"          "operator": "is","#,
    r#"          "value": "datacenter-1""#,
    "        }",
    "      ],",
    r#"      "host_label_groups": ["#,
    "        {",
    r#"          "label_group": ["#,
    "            {",
    r#"              "operator": "and","#,
    r#"              "label": "application:wordpress""#,
    "            }",
    "          ]",
    "        }",
    "      ]",
    "    }",
    "  }",
    "}",
    "```",
    "\n💡 **4. Simple All-Hosts Rule (Swagger Format)**",
    "```json",
    "{",
    r#"  "extensions": {"#,
    r#"    "ruleset": "host_contactgroups","#,
    r#"    "folder": "/","#,
    r#"    "properties": {"#,
    r#"      "comment": "Default contact group for all hosts""#,
    "    },",
    r#"    "value_raw": ["monitoring-team"]"#,
    "  }",
    "}",
    "```",
    "\n🎯 **Usage Tips (Updated for Swagger Format):**",
    "• All rule data must be under `extensions` object",
    "• Use `conditions` with proper operator format (match_on, operator)",
    "• `value_raw` contains the actual rule values (contact groups, host groups)",
    "• Empty conditions = rule applies to all hosts",
    "• Multiple groups can be assigned in one rule",
    "• Use `folder_index` for rule positioning (0 = top)",
    "• Remember to activate changes after creating rules",
];

#[async_trait]
impl Handler for HostGroupRulesHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_find_host_grouping_rulesets" => Ok(self.find_host_grouping_rulesets().await),
            "vibemk_create_host_contactgroup_rule" => {
                self.create_host_contactgroup_rule(args).await
            }
            "vibemk_create_host_hostgroup_rule" => self.create_host_hostgroup_rule(args).await,
            "vibemk_get_example_rule_structures" => Ok(self.example_rule_structures()),
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|e| self.base.error_response("Unexpected Error", Some(&e)))
    }
}
