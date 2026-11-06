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
/// JSON has no tuple, and many valuespecs demand one: a string that already parses as a
/// Python literal, such as "{'levels': (38.0, 45.0)}", is passed through as the literal itself.
fn value_raw(value: &Value) -> String {
    match value {
        Value::Array(a) if a.len() == 1 => py_repr(&a[0]),
        Value::String(s) if is_py_literal(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `ast.literal_eval(s)` succeeds: strings, numbers, True/False/None, tuples, lists,
/// dicts, sets and set(), with Python's leading-space stripping.
/// shortcut: hashability of dict keys / set items is not checked (Python raises TypeError).
fn is_py_literal(s: &str) -> bool {
    let mut p = Lit {
        // shortcut: all leading whitespace goes, where Python refuses an indented line after a blank one.
        b: s.trim_start().as_bytes(),
        i: 0,
    };
    p.expr() && {
        p.ws(true);
        p.i == p.b.len()
    }
}

struct Lit<'a> {
    b: &'a [u8],
    i: usize,
}

impl Lit<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    /// Skip spaces; newlines only inside brackets (or trailing, where `nl` is set too).
    fn ws(&mut self, nl: bool) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\x0c' => self.i += 1,
                b'\n' | b'\r' if nl => self.i += 1,
                b'\\' if self.b.get(self.i + 1) == Some(&b'\n') => self.i += 2,
                b'#' => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn eat(&mut self, c: u8, nl: bool) -> bool {
        self.ws(nl);
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn word(&mut self, w: &str) -> bool {
        let end = self.i + w.len();
        let next_is_ident = self
            .b
            .get(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        if self.b.get(self.i..end) == Some(w.as_bytes()) && !next_is_ident {
            self.i = end;
            true
        } else {
            false
        }
    }
    /// One literal value, with `nl` telling whether newlines are allowed around it.
    fn value(&mut self, nl: bool) -> bool {
        self.ws(nl);
        let Some(c) = self.peek() else { return false };
        match c {
            b'(' | b'[' | b'{' => self.container(c),
            b'+' | b'-' => {
                self.i += 1;
                self.ws(nl);
                self.signed(nl)
            }
            b'0'..=b'9' | b'.' => self.number(nl),
            _ => {
                if self.word("True") || self.word("False") || self.word("None") {
                    return true;
                }
                if self.word("set") {
                    return self.eat(b'(', nl) && self.eat(b')', true);
                }
                let mut ok = false;
                while self.string() {
                    ok = true; // adjacent strings concatenate
                    self.ws(nl);
                }
                ok
            }
        }
    }
    fn expr(&mut self) -> bool {
        // A bare top-level "1, 2" is a tuple too.
        self.value(false) && {
            let mut ok = true;
            while ok && self.eat(b',', false) {
                self.ws(false);
                if self.i == self.b.len() || matches!(self.peek(), Some(b'\n' | b'\r')) {
                    break;
                }
                ok = self.value(false);
            }
            ok
        }
    }
    fn container(&mut self, open: u8) -> bool {
        self.i += 1;
        let close = match open {
            b'(' => b')',
            b'[' => b']',
            _ => b'}',
        };
        if self.eat(close, true) {
            return true; // (), [], {}
        }
        let mut is_dict = None;
        loop {
            if !self.value(true) {
                return false;
            }
            if open == b'{' {
                let colon = self.eat(b':', true);
                if *is_dict.get_or_insert(colon) != colon || (colon && !self.value(true)) {
                    return false;
                }
            }
            if self.eat(close, true) {
                return true;
            }
            if !self.eat(b',', true) {
                return false;
            }
            if self.eat(close, true) {
                return true; // trailing comma
            }
        }
    }
    /// A number, or a complex "real ± imaginary".
    fn number(&mut self, nl: bool) -> bool {
        let Some(imaginary) = self.num_token() else {
            return false;
        };
        if !imaginary {
            let save = self.i;
            self.ws(nl);
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
                self.ws(nl);
                return self.num_token() == Some(true);
            }
            self.i = save;
        }
        true
    }
    /// The operand of a unary sign: a number, or one in parentheses ("-(1)").
    fn signed(&mut self, nl: bool) -> bool {
        if self.peek() == Some(b'(') {
            self.i += 1;
            self.ws(true);
            return self.num_token().is_some() && self.eat(b')', true);
        }
        self.number(nl)
    }
    /// One int/float/imaginary token: Some(is_imaginary) when Python accepts it.
    fn num_token(&mut self) -> Option<bool> {
        let start = self.i;
        while let Some(c) = self.peek() {
            let exp_sign =
                matches!(c, b'+' | b'-') && matches!(self.b.get(self.i - 1), Some(b'e' | b'E'));
            if c.is_ascii_alphanumeric()
                || c == b'_'
                || c == b'.'
                || (exp_sign && !self.is_hex(start))
            {
                self.i += 1;
            } else {
                break;
            }
        }
        let tok = &self.b[start..self.i];
        let hex = self.is_hex(start);
        let digit = |c: Option<&u8>| {
            c.is_some_and(|c| {
                if hex {
                    c.is_ascii_hexdigit()
                } else {
                    c.is_ascii_digit()
                }
            })
        };
        // An underscore sits between two digits (or right after a 0x/0o/0b prefix).
        let underscores_ok = tok
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == b'_')
            .all(|(p, _)| {
                (digit(tok.get(p.wrapping_sub(1))) || (p == 2 && tok[0] == b'0'))
                    && digit(tok.get(p + 1))
            });
        let tok = String::from_utf8_lossy(tok).replace('_', "");
        (underscores_ok && valid_number(&tok)).then(|| tok.ends_with(['j', 'J']))
    }
    fn is_hex(&self, start: usize) -> bool {
        matches!(self.b.get(start..start + 2), Some(b"0x" | b"0X"))
    }
    fn string(&mut self) -> bool {
        let start = self.i;
        let mut j = self.i;
        while j < self.b.len() && j - start < 2 && b"rRbBuU".contains(&self.b[j]) {
            j += 1;
        }
        let Some(&q) = self.b.get(j).filter(|c| **c == b'\'' || **c == b'"') else {
            self.i = start;
            return false;
        };
        let triple = self.b.get(j..j + 3) == Some(&[q, q, q]);
        let mut k = j + if triple { 3 } else { 1 };
        while k < self.b.len() {
            match self.b[k] {
                b'\\' => k += 2,
                b'\n' if !triple => return false,
                c if c == q && (!triple || self.b.get(k..k + 3) == Some(&[q, q, q])) => {
                    self.i = k + if triple { 3 } else { 1 };
                    return true;
                }
                _ => k += 1,
            }
        }
        false
    }
}

/// A Python int, float or imaginary token (underscores already removed).
fn valid_number(t: &str) -> bool {
    let imaginary = t.ends_with(['j', 'J']);
    let t = t.strip_suffix(['j', 'J']).unwrap_or(t);
    let lower = t.to_ascii_lowercase();
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(d) = lower.strip_prefix(prefix) {
            return !d.is_empty() && u128::from_str_radix(d, radix).is_ok();
        }
    }
    let digits = lower.chars().all(|c| c.is_ascii_digit());
    if digits && !lower.is_empty() {
        // Python refuses leading zeros on a non-zero int ("010").
        return imaginary || !lower.starts_with('0') || lower.bytes().all(|c| c == b'0');
    }
    lower.chars().any(|c| c.is_ascii_digit())
        && lower.parse::<f64>().is_ok()
        && !lower.contains("inf")
        && !lower.contains("nan")
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

        let rule_config = arg(args, "rule_config");
        let conditions = arg(args, "conditions");
        let mut props = Map::new();
        for key in ["comment", "disabled"] {
            if let Some(v) = args.get(key).filter(|v| !v.is_null()) {
                props.insert(key.into(), v.clone());
            }
        }
        let fields = [
            ("value_raw", rule_config.is_some()),
            ("conditions", conditions.is_some()),
            ("properties", !props.is_empty()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect::<Vec<_>>()
        .join(", ");
        if fields.is_empty() {
            return Ok(self.base.error_response(
                "No data to update",
                Some("At least one field must be provided"),
            ));
        }

        // CheckMK's PUT replaces the whole rule (value_raw as a Python literal, conditions,
        // properties all required): start from the stored rule, change only what was asked.
        let endpoint = format!("objects/rule/{}", path_segment(&rule_id_s)?);
        let current = match self.base.client.get(&endpoint, None, &[]).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(self.base.error_response(
                    "Rule update failed",
                    Some(&format!("Could not read rule '{rule_id_s}': {e}")),
                ))
            }
        };
        let stored = current
            .data
            .get("extensions")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut properties = stored
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        properties.extend(props);
        let data = json!({
            "value_raw": rule_config.map_or_else(|| stored.get("value_raw").cloned().unwrap_or(Value::Null), |c| json!(value_raw(c))),
            "conditions": conditions.cloned().or_else(|| stored.get("conditions").cloned()).unwrap_or_else(|| json!({})),
            "properties": properties,
        });
        let headers = vec![("If-Match".to_string(), Base::extract_etag(&current))];
        self.base
            .client
            .put(&endpoint, Some(&data), &headers)
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

#[cfg(test)]
mod literal_tests {
    use super::is_py_literal;

    /// Expected outcomes are what ast.literal_eval returned on CPython 3.13 for each input.
    #[test]
    fn agrees_with_ast_literal_eval() {
        for (input, accepted) in [
            ("{'levels': (38.0, 45.0)}", true),
            ("('fixed', (0.5, 0.8))", true),
            ("123", true),
            ("-1.5e-3", true),
            ("1+2j", true),
            ("0x1F", true),
            ("[1, 2,]", true),
            ("{1, 2}", true),
            ("set()", true),
            ("{}", true),
            ("()", true),
            ("(1,)", true),
            ("1, 2", true),
            ("'a' \"b\"", true),
            ("  'x'", true),
            ("True", true),
            ("None", true),
            ("{'a': [None, False], 'b': {'c': ()}}", true),
            ("'it\\'s'", true),
            ("\"\"\"multi\nline\"\"\"", true),
            ("[\n 1,\n 2\n]", true),
            ("'x'\n", true),
            ("", false),
            ("hello", false),
            ("web-servers", false),
            ("foo bar", false),
            ("{'a': 1", false),
            ("{'a': 1, 2}", false),
            ("1 2", false),
            ("010", false),
            ("inf", false),
            ("nan", false),
            ("f'x'", false),
            ("'unterminated", false),
            ("Truex", false),
            ("x()", false),
            ("1 + 2", false),
            ("\n1", true),
            ("1,\n2", false),
            ("{'a' 1}", false),
            ("[1,,2]", false),
            ("(,)", false),
            ("-True", false),
            ("1e5", true),
            ("1_000", true),
            ("0_0", true),
            ("00", true),
            (".5", true),
            ("5.", true),
            ("1e", false),
            ("0x", false),
            ("-(1)", true),
            ("- 1", true),
            ("(1)", true),
            ("((1,2))", true),
            ("1,", true),
            ("set( )", true),
            ("'a''b'", true),
            ("1e+5", true),
            ("1E-5j", true),
            ("-1-2j", true),
            ("1+2", false),
            ("2j+1", false),
            ("[1, 2] ", true),
            ("\t5", true),
            (" \n5", true),
            ("5 # c", true),
            ("b'x'", true),
            ("rb'x'", true),
            ("u'x'", true),
            ("{'a': 1,}", true),
            ("{1: 2, 3}", false),
            ("set(1)", false),
            ("0b102", false),
            ("0o7", true),
            ("1__0", false),
            ("_1", false),
            ("1_", false),
            ("'a\\\nb'", true),
            ("(1\n,2)", true),
            ("1\\\n+2j", true),
            ("'''a'''", true),
            ("-0x10", true),
            ("+5", true),
            ("~5", false),
            ("not True", false),
            ("[*a]", false),
        ] {
            assert_eq!(is_py_literal(input), accepted, "{input:?}");
        }
    }
}
