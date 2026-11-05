//! Port of src/vibemk/handlers/hosts.py (HostHandler) and utils/folder_validator.py.
//!
//! The Rust client returns `Err` for every non-2xx answer, so the Python `if not success`
//! branches (dead since client.py raises) are not ported; the `Err` paths keep the Python wording.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, ApiErrorKind, CheckMkClient};
use crate::handlers::base::{py_repr, time_ago, Base, Handler};
use crate::paths::{path_segment, UnsafePathError};

static NULL: Value = Value::Null;

/// What a Python handler would have raised: a CheckMKError, or a ValueError from `path_segment`.
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

/// Python `str()` of a JSON value.
fn ps(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
/// Argument as a string, "" when missing or not a string (Python: falsy).
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn field<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}
/// `v.get(k, default)` rendered with str().
fn or(v: &Value, k: &str, default: &str) -> String {
    v.get(k).map_or_else(|| default.to_string(), ps)
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
fn map_of(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}
fn attrs_of(data: &Value) -> Map<String, Value> {
    data.pointer("/extensions/attributes")
        .map(map_of)
        .unwrap_or_default()
}
fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}
fn state_name(state: &Value) -> Option<&'static str> {
    match state.as_i64() {
        Some(0) => Some("UP"),
        Some(1) => Some("DOWN"),
        Some(2) => Some("UNREACHABLE"),
        _ => None,
    }
}
/// The structured payload names only the states a caller can branch on.
fn canonical_state(state: &Value) -> &'static str {
    state_name(state).unwrap_or("UNKNOWN")
}
fn status_display(status: &str) -> String {
    let emoji = match status {
        "UP" => "🟢",
        "DOWN" => "🔴",
        "UNREACHABLE" => "🟡",
        _ => "⚪",
    };
    format!("{emoji} **{status}**")
}
fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

fn valid_chars(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}
fn valid_host_name(n: &str) -> bool {
    valid_chars(n, &['.', '_', '-'])
}
fn valid_ip(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|x| x.parse::<std::net::IpAddr>().is_ok())
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

// ---- attribute comparison ----

#[derive(Default)]
struct Changes {
    added: Map<String, Value>,
    modified: Vec<(String, Value, Value)>,
    removed: Map<String, Value>,
}
impl Changes {
    fn has_changes(&self) -> bool {
        !(self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty())
    }
    fn format(&self) -> String {
        let mut out = String::new();
        if !self.added.is_empty() {
            out.push_str("**Added:**\n");
            self.added
                .iter()
                .for_each(|(k, v)| out.push_str(&format!("• {k}: {}\n", ps(v))));
        }
        if !self.modified.is_empty() {
            out.push_str("**Modified:**\n");
            self.modified.iter().for_each(|(k, old, new)| {
                out.push_str(&format!("• {k}: {} → {}\n", ps(old), ps(new)))
            });
        }
        if !self.removed.is_empty() {
            out.push_str("**Removed:**\n");
            self.removed
                .iter()
                .for_each(|(k, v)| out.push_str(&format!("• {k}: {}\n", ps(v))));
        }
        out
    }
}
fn compare_attributes(current: &Map<String, Value>, desired: &Map<String, Value>) -> Changes {
    let mut c = Changes::default();
    for (k, v) in desired {
        match current.get(k) {
            None => {
                c.added.insert(k.clone(), v.clone());
            }
            Some(old) if old != v => c.modified.push((k.clone(), old.clone(), v.clone())),
            _ => {}
        }
    }
    for (k, v) in current {
        if !desired.contains_key(k) {
            c.removed.insert(k.clone(), v.clone());
        }
    }
    c
}

/// Python `json.dumps(v, indent=2)`: ensure_ascii escapes everything above U+007F.
fn json_dumps_indent2(v: &Value) -> String {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = String::with_capacity(pretty.len());
    let mut buf = [0u16; 2];
    for c in pretty.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            c.encode_utf16(&mut buf)
                .iter()
                .for_each(|u| out.push_str(&format!("\\u{u:04x}")));
        }
    }
    out
}

fn validate_update_attributes(attributes: &Map<String, Value>) -> Vec<String> {
    let mut errors = vec![];
    if let Some(ip) = attributes.get("ipaddress") {
        if !valid_ip(ip) {
            errors.push(format!("Invalid IP address format: '{}'", ps(ip)));
        }
    }
    if let Some(site) = attributes.get("site") {
        match site.as_str() {
            Some(x) if !x.trim().is_empty() => {
                if !valid_chars(x, &['_']) {
                    errors.push(format!("Invalid site name format: '{x}'. Must contain only letters, numbers, and underscores"));
                }
            }
            _ => errors.push("Site must be a non-empty string".into()),
        }
    }
    if let Some(alias) = attributes.get("alias") {
        match alias.as_str() {
            None => errors.push("Alias must be a string".into()),
            Some(a) if a.chars().count() > 255 => {
                errors.push("Alias cannot be longer than 255 characters".into())
            }
            _ => {}
        }
    }
    for (key, value) in attributes {
        if let Some(tag) = key.strip_prefix("tag_") {
            if !valid_chars(tag, &['_']) {
                errors.push(format!("Invalid tag name: '{tag}'. Must contain only letters, numbers, and underscores"));
            }
            if !value.is_string() {
                errors.push(format!("Tag value for '{key}' must be a string"));
            }
        } else if key == "tag" || key == "tags" {
            errors.push(format!(
                "Tag attributes must be prefixed with 'tag_'. Use 'tag_{key}' instead of '{key}'"
            ));
        }
    }
    errors
}

pub struct HostHandler {
    base: Base,
}

impl HostHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn client(&self) -> &CheckMkClient {
        &self.base.client
    }

    fn missing(&self, what: &str) -> Vec<Value> {
        self.base
            .error_response("Missing parameter", Some(&format!("{what} is required")))
    }

    async fn get_hosts(&self) -> Out {
        // Monitoring collection, not Setup's host_config (empty for a read-only account); 'state' asked for explicitly.
        let params = json!({"columns": ["name", "state"]});
        let r = self
            .client()
            .get("domain-types/host/collections/all", Some(&params), &[])
            .await?;
        let hosts = list(&r.data, "value");
        if hosts.is_empty() {
            return Ok(self
                .base
                .structured_response("📭 No hosts found", json!({"total": 0, "hosts": []})));
        }
        let (mut lines, mut entries) = (vec![], vec![]);
        for host in hosts.iter().take(50) {
            let ext = field(host, "extensions");
            let id = host
                .get("id")
                .filter(|v| truthy(v))
                .map_or_else(|| or(ext, "name", "Unknown"), ps);
            let state = field(ext, "state");
            lines.push(format!(
                "🖥️ {id} ({})",
                state_name(state).unwrap_or("UNKNOWN")
            ));
            entries.push(json!({"host_name": id, "state": canonical_state(state)}));
        }
        Ok(self.base.structured_response(
            &format!(
                "🖥️ **CheckMK Hosts** ({} total, showing first {}):\n\n{}",
                hosts.len(),
                lines.len(),
                lines.join("\n")
            ),
            json!({"total": hosts.len(), "hosts": entries}),
        ))
    }

    async fn get_host_status(&self, host: &str) -> Out {
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        // Method 1: the object itself, with the monitoring columns.
        if let Ok(r) = self.status_direct(host).await {
            return Ok(r);
        }
        // Method 2: find it in the host collection.
        if let Ok(r) = self
            .client()
            .get("domain-types/host/collections/all", None, &[])
            .await
        {
            if let Some(hosts) = r.data.get("value").and_then(Value::as_array) {
                for h in hosts
                    .iter()
                    .filter(|h| h.get("id").and_then(Value::as_str) == Some(host))
                {
                    let state = h.pointer("/extensions/state").unwrap_or(&NULL);
                    if !state.is_null() {
                        let status = state_name(state)
                            .map_or_else(|| format!("UNKNOWN({})", ps(state)), str::to_string);
                        return Ok(self.base.structured_response(
                            &format!("✅ **Host Status: {host}** (Fallback Method)\n\n**Status:** {}\n**State Code:** {}\n\n✅ **Data from CheckMK host collections API**", status_display(&status), ps(state)),
                            json!({"host_name": host, "state": canonical_state(state), "state_code": state, "is_hard_state": false, "has_been_checked": true, "plugin_output": "", "last_check": null, "last_state_change": null}),
                        ));
                    }
                }
                return Ok(self.base.error_response(
                    "Host not found",
                    Some(&format!("Host '{host}' not found in host collections")),
                ));
            }
        }
        // Method 3: is it at least configured?
        if self.host_config(host).await.is_ok() {
            return Ok(self.base.structured_response(
                    &format!("⚪ **Host Status: {host}**\n\n**Status:** MONITORING DATA UNAVAILABLE\n\n✅ Host is configured in CheckMK\n❌ Live monitoring state not accessible\n\n**Possible Issues:**\n• Host not actively monitored\n• Monitoring core not running\n• API permissions insufficient\n\n**Recommendation:**\nCheck CheckMK GUI for actual status"),
                    json!({"host_name": host, "state": "UNKNOWN", "state_code": -1, "is_hard_state": false, "has_been_checked": false, "plugin_output": "", "last_check": null, "last_state_change": null}),
            ));
        }
        Ok(self.base.error_text(&format!(
            "❌ **Host Status Retrieval Failed**\n\nHost: {host}\n\n**Tried Methods:**\n1️⃣ Direct host object API (objects/host/)\n2️⃣ Host collections query (real-time data)\n3️⃣ Host configuration check\n\n**Possible Issues:**\n• Host not found in monitoring system\n• Host name mismatch\n• CheckMK API version compatibility\n• Monitoring data not yet available\n\n**Recommendation:**\nVerify the host exists in CheckMK GUI and is being monitored."
        )))
    }

    async fn status_direct(&self, host: &str) -> Out {
        let cols = [
            "name",
            "state",
            "hard_state",
            "state_type",
            "plugin_output",
            "last_check",
            "last_state_change",
            "has_been_checked",
        ];
        let r = self
            .client()
            .get(
                &format!("objects/host/{}", path_segment(host)?),
                Some(&json!({"columns": cols})),
                &[],
            )
            .await?;
        let Some(ext) = r.data.get("extensions") else {
            return Ok(self.base.error_response(
                "Unexpected response",
                Some("Host data structure not as expected"),
            ));
        };
        let (state, hard_state, state_type) = (
            field(ext, "state"),
            field(ext, "hard_state"),
            field(ext, "state_type"),
        );
        let is_hard = state_type.as_i64() == Some(1);
        // The hard state is the real monitoring status once CheckMK has settled on it.
        let (effective, info) = if !hard_state.is_null() && is_hard {
            (hard_state, format!("Hard State: {}", ps(hard_state)))
        } else if !state.is_null() {
            (state, format!("Soft State: {}", ps(state)))
        } else {
            return Ok(self.base.error_response(
                "No state data",
                Some(&format!(
                    "Host '{host}' found but no state information available"
                )),
            ));
        };
        let status = state_name(effective)
            .map_or_else(|| format!("UNKNOWN({})", ps(effective)), str::to_string);
        let checked = ext.get("has_been_checked").is_some_and(truthy);
        let plugin_output = ext
            .get("plugin_output")
            .cloned()
            .unwrap_or_else(|| json!("No output available"));
        let (last_check, last_change) = (field(ext, "last_check"), field(ext, "last_state_change"));
        // 0 is CheckMK's "not yet", not a moment in 1970.
        let moment = |v: &Value| {
            if v.is_number() && truthy(v) {
                v.clone()
            } else {
                Value::Null
            }
        };
        Ok(self.base.structured_response(
            &format!(
                "✅ **Host Status: {host}**\n\n**Status:** {}\n**State Code:** {} ({info})\n**Has Been Checked:** {}\n**Last Check:** {}\n**Last State Change:** {}\n\n**Plugin Output:** {}\n\n✅ **Live monitoring data from CheckMK REST API**",
                status_display(&status),
                ps(effective),
                if checked { "Yes" } else { "No" },
                time_ago(last_check).unwrap_or_else(|| "Never".into()),
                time_ago(last_change).unwrap_or_else(|| "Never".into()),
                ps(&plugin_output)
            ),
            json!({"host_name": host, "state": canonical_state(effective), "state_code": effective, "is_hard_state": is_hard, "has_been_checked": checked, "plugin_output": plugin_output, "last_check": moment(last_check), "last_state_change": moment(last_change)}),
        ))
    }

    async fn host_config(&self, host: &str) -> Result<Value, Fail> {
        Ok(self
            .client()
            .get(
                &format!("objects/host_config/{}", path_segment(host)?),
                None,
                &[],
            )
            .await?
            .data)
    }

    async fn get_host_details(&self, host: &str) -> Out {
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let data = self.host_config(host).await?;
        let (ext, a) = (
            field(&data, "extensions"),
            field(field(&data, "extensions"), "attributes"),
        );
        Ok(text(format!(
            "🔍 **Host Details: {host}**\n\nFolder: {}\nIP Address: {}\nAlias: {}\nAgent Type: {}\nSite: {}",
            or(ext, "folder", "/"),
            or(a, "ipaddress", "Not set"),
            or(a, "alias", "Not set"),
            or(a, "tag_agent", "Unknown"),
            or(a, "site", "Not set")
        )))
    }

    async fn get_host_config(&self, host: &str) -> Out {
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let data = self.host_config(host).await?;
        Ok(self
            .base
            .info_response(&format!("Host Configuration: {host}"), Some(&data)))
    }

    /// Single host or `hosts` array: pick the API, then say which one was picked.
    async fn create_host_smart(&self, args: &Value) -> Out {
        let (mut result, route) = if args.get("hosts").is_some_and(truthy) {
            let bulk = json!({"entries": args["hosts"], "bake_agent": args.get("bake_agent").unwrap_or(&json!(false))});
            (self.bulk_create_hosts(&bulk).await?, "\n\n🤖 **Smart Routing Applied:** Multiple hosts detected → Bulk Creation API\n📡 **API Endpoint:** POST domain-types/host_config/actions/bulk-create/invoke")
        } else if args.get("host_name").is_some() {
            (self.create_host(args).await?, "\n\n🤖 **Smart Routing Applied:** Single host detected → Individual Creation API\n📡 **API Endpoint:** POST domain-types/host_config/collections/all")
        } else {
            return Ok(self.base.error_response("Invalid parameters", Some("Must provide either 'host_name' (single mode) or 'hosts' array (multiple mode)")));
        };
        if let Some(Value::String(t)) = result.first_mut().and_then(|b| b.get_mut("text")) {
            t.push_str(route);
        }
        Ok(result)
    }

    async fn create_host(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        let folder = args.get("folder").cloned().unwrap_or_else(|| json!("/"));
        let attributes = args.get("attributes").cloned().unwrap_or_else(|| json!({}));
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        if !valid_host_name(host) {
            return Ok(self.base.error_response(
                "Invalid host name",
                Some("Host name must contain only letters, numbers, hyphens, and underscores"),
            ));
        }
        if attributes.get("ipaddress").is_some_and(|ip| !valid_ip(ip)) {
            return Ok(self
                .base
                .error_response("Invalid IP address", Some("IP address format is invalid")));
        }
        match self.host_config(host).await {
            Ok(_) => {
                return Ok(self.base.error_response(
                    "Host already exists",
                    Some(&format!(
                        "Host '{host}' already exists. Use update_host to modify it."
                    )),
                ))
            }
            Err(Fail::Api(ApiError {
                kind: ApiErrorKind::NotFound,
                ..
            })) => {}
            Err(e) => return Err(e),
        }
        let f = validate_folder_path(&folder, "create");
        if !f.valid {
            return Ok(self
                .base
                .error_response("Invalid folder path", Some(&f.error_message)));
        }
        let body = json!({"folder": f.checkmk_path, "host_name": host, "attributes": attributes});
        self.client()
            .post("domain-types/host_config/collections/all", Some(&body), &[])
            .await?;
        let line = |label: &str, key: &str| {
            attributes
                .get(key)
                .filter(|v| truthy(v))
                .map_or_else(String::new, |v| format!("• {label}: {}\n", ps(v)))
        };
        Ok(text(format!(
            "✅ **Host Created Successfully**\n\n**Host:** {host}\n**Folder:** {} ({})\n**Attributes Set:** {}\n\n📋 **Host Details:**\n{}{}{}\n⚠️ **Remember to activate changes!**\n\n💡 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review\n2️⃣ Use 'activate_changes' to apply configuration",
            f.display_path,
            f.checkmk_path,
            attributes.as_object().map_or(0, Map::len),
            line("IP Address", "ipaddress"),
            line("Alias", "alias"),
            line("Site", "site"),
        )))
    }

    async fn bulk_create_hosts(&self, args: &Value) -> Out {
        let entries = list(args, "entries");
        let bake = args.get("bake_agent").is_some_and(truthy);
        if entries.is_empty() {
            return Ok(self.missing("entries list"));
        }
        let mut errors = vec![];
        for (i, e) in entries.iter().enumerate() {
            let name = s(e, "host_name");
            if name.is_empty() {
                errors.push(format!("Entry {}: host_name is required", i + 1));
            } else if !valid_host_name(name) {
                errors.push(format!("Entry {}: Invalid host name '{name}'", i + 1));
            }
            if let Some(ip) = e
                .pointer("/attributes/ipaddress")
                .filter(|ip| !valid_ip(ip))
            {
                errors.push(format!("Entry {}: Invalid IP address '{}'", i + 1, ps(ip)));
            }
        }
        if !errors.is_empty() {
            return Ok(self.base.error_response(
                "Validation failed",
                Some(&format!(
                    "Bulk host creation validation errors:\n• {}",
                    errors.join("\n• ")
                )),
            ));
        }
        // CheckMK's root folder is "~".
        let processed: Vec<Value> = entries
            .iter()
            .map(|e| {
                let mut e = e.clone();
                if e.get("folder").is_none_or(|f| f == "/") {
                    e["folder"] = json!("~");
                }
                e
            })
            .collect();
        let mut body = json!({"entries": processed});
        if bake {
            body["bake_agent"] = json!(true);
        }
        let r = match self
            .client()
            .post(
                "domain-types/host_config/actions/bulk-create/invoke",
                Some(&body),
                &[],
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(self.base.error_response(
                    "Operation failed",
                    Some(&format!("Unexpected error during bulk host creation: {e}")),
                ))
            }
        };
        let (mut created, mut count) = (vec![], entries.len());
        if let Some(value) = r.data.get("value") {
            if let Some(items) = value.as_array() {
                count = items.len();
                for h in items.iter().take(10) {
                    created.push(format!(
                        "• {} (Folder: {})",
                        or(h, "id", "Unknown"),
                        h.pointer("/extensions/folder")
                            .map_or_else(|| "/".into(), ps)
                    ));
                }
            }
        }
        let mut t = format!(
            "✅ **Bulk Host Creation Successful**\n\n**Hosts Created:** {count}/{}\n",
            entries.len()
        );
        if bake {
            t.push_str("**Agent Baking:** Enabled (process started in background)\n");
        }
        t.push_str("\n📋 **Created Hosts:**\n");
        let more = entries.len().saturating_sub(10);
        if !created.is_empty() {
            t.push_str(&created.join("\n"));
            if more > 0 {
                t.push_str(&format!("\n... and {more} more hosts"));
            }
        } else {
            for (i, e) in entries.iter().take(10).enumerate() {
                t.push_str(&format!(
                    "• {} (Folder: {})\n",
                    e.get("host_name")
                        .map_or_else(|| format!("Host-{}", i + 1), ps),
                    or(e, "folder", "/")
                ));
            }
            if more > 0 {
                t.push_str(&format!("... and {more} more hosts\n"));
            }
        }
        t.push_str("\n⚠️ **Remember to activate changes!**\n\n💡 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review all changes\n2️⃣ Use 'activate_changes' to apply configuration\n");
        if bake {
            t.push_str("3️⃣ Monitor agent baking progress in CheckMK GUI");
        }
        Ok(text(t))
    }

    async fn update_host(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        let attributes = args.get("attributes").map(map_of).unwrap_or_default();
        let mode = args
            .get("update_mode")
            .map_or_else(|| "update".to_string(), ps);
        let remove: Vec<String> = list(args, "remove_attributes").iter().map(ps).collect();
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let errors = validate_update_attributes(&attributes);
        if !errors.is_empty() {
            return Ok(self
                .base
                .error_response("Validation failed", Some(&errors.join("\n"))));
        }
        let endpoint = format!("objects/host_config/{}", path_segment(host)?);
        // ETag from the object just read: the write is refused if someone else changed it since.
        let current = self.client().get(&endpoint, None, &[]).await?;
        let etag = Base::extract_etag(&current);
        let current_attrs = attrs_of(&current.data);
        let invalid = |m: &str| Ok(self.base.error_response("Invalid combination", Some(m)));
        // CheckMK 2.2.0p7+ refuses attributes, update_attributes and remove_attributes together.
        let (body, operation) = match mode.as_str() {
            "overwrite" if !remove.is_empty() => return invalid(
                "Cannot use 'remove_attributes' with 'overwrite' mode. Use 'remove' mode instead.",
            ),
            "overwrite" => (
                json!({"attributes": attributes}),
                "Complete replacement of host attributes".to_string(),
            ),
            "remove" if !attributes.is_empty() => {
                return invalid(
                    "Cannot specify 'attributes' with 'remove' mode. Use 'update' mode instead.",
                )
            }
            "remove" if remove.is_empty() => {
                return Ok(self.base.error_response(
                    "Missing parameter",
                    Some("remove_attributes is required for 'remove' mode"),
                ))
            }
            "remove" => (
                json!({"remove_attributes": remove}),
                format!("Removing attributes: {}", remove.join(", ")),
            ),
            _ if !remove.is_empty() => {
                return invalid(
                    "Cannot use 'remove_attributes' with 'update' mode. Use 'remove' mode instead.",
                )
            }
            _ => (
                json!({"update_attributes": attributes}),
                "Merging with existing host attributes".to_string(),
            ),
        };
        let headers = vec![("If-Match".to_string(), etag)];
        match self.client().put(&endpoint, Some(&body), &headers).await {
            Ok(_) => {}
            Err(e) if e.kind == ApiErrorKind::Api && matches!(e.status_code, Some(412 | 428)) => {
                return Ok(self.base.error_response("Concurrent modification detected", Some(&format!("Host '{host}' was modified by another process. Please re-read it and retry the operation."))))
            }
            Err(e) if e.kind == ApiErrorKind::Api => return Err(e.into()),
            Err(e) => return Ok(self.base.error_response("Update operation failed", Some(&format!("Unexpected error updating host '{host}': {e}")))),
        }
        let changes = match mode.as_str() {
            "update" => {
                let mut merged = current_attrs.clone();
                merged.extend(attributes);
                compare_attributes(&current_attrs, &merged)
            }
            "overwrite" => compare_attributes(&current_attrs, &attributes),
            _ => Changes {
                removed: remove
                    .iter()
                    .filter_map(|a| current_attrs.get(a).map(|v| (a.clone(), v.clone())))
                    .collect(),
                ..Changes::default()
            },
        };
        Ok(text(format!(
            "✅ **Host Updated Successfully**\n\n**Host:** {host}\n**Update Mode:** {mode}\n**Operation:** {operation}\n\n📋 **Changes Applied:**\n{}\n\n⚠️ **Remember to activate changes!**\n💡 Use 'vibemk_activate_changes' to apply the configuration",
            if changes.has_changes() { changes.format() } else { "No changes detected".into() }
        )))
    }

    async fn delete_host(&self, host: &str) -> Out {
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        self.client()
            .delete(
                &format!("objects/host_config/{}", path_segment(host)?),
                None,
                &[],
            )
            .await?;
        Ok(text(format!("✅ **Host Deleted Successfully**\n\nHost: {host}\n\n📝 **Next Steps:**\n1️⃣ Use 'get_pending_changes' to review the deletion\n2️⃣ Use 'activate_changes' to apply the configuration\n\n💡 **Important:** The host is only marked for deletion until you activate changes!")))
    }

    async fn move_host(&self, args: &Value) -> Out {
        let (host, target) = (s(args, "host_name"), s(args, "target_folder"));
        if host.is_empty() || target.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("host_name and target_folder are required"),
            ));
        }
        let endpoint = format!(
            "objects/host_config/{}/actions/move/invoke",
            path_segment(host)?
        );
        self.client()
            .post(&endpoint, Some(&json!({"target_folder": target})), &[])
            .await?;
        Ok(self.base.success_response("Host Moved Successfully", Some(&json!({"host": host, "folder": target, "message": "Remember to activate changes!"}))))
    }

    async fn rename_host(&self, args: &Value) -> Out {
        let (host, new_name) = (s(args, "host_name"), s(args, "new_name"));
        if host.is_empty() || new_name.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("host_name and new_name are required"),
            ));
        }
        let endpoint = format!(
            "objects/host_config/{}/actions/rename/invoke",
            path_segment(host)?
        );
        let r = self
            .client()
            .put(&endpoint, Some(&json!({"new_name": new_name})), &[])
            .await?;
        let job = or(&r.data, "id", "");
        Ok(text(format!(
            "✅ **Host Rename Started**\n\nOld name: {host}\nNew name: {new_name}\n{}\n⚠️ Remember to activate changes after the job completes.",
            if job.is_empty() || job == "None" { String::new() } else { format!("Job ID: {job}\n") }
        )))
    }

    async fn bulk_update_hosts(&self, args: &Value) -> Out {
        let entries = list(args, "entries");
        if entries.is_empty() {
            return Ok(self.missing("entries list"));
        }
        self.client()
            .put(
                "domain-types/host_config/actions/bulk-update/invoke",
                Some(&json!({"entries": entries})),
                &[],
            )
            .await?;
        Ok(self.base.success_response(
            "Bulk Update Successful",
            Some(&json!({"updated": entries.len(), "message": "Remember to activate changes!"})),
        ))
    }

    async fn create_cluster_host(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        let nodes = list(args, "nodes");
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        if nodes.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("nodes list is required for cluster hosts"),
            ));
        }
        let folder = args.get("folder").map_or_else(|| "/".to_string(), ps);
        let folder = if folder == "/" {
            "~".to_string()
        } else {
            folder
        };
        let mut attributes = args.get("attributes").map(map_of).unwrap_or_default();
        attributes.insert("tag_agent".into(), json!("no-agent")); // a cluster has no agent of its own
        attributes.insert("nodes".into(), json!(nodes));
        let body = json!({"folder": folder, "host_name": host, "attributes": attributes});
        self.client()
            .post("domain-types/host_config/collections/all", Some(&body), &[])
            .await?;
        Ok(text(format!(
            "✅ **Cluster Host Created Successfully**\n\n**Cluster Host:** {host}\n**Folder:** {folder}\n**Nodes:** {}\n\n📋 **Cluster Configuration:**\n• Node Count: {}\n• Agent Type: No Agent (Cluster)\n\n⚠️ **Remember to activate changes!**",
            nodes.iter().map(ps).collect::<Vec<_>>().join(", "),
            nodes.len()
        )))
    }

    async fn validate_host_config(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        let attributes = field(args, "attributes");
        let operation = args
            .get("operation")
            .map_or_else(|| "create".to_string(), ps);
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let (mut errors, mut warnings) = (vec![], vec![]);
        if !valid_host_name(host) {
            errors.push("Invalid host name format");
        }
        if attributes.get("ipaddress").is_some_and(|ip| !valid_ip(ip)) {
            errors.push("Invalid IP address format");
        }
        let folder = args.get("folder").map_or_else(|| "/".to_string(), ps);
        if self.folder_config(&folder).await.is_err() {
            warnings.push(format!("Folder '{folder}' may not exist"));
        }
        if operation == "create" {
            self.host_config(host).await?;
            errors.push("Host already exists");
        }
        let mut t = format!("🔍 **Host Configuration Validation**\n\n**Host:** {host}\n**Operation:** {operation}\n**Status:** {}\n\n", if errors.is_empty() { "✅ Valid" } else { "❌ Invalid" });
        if !errors.is_empty() {
            t.push_str("🚨 **Errors:**\n");
            errors.iter().for_each(|e| t.push_str(&format!("• {e}\n")));
            t.push('\n');
        }
        if !warnings.is_empty() {
            t.push_str("⚠️ **Warnings:**\n");
            warnings
                .iter()
                .for_each(|w| t.push_str(&format!("• {w}\n")));
            t.push('\n');
        }
        if errors.is_empty() {
            t.push_str("✅ Configuration is valid and ready for deployment");
        }
        Ok(text(t))
    }

    async fn folder_config(&self, folder: &str) -> Result<Value, Fail> {
        let ident = format!(
            "~{}",
            folder
                .trim_matches(|c| c == '/' || c == '~')
                .replace('/', "~")
        );
        Ok(self
            .client()
            .get(
                &format!("objects/folder_config/{}", path_segment(&ident)?),
                None,
                &[],
            )
            .await?
            .data)
    }

    async fn compare_host_states(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let current = attrs_of(&self.host_config(host).await?);
        let changes = compare_attributes(
            &current,
            &args
                .get("desired_attributes")
                .map(map_of)
                .unwrap_or_default(),
        );
        let mut t = format!(
            "🔄 **Host State Comparison**\n\n**Host:** {host}\n**Changes Required:** {}\n\n",
            if changes.has_changes() { "Yes" } else { "No" }
        );
        if changes.has_changes() {
            t.push_str("📋 **Detected Changes:**\n");
            t.push_str(&changes.format());
        } else {
            t.push_str("✅ Host is already in the desired state");
        }
        Ok(text(t))
    }

    async fn get_host_effective_attributes(&self, args: &Value) -> Out {
        let host = s(args, "host_name");
        if host.is_empty() {
            return Ok(self.missing("host_name"));
        }
        let data = self.host_config(host).await?;
        let attributes = attrs_of(&data);
        let folder = data
            .pointer("/extensions/folder")
            .map_or_else(|| "/".to_string(), ps);
        let mut effective = Map::new();
        if folder != "/" {
            // The host reports its folder as "/linux/web"; the API addresses it as "~linux~web".
            effective.extend(attrs_of(&self.folder_config(&folder).await?));
        }
        effective.extend(attributes.clone());
        let mut t = format!(
            "📋 **Effective Host Attributes**\n\n**Host:** {host}\n**Folder:** {folder}\n\n"
        );
        if effective.is_empty() {
            t.push_str("ℹ️ No attributes configured");
        } else {
            t.push_str("🎯 **Effective Attributes:**\n");
            for (k, v) in &effective {
                t.push_str(&format!(
                    "• **{k}:** {} _{}_\n",
                    ps(v),
                    if attributes.contains_key(k) {
                        "Host"
                    } else {
                        "Inherited"
                    }
                ));
            }
        }
        Ok(text(t))
    }

    async fn clone_host(&self, args: &Value) -> Out {
        let (source, target) = (s(args, "source_hostname"), s(args, "target_hostname"));
        if source.is_empty() || target.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("source_hostname and target_hostname are required"),
            ));
        }
        let data = self.host_config(source).await?;
        let folder = data
            .pointer("/extensions/folder")
            .map_or_else(|| "/".to_string(), ps);
        let mut attributes = attrs_of(&data);
        attributes.shift_remove("meta_data");
        if let Some(ip) = args.get("ip_address").filter(|v| truthy(v)) {
            attributes.insert("ipaddress".into(), ip.clone());
        }
        let body = json!({"host_name": target, "folder": format!("~{}", folder.trim_start_matches('/').replace('/', "~")), "attributes": attributes});
        self.client()
            .post("domain-types/host_config/collections/all", Some(&body), &[])
            .await?;
        Ok(text(format!("✅ Host '{target}' created as a clone of '{source}'.\nFolder: {folder}\nAttributes: {}", json_dumps_indent2(&Value::Object(attributes)))))
    }
}

#[async_trait]
impl Handler for HostHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let host = s(args, "host_name");
        let result = match tool {
            "vibemk_get_checkmk_hosts" => self.get_hosts().await,
            "vibemk_get_host_status" => self.get_host_status(host).await,
            "vibemk_get_host_details" => self.get_host_details(host).await,
            "vibemk_get_host_config" => self.get_host_config(host).await,
            "vibemk_create_host" => self.create_host_smart(args).await,
            "vibemk_bulk_create_hosts" => self.bulk_create_hosts(args).await,
            "vibemk_update_host" => self.update_host(args).await,
            "vibemk_delete_host" => self.delete_host(host).await,
            "vibemk_move_host" => self.move_host(args).await,
            "vibemk_bulk_update_hosts" => self.bulk_update_hosts(args).await,
            "vibemk_create_cluster_host" => self.create_cluster_host(args).await,
            "vibemk_validate_host_config" => self.validate_host_config(args).await,
            "vibemk_compare_host_states" => self.compare_host_states(args).await,
            "vibemk_get_host_effective_attributes" => {
                self.get_host_effective_attributes(args).await
            }
            "vibemk_clone_host" => self.clone_host(args).await,
            "vibemk_rename_host" => self.rename_host(args).await,
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
