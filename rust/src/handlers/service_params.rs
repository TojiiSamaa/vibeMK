//! Port of src/vibemk/handlers/service_params.py (ServiceParamsHandler).
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported. `value_raw` is a Python literal:
//! built by hand (tuples) and read back by `literal_eval` below, never evaluated.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::{path_segment, UnsafePathError};

enum Fail {
    Api(ApiError),
    Path(UnsafePathError),
    Other(String),
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
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}
/// `arguments.get(k)` with None for missing or null.
fn arg<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}
/// `arguments.get(k)` shown with str(); only called on truthy values.
fn show(v: &Value, k: &str) -> String {
    arg(v, k).map_or_else(String::new, ps)
}

/// Python `int(x)`.
fn py_int(v: &Value) -> Result<i64, Fail> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| Fail::Other("int too large".into())),
        Value::String(t) => t.trim().parse().map_err(|_| {
            Fail::Other(format!(
                "invalid literal for int() with base 10: {}",
                py_repr(v)
            ))
        }),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => Err(Fail::Other(
            "int() argument must be a string, a real number, or a number, not 'NoneType'".into(),
        )),
    }
}
/// Python `float(x)`.
fn py_float(v: &Value) -> Result<f64, Fail> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(t) => t
            .trim()
            .parse()
            .map_err(|_| Fail::Other(format!("could not convert string to float: {}", py_repr(v)))),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        _ => Err(Fail::Other(
            "float() argument must be a string or a real number, not 'NoneType'".into(),
        )),
    }
}
fn pair(a: f64, b: f64) -> String {
    format!("({a:?}, {b:?})")
}
fn quoted(t: &str) -> String {
    py_repr(&Value::String(t.to_string()))
}
/// `f"{n:,}"`.
fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

// ---- ast.literal_eval: dict, tuple, list, str, numbers, True/False/None; tuples become arrays ----

struct Lit<'a> {
    c: &'a [char],
    i: usize,
}
impl Lit<'_> {
    fn ws(&mut self) {
        while self.c.get(self.i).is_some_and(|c| c.is_whitespace()) {
            self.i += 1;
        }
    }
    fn eat(&mut self, ch: char) -> bool {
        self.ws();
        let hit = self.c.get(self.i) == Some(&ch);
        if hit {
            self.i += 1;
        }
        hit
    }
    fn items(&mut self, close: char, dict: bool) -> Option<Vec<Value>> {
        let mut out = vec![];
        loop {
            if self.eat(close) {
                return Some(out);
            }
            out.push(self.value()?);
            if dict {
                if !self.eat(':') {
                    return None;
                }
                out.push(self.value()?);
            }
            if !self.eat(',') && self.c.get(self.i) != Some(&close) {
                self.ws();
                if self.c.get(self.i) != Some(&close) {
                    return None;
                }
            }
        }
    }
    fn value(&mut self) -> Option<Value> {
        self.ws();
        let ch = *self.c.get(self.i)?;
        match ch {
            '{' => {
                self.i += 1;
                let kv = self.items('}', true)?;
                let mut m = Map::new();
                for p in kv.chunks(2) {
                    m.insert(p[0].as_str()?.to_string(), p[1].clone());
                }
                Some(Value::Object(m))
            }
            '[' | '(' => {
                self.i += 1;
                Some(Value::Array(
                    self.items(if ch == '[' { ']' } else { ')' }, false)?,
                ))
            }
            '\'' | '"' => {
                self.i += 1;
                let mut t = String::new();
                loop {
                    let c = *self.c.get(self.i)?;
                    self.i += 1;
                    match c {
                        c if c == ch => return Some(Value::String(t)),
                        '\\' => {
                            let e = *self.c.get(self.i)?;
                            self.i += 1;
                            match e {
                                'n' => t.push('\n'),
                                't' => t.push('\t'),
                                'r' => t.push('\r'),
                                '\\' | '\'' | '"' => t.push(e),
                                other => {
                                    t.push('\\');
                                    t.push(other);
                                }
                            }
                        }
                        c => t.push(c),
                    }
                }
            }
            c if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' => {
                let start = self.i;
                self.i += 1;
                while self.c.get(self.i).is_some_and(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_')
                }) {
                    self.i += 1;
                }
                let t: String = self.c[start..self.i]
                    .iter()
                    .filter(|c| **c != '_')
                    .collect();
                t.parse::<i64>()
                    .map(Value::from)
                    .ok()
                    .or_else(|| t.parse::<f64>().ok().map(Value::from))
            }
            _ => {
                let start = self.i;
                while self.c.get(self.i).is_some_and(|c| c.is_ascii_alphabetic()) {
                    self.i += 1;
                }
                match self.c[start..self.i].iter().collect::<String>().as_str() {
                    "True" => Some(Value::Bool(true)),
                    "False" => Some(Value::Bool(false)),
                    "None" => Some(Value::Null),
                    _ => None,
                }
            }
        }
    }
}
fn literal_eval(src: &str) -> Option<Value> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Lit { c: &chars, i: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == chars.len()).then_some(v)
}

pub struct ServiceParamsHandler {
    base: Base,
}

impl ServiceParamsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn resolve_folder(&self, hostname: &str, explicit: Option<&Value>) -> String {
        if truthy(explicit) {
            return explicit.map_or_else(String::new, ps);
        }
        let Ok(_) = path_segment(hostname) else {
            return "~".into();
        };
        let endpoint = format!(
            "objects/host_config/{}",
            path_segment(hostname).unwrap_or_default()
        );
        match self.base.client.get(&endpoint, None, &[]).await {
            Ok(r) => {
                let raw = r
                    .data
                    .pointer("/extensions/folder")
                    .map_or_else(|| "/".to_string(), ps);
                format!("~{}", raw.trim_start_matches('/').replace('/', "~"))
            }
            Err(_) => "~".into(),
        }
    }

    async fn discover_services(&self, hostname: &str) -> String {
        let run = async {
            let endpoint = format!(
                "objects/host/{}/actions/discover_services/invoke",
                path_segment(hostname).map_err(|e| e.to_string())?
            );
            self.base
                .client
                .post(&endpoint, Some(&json!({"mode": "refresh"})), &[])
                .await
                .map_err(|e| e.to_string())
        };
        match run.await {
            Ok(_) => format!(
                "✅ Service discovery run for '{hostname}' — the new thresholds are active."
            ),
            Err(e) => format!("⚠️  Service discovery failed: {e}"),
        }
    }

    /// POST a rule; returns its id.
    async fn create_rule(&self, body: Value) -> Result<String, Fail> {
        let r = self
            .base
            .client
            .post("domain-types/rule/collections/all", Some(&body), &[])
            .await?;
        Ok(r.data.get("id").map_or_else(String::new, ps))
    }

    async fn set_process_thresholds(&self, a: &Value) -> Out {
        let hostname = s(a, "hostname");
        let process_name = s(a, "process_name");
        let warn_min = py_int(a.get("warn_min").unwrap_or(&json!(1)))?;
        let crit_min = py_int(a.get("crit_min").unwrap_or(&json!(1)))?;
        let description = s(a, "description");
        let run_discovery = a.get("run_discovery").is_none_or(|v| truthy(Some(v)));
        let folder = self.resolve_folder(hostname, a.get("folder")).await;
        if hostname.is_empty() || process_name.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("hostname and process_name are required"),
            ));
        }
        let (Some(warn_max), Some(crit_max)) = (arg(a, "warn_max"), arg(a, "crit_max")) else {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("warn_max and crit_max are required"),
            ));
        };
        let proc = process_name
            .strip_prefix("Process ")
            .unwrap_or(process_name);
        let both = |x: &str, y: &str| truthy(a.get(x)) && truthy(a.get(y));
        let float_pair = |x: &str, y: &str| -> Result<String, Fail> {
            Ok(pair(py_float(&a[x])?, py_float(&a[y])?))
        };
        let mb_pair = |x: &str, y: &str| -> Result<String, Fail> {
            Ok(format!(
                "({}, {})",
                py_int(&a[x])? * 1024 * 1024,
                py_int(&a[y])? * 1024 * 1024
            ))
        };

        let mut params = format!(
            "{{'cpu_rescale_max': True, 'levels': ({warn_min}, {crit_min}, {}, {})",
            py_int(warn_max)?,
            py_int(crit_max)?
        );
        if both("cpu_warn_percent", "cpu_crit_percent") {
            params += &format!(
                ", 'cpulevels': {}",
                float_pair("cpu_warn_percent", "cpu_crit_percent")?
            );
        }
        if both("single_cpu_warn_percent", "single_cpu_crit_percent") {
            params += &format!(
                ", 'single_cpulevels': {}",
                float_pair("single_cpu_warn_percent", "single_cpu_crit_percent")?
            );
        }
        if truthy(a.get("cpu_average_min")) {
            params += &format!(", 'cpu_average': {}", py_int(&a["cpu_average_min"])?);
        }
        if both("mem_warn_mb", "mem_crit_mb") {
            params += &format!(
                ", 'virtual_levels': {}",
                mb_pair("mem_warn_mb", "mem_crit_mb")?
            );
        }
        if both("resident_warn_mb", "resident_crit_mb") {
            params += &format!(
                ", 'resident_levels': {}",
                mb_pair("resident_warn_mb", "resident_crit_mb")?
            );
        }
        params.push('}');
        let value_raw = format!(
            "{{'descr': {p}, 'match': {p}, 'default_params': {params}}}",
            p = quoted(proc)
        );

        let (warn_s, crit_s) = (ps(warn_max), ps(crit_max));
        let description = if description.is_empty() {
            format!("Process {proc}: warn>={warn_s}, crit>={crit_s}")
        } else {
            description.to_string()
        };
        let rule_id = self
            .create_rule(json!({
                "ruleset": "inventory_processes_rules",
                "folder": folder,
                "value_raw": value_raw,
                "conditions": {
                    "host_name": {"match_on": [hostname], "operator": "one_of"},
                    "host_tags": [], "host_label_groups": [], "service_label_groups": [],
                },
                "properties": {"disabled": false, "description": description},
            }))
            .await?;

        let (cpu_w, cpu_c) = (show(a, "cpu_warn_percent"), show(a, "cpu_crit_percent"));
        let (scpu_w, scpu_c) = (
            show(a, "single_cpu_warn_percent"),
            show(a, "single_cpu_crit_percent"),
        );
        let (mem_w, mem_c) = (show(a, "mem_warn_mb"), show(a, "mem_crit_mb"));
        let (res_w, res_c) = (show(a, "resident_warn_mb"), show(a, "resident_crit_mb"));
        let mut msg = format!("✅ Process thresholds for 'Process {proc}' on '{hostname}' created.\nMinimum: warn<{warn_min}, crit<{crit_min}\nMaximum: warn>={warn_s}, crit>={crit_s}\n");
        if truthy(a.get("cpu_warn_percent")) {
            msg += &format!("CPU total: warn {cpu_w}%, crit {cpu_c}%\n");
        }
        if truthy(a.get("single_cpu_warn_percent")) {
            msg += &format!("CPU per process: warn {scpu_w}%, crit {scpu_c}%\n");
        }
        if truthy(a.get("cpu_average_min")) {
            msg += &format!(
                "CPU averaged over: {} minutes\n",
                show(a, "cpu_average_min")
            );
        }
        if truthy(a.get("mem_warn_mb")) {
            msg += &format!("Virtual memory: warn {mem_w} MB, crit {mem_c} MB\n");
        }
        if truthy(a.get("resident_warn_mb")) {
            msg += &format!("Resident memory: warn {res_w} MB, crit {res_c} MB\n");
        }
        msg += &format!("Rule-ID: {rule_id}\n");
        if run_discovery {
            // Activation must happen first so the rule is visible during discovery
            msg += &self.base.run_activation().await;
            msg += "\n";
            msg += &self.discover_services(hostname).await;
        } else {
            msg += "⚠️  No service discovery has run yet — the thresholds take effect only after one does.";
        }
        Ok(text(msg))
    }

    async fn list_process_rules(&self, a: &Value) -> Out {
        let hostname = arg(a, "hostname").filter(|v| truthy(Some(v))).map(ps);
        let process_name = arg(a, "process_name")
            .filter(|v| truthy(Some(v)))
            .map(|v| ps(v).to_lowercase());
        let data = self
            .base
            .client
            .get(
                "domain-types/rule/collections/all",
                Some(&json!({"ruleset_name": "inventory_processes_rules"})),
                &[],
            )
            .await?
            .data;
        let rules = data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut lines =
            vec!["🔍 **Process monitoring rules (inventory_processes_rules)**\n".to_string()];
        for rule in &rules {
            let ext = rule.get("extensions").unwrap_or(&Value::Null);
            let hosts: Vec<String> = ext
                .pointer("/conditions/host_name/match_on")
                .and_then(Value::as_array)
                .map(|h| h.iter().map(ps).collect())
                .unwrap_or_default();
            let vr = ext.get("value_raw").map_or_else(String::new, ps);
            if hostname.as_ref().is_some_and(|h| !hosts.contains(h)) {
                continue;
            }
            if process_name
                .as_ref()
                .is_some_and(|p| !vr.to_lowercase().contains(p.as_str()))
            {
                continue;
            }
            let status = if truthy(ext.pointer("/properties/disabled")) {
                "🔴"
            } else {
                "🟢"
            };
            let hosts_str = if hosts.is_empty() {
                "all hosts".to_string()
            } else {
                hosts.join(", ")
            };
            // value_raw comes from the server: read it as a literal, never run it
            let parsed = literal_eval(&vr).and_then(|v| {
                let dp = v
                    .as_object()?
                    .get("default_params")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let dp = dp.as_object()?;
                let get = |k: &str| v.get(k).map_or_else(|| "?".to_string(), ps);
                let mut details = match dp.get("levels").filter(|l| truthy(Some(l))) {
                    Some(l) => {
                        let l = l.as_array().filter(|l| l.len() >= 4)?;
                        format!(
                            "min warn/crit: {}/{}, max warn/crit: {}/{}",
                            ps(&l[0]),
                            ps(&l[1]),
                            ps(&l[2]),
                            ps(&l[3])
                        )
                    }
                    None => "(no thresholds)".to_string(),
                };
                if let Some(c) = dp.get("cpulevels").filter(|c| truthy(Some(c))) {
                    let c = c.as_array().filter(|c| c.len() >= 2)?;
                    details += &format!(", CPU warn/crit: {}%/{}%", ps(&c[0]), ps(&c[1]));
                }
                Some((get("descr"), get("match"), details))
            });
            let (descr, matched, details) =
                parsed.unwrap_or_else(|| ("?".into(), "?".into(), vr.chars().take(120).collect()));
            let id = rule.get("id").ok_or_else(|| Fail::Other("'id'".into()))?;
            lines.push(format!("• {status} **Process {descr}** (match: `{matched}`) → {hosts_str}\n  ID: `{}`\n  {details}", ps(id)));
        }
        if lines.len() == 1 {
            lines.push("No process monitoring rules found.".into());
        }
        Ok(text(lines.join("\n")))
    }

    async fn set_interface_params(&self, a: &Value) -> Out {
        let hostname = s(a, "hostname");
        let interface_name = s(a, "interface_name");
        let description = s(a, "description");
        let folder = self.resolve_folder(hostname, a.get("folder")).await;
        if hostname.is_empty() || interface_name.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("hostname and interface_name are required"),
            ));
        }
        let Some(speed) = arg(a, "expected_speed_mbit") else {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("at least expected_speed_mbit is required"),
            ));
        };
        let service_desc = format!(
            "Interface {}",
            interface_name
                .strip_prefix("Interface ")
                .unwrap_or(interface_name)
        );
        let speed_mbit = py_int(speed)?;
        let speed_bits = speed_mbit * 1_000_000;
        let description = if description.is_empty() {
            format!("{service_desc} on {hostname}: expected speed {speed_mbit} Mbit/s")
        } else {
            description.to_string()
        };
        let rule_id = self
            .create_rule(json!({
                "ruleset": "checkgroup_parameters:interfaces",
                "folder": folder,
                "value_raw": format!("{{'speed': {speed_bits}}}"),
                "conditions": {
                    "host_name": {"match_on": [hostname], "operator": "one_of"},
                    "service_description": {"match_on": [service_desc], "operator": "one_of"},
                    "host_tags": [], "host_label_groups": [], "service_label_groups": [],
                },
                "properties": {"disabled": false, "description": description},
            }))
            .await?;
        Ok(text(format!(
            "✅ Interface parameters set for '{service_desc}' on '{hostname}'.\nExpected speed: {speed_mbit} Mbit/s ({} bits/s)\nRule-ID: {rule_id}\nℹ️  Activate changes for the rule to take effect.",
            thousands(speed_bits)
        )))
    }

    async fn set_memory_thresholds(&self, a: &Value) -> Out {
        let hostname = s(a, "hostname");
        let description = s(a, "description");
        let folder = self.resolve_folder(hostname, a.get("folder")).await;
        if hostname.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("hostname is required")));
        }
        if !truthy(a.get("ram_warn_percent")) && !truthy(a.get("swap_warn_percent")) {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("at least ram_warn_percent or swap_warn_percent required"),
            ));
        }
        let (ram_warn, ram_crit) = (arg(a, "ram_warn_percent"), arg(a, "ram_crit_percent"));
        let (swap_warn, swap_crit) = (arg(a, "swap_warn_percent"), arg(a, "swap_crit_percent"));
        let (mut entries, mut parts, mut lines) = (vec![], vec![], String::new());
        if let (Some(w), Some(c)) = (ram_warn, ram_crit) {
            entries.push(format!(
                "'levels_virtual': ('perc_used', {})",
                pair(py_float(w)?, py_float(c)?)
            ));
            parts.push(format!("RAM warn={}% crit={}%", ps(w), ps(c)));
            lines += &format!("RAM: warn>={}%, crit>={}%\n", ps(w), ps(c));
        }
        if let (Some(w), Some(c)) = (swap_warn, swap_crit) {
            entries.push(format!(
                "'levels_swap': ('perc_used', {})",
                pair(py_float(w)?, py_float(c)?)
            ));
            parts.push(format!("Swap warn={}% crit={}%", ps(w), ps(c)));
            lines += &format!("Swap: warn>={}%, crit>={}%\n", ps(w), ps(c));
        }
        let description = if description.is_empty() {
            format!("Memory {hostname}: {}", parts.join(", "))
        } else {
            description.to_string()
        };
        let rule_id = self
            .create_rule(json!({
                "ruleset": "checkgroup_parameters:memory_linux",
                "folder": folder,
                "value_raw": format!("{{{}}}", entries.join(", ")),
                "conditions": {
                    "host_name": {"match_on": [hostname], "operator": "one_of"},
                    "host_tags": [], "host_label_groups": [], "service_label_groups": [],
                },
                "properties": {"disabled": false, "description": description},
            }))
            .await?;
        Ok(text(format!("✅ Memory thresholds set for '{hostname}'.\n{lines}Rule-ID: {rule_id}\nℹ️  The rule is at the top, so it takes precedence over older rules.")))
    }

    async fn delete_service_param_rule(&self, a: &Value) -> Out {
        let rule_id = s(a, "rule_id");
        if rule_id.is_empty() {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("rule_id is required")));
        }
        self.base
            .client
            .delete(
                &format!("objects/rule/{}", path_segment(rule_id)?),
                None,
                &[],
            )
            .await?;
        Ok(text(format!("✅ Rule `{rule_id}` deleted.")))
    }
}

#[async_trait]
impl Handler for ServiceParamsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_set_process_thresholds" => self.set_process_thresholds(args).await,
            "vibemk_list_process_rules" => self.list_process_rules(args).await,
            "vibemk_set_interface_params" => self.set_interface_params(args).await,
            "vibemk_set_memory_thresholds" => self.set_memory_thresholds(args).await,
            "vibemk_delete_service_param_rule" => self.delete_service_param_rule(args).await,
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
            Fail::Other(m) => self.base.error_response("Unexpected Error", Some(&m)),
        })
    }
}
