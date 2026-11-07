//! Port of src/vibemk/handlers/rulesets.py (RulesetsHandler).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
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
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
/// `d.get(k, default)`: the default only when the key is missing (an explicit null stays null).
fn get_or<'a>(d: &'a Value, k: &str, default: &'a Value) -> &'a Value {
    d.get(k).unwrap_or(default)
}
/// `d.get(k, default)` rendered as text.
fn str_or(d: &Value, k: &str, default: &str) -> String {
    d.get(k).map_or_else(|| default.to_string(), ps)
}
/// `str(x).lower()` of a Python bool-ish.
fn lower(v: &Value) -> String {
    ps(v).to_lowercase()
}

pub struct RulesetsHandler {
    base: Base,
}

impl RulesetsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    /// The inner `except CheckMKError` of the three tools, keyed by HTTP status.
    fn status_error(
        &self,
        e: &ApiError,
        table: &[(u16, &str, String)],
        fallback: &str,
    ) -> Vec<Value> {
        let status = e.status_code.unwrap_or(0);
        match table.iter().find(|(s, _, _)| *s == status) {
            Some((_, title, detail)) => self.base.error_response(title, Some(detail)),
            None => self.base.error_response(fallback, Some(&e.to_string())),
        }
    }

    async fn search(&self, args: &Value) -> Out {
        let (empty, f, t) = (
            Value::String(String::new()),
            Value::Bool(false),
            Value::Bool(true),
        );
        let fulltext = get_or(args, "fulltext", &empty);
        let folder = get_or(args, "folder", &empty);
        let deprecated = get_or(args, "deprecated", &f);
        let used = get_or(args, "used", &t);
        let name = get_or(args, "name", &empty);

        let mut params = Map::new();
        if truthy(fulltext) {
            params.insert("fulltext".into(), fulltext.clone());
        }
        if truthy(folder) {
            params.insert("folder".into(), folder.clone());
        }
        if !deprecated.is_null() {
            params.insert("deprecated".into(), json!(lower(deprecated)));
        }
        if !used.is_null() {
            params.insert("used".into(), json!(lower(used)));
        }
        if truthy(name) {
            params.insert("name".into(), name.clone());
        }
        let params = Value::Object(params);

        let result = match self
            .base
            .client
            .get("domain-types/ruleset/collections/all", Some(&params), &[])
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Python: getattr(e, "error_data", {}) is always {}, so the detail is str(e).
                let detail = e.to_string();
                return Ok(self.status_error(
                    &e,
                    &[
                        (400, "Search Parameter Error", format!("Invalid search parameters: {detail}")),
                        (403, "Permission Denied", "Access denied. You may not have 'wato.rulesets' permission for ruleset management.".into()),
                        (406, "Accept Header Error", "API cannot satisfy the requested content type. Check Accept headers.".into()),
                    ],
                    "Ruleset Search Failed",
                ));
            }
        };
        let rulesets = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);

        if rulesets.is_empty() {
            let mut criteria = vec![];
            if truthy(fulltext) {
                criteria.push(format!("text '{}'", ps(fulltext)));
            }
            if truthy(folder) {
                criteria.push(format!("folder '{}'", ps(folder)));
            }
            if truthy(name) {
                criteria.push(format!("name '{}'", ps(name)));
            }
            let criteria = if criteria.is_empty() {
                "your criteria".to_string()
            } else {
                criteria.join(" and ")
            };
            return Ok(text(format!(
                "🔍 **No Rulesets Found**\n\nNo rulesets match {criteria}.\n\n💡 **Tips:**\n• Try broader search terms\n• Check if deprecated rulesets should be included\n• Verify folder path is correct"
            )));
        }
        Ok(text(Self::format_search(rulesets, &params)))
    }

    async fn show(&self, args: &Value) -> Out {
        let name = args.get("ruleset_name").unwrap_or(&NULL);
        if !truthy(name) {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("ruleset_name is required")));
        }
        let name = ps(name);
        let seg = path_segment(&name).map_err(|e| Fail::Other(e.to_string()))?;
        match self.base.client.get(&format!("objects/ruleset/{seg}"), None, &[]).await {
            Ok(r) => Ok(text(Self::format_details(&name, &r.data))),
            Err(e) => Ok(self.status_error(
                &e,
                &[
                    (403, "Permission Denied", "Access denied. You may not have 'wato.rulesets' permission for ruleset management.".into()),
                    (404, "Ruleset Not Found", format!("Ruleset '{name}' not found. Check the ruleset name and try again.")),
                    (406, "Accept Header Error", "API cannot satisfy the requested content type. Check Accept headers.".into()),
                ],
                "Failed to retrieve ruleset",
            )),
        }
    }

    async fn list(&self, args: &Value) -> Out {
        let limit = args.get("limit").cloned().unwrap_or_else(|| json!(50));
        let show_deprecated = args
            .get("show_deprecated")
            .cloned()
            .unwrap_or(Value::Bool(false));
        let params = json!({"used": "true", "deprecated": lower(&show_deprecated)});

        let result = match self
            .base
            .client
            .get("domain-types/ruleset/collections/all", Some(&params), &[])
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(self.status_error(
                    &e,
                    &[(
                        403,
                        "Permission Denied",
                        "Access denied. You may not have 'wato.rulesets' permission.".into(),
                    )],
                    "Failed to list rulesets",
                ))
            }
        };
        let mut rulesets: Vec<Value> = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut truncated = false;
        if let (true, Some(n)) = (truthy(&limit), limit.as_i64()) {
            let len = rulesets.len() as i64;
            if len > n {
                // Python `rulesets[:n]`: a negative n drops |n| from the end.
                rulesets.truncate(if n >= 0 { n } else { (len + n).max(0) } as usize);
                truncated = true;
            }
        }
        Ok(text(Self::format_list(
            &rulesets,
            &limit,
            truncated,
            truthy(&show_deprecated),
        )))
    }

    fn format_search(rulesets: &[Value], params: &Value) -> String {
        let mut r = "🔍 **Ruleset Search Results**\n\n".to_string();
        if let Some(p) = params.as_object().filter(|p| !p.is_empty()) {
            r.push_str("**Search Criteria:**\n");
            for (k, v) in p {
                r.push_str(&format!("• {k}: {}\n", ps(v)));
            }
            r.push('\n');
        }
        r.push_str(&format!("**Found {} rulesets**\n\n", rulesets.len()));
        for (i, rs) in rulesets.iter().take(20).enumerate() {
            let ext = rs.get("extensions").unwrap_or(&NULL);
            let id = str_or(rs, "id", "Unknown");
            let title = str_or(ext, "title", "No title");
            let mut help = str_or(ext, "help", "");
            let deprecated = truthy(get_or(ext, "deprecated", &NULL));
            let n = ext
                .get("number_of_rules")
                .map_or_else(|| "0".to_string(), ps);
            if help.chars().count() > 100 {
                help = format!("{}...", help.chars().take(100).collect::<String>());
            }
            let icon = if deprecated { "⚠️" } else { "✅" };
            r.push_str(&format!(
                "**{}. {icon} {id}**\n   📋 Title: {title}\n   📊 Rules: {n}\n",
                i + 1
            ));
            if !help.is_empty() {
                r.push_str(&format!("   💡 {help}\n"));
            }
            if deprecated {
                r.push_str("   ⚠️ Deprecated ruleset\n");
            }
            r.push('\n');
        }
        if rulesets.len() > 20 {
            r.push_str(&format!(
                "... and {} more rulesets\n\n",
                rulesets.len() - 20
            ));
        }
        r.push_str("💡 **Use `vibemk_show_ruleset` with ruleset name for detailed information**");
        r
    }

    fn format_details(name: &str, data: &Value) -> String {
        let ext = data.get("extensions").unwrap_or(&NULL);
        // objects/ruleset/{name}: title at the top level, extensions only has
        // name and number_of_rules (help and match_type are collection-only).
        let title = str_or(data, "title", "No title");
        let deprecated = truthy(get_or(ext, "deprecated", &NULL));
        let n = ext
            .get("number_of_rules")
            .map_or_else(|| "0".to_string(), ps);
        let mut r = format!("📋 **Ruleset Details: {name}**\n\n");
        if deprecated {
            r.push_str("⚠️ **DEPRECATED RULESET**\n\n");
        }
        r.push_str(&format!("**Title:** {title}\n**Number of Rules:** {n}\n\n"));
        r.push_str("**Available Operations:**\n");
        r.push_str("• Use `vibemk_get_ruleset` to see actual rules in this ruleset\n");
        r.push_str("• Use `vibemk_create_rule` to add new rules to this ruleset\n");
        r.push_str("• Use `vibemk_search_rulesets` to find related rulesets\n");
        if deprecated {
            r.push_str(
                "\n⚠️ **Note:** This is a deprecated ruleset. Consider using newer alternatives.",
            );
        }
        r
    }

    fn format_list(
        rulesets: &[Value],
        limit: &Value,
        truncated: bool,
        show_deprecated: bool,
    ) -> String {
        let mut r = "📋 **Available Rulesets**\n\n".to_string();
        if show_deprecated {
            r.push_str("**Including deprecated rulesets**\n\n");
        }
        let limited = if truncated {
            format!(" (limited to {})", ps(limit))
        } else {
            String::new()
        };
        r.push_str(&format!(
            "**Showing {} rulesets{limited}**\n\n",
            rulesets.len()
        ));

        let mut categorized: BTreeMap<String, Vec<(String, String, bool)>> = BTreeMap::new();
        for rs in rulesets {
            let ext = rs.get("extensions").unwrap_or(&NULL);
            let id = str_or(rs, "id", "Unknown");
            let category = if id.contains('_') {
                id.split('_').next().unwrap_or("").to_string()
            } else {
                "general".to_string()
            };
            categorized.entry(category).or_default().push((
                id,
                str_or(ext, "title", "No title"),
                truthy(get_or(ext, "deprecated", &NULL)),
            ));
        }
        for (category, rules) in &categorized {
            if rules.len() > 1 {
                r.push_str(&format!("**{}:**\n", category.to_uppercase()));
                for (id, title, dep) in rules.iter().take(5) {
                    r.push_str(&format!(
                        "  {} `{id}` - {title}\n",
                        if *dep { "⚠️" } else { "✅" }
                    ));
                }
                if rules.len() > 5 {
                    r.push_str(&format!(
                        "  ... and {} more {category} rulesets\n",
                        rules.len() - 5
                    ));
                }
                r.push('\n');
            }
        }
        if truncated {
            r.push_str(&format!("\n⚠️ **Note:** Results limited to {}. Use search filters for more specific results.\n", ps(limit)));
        }
        r.push_str("\n💡 **Usage:**\n");
        r.push_str("• `vibemk_show_ruleset` - Get detailed information about a specific ruleset\n");
        r.push_str("• `vibemk_search_rulesets` - Search rulesets with filters\n");
        r.push_str("• `vibemk_get_ruleset` - View rules within a ruleset\n");
        r
    }
}

#[async_trait]
impl Handler for RulesetsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_search_rulesets" => self.search(args).await,
            "vibemk_show_ruleset" => self.show(args).await,
            "vibemk_list_rulesets" => self.list(args).await,
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
