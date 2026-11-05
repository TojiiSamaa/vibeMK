//! Port of src/vibemk/handlers/services.py (ServiceHandler).
//!
//! `Err` from the client is the Python exception (the Python client raises on non-2xx), so every
//! `if not result.get("success")` branch of the Python is the `Err` arm here.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use serde_json::{json, Map, Value};

use crate::client::CheckMkClient;
use crate::handlers::base::{py_repr, time_ago, Base, Handler};
use crate::paths::path_segment;

const MAX_SERVICES_DISPLAYED: usize = 50;
// Column positions in the query API's array-shaped rows.
const COL_STATE: usize = 2;
const COL_PLUGIN_OUTPUT: usize = 3;
const COL_LAST_CHECK: usize = 4;
const COL_LAST_STATE_CHANGE: usize = 5;
const COLLECTION: &str = "domain-types/service/collections/all";

/// Python `str()` of a JSON value: a string stays as is.
pub(crate) fn pystr(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// Python truthiness.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// _STATUS_MAP.get(state, "UNKNOWN(state)").
fn status_text(state: &Value) -> String {
    match state.as_i64() {
        Some(0) => "OK".into(),
        Some(1) => "WARNING".into(),
        Some(2) => "CRITICAL".into(),
        Some(3) => "UNKNOWN".into(),
        _ => format!("UNKNOWN({})", pystr(state)),
    }
}

/// A Unix timestamp, or null for CheckMK's 0 and for placeholders like "Never".
fn timestamp(v: &Value) -> Value {
    v.as_f64()
        .filter(|t| *t > 0.0)
        .map_or(Value::Null, |t| json!(t as i64))
}

/// The SERVICE_STATUS object, whichever of the lookups produced the values.
fn status_data(
    host: &Value,
    service: &str,
    state: &Value,
    plugin_output: &Value,
    last_check: &Value,
    last_state_change: &Value,
    state_type: &Value,
) -> Value {
    let code = state.as_i64().filter(|c| (0..=3).contains(c));
    let mut data = Map::new();
    data.insert("host_name".into(), json!(pystr(host)));
    data.insert("service_description".into(), json!(service));
    data.insert(
        "state".into(),
        json!(code.map_or_else(|| "UNAVAILABLE".to_string(), |_| status_text(state))),
    );
    data.insert("state_code".into(), json!(code));
    data.insert(
        "plugin_output".into(),
        json!(plugin_output.as_str().map_or("", str::trim)),
    );
    data.insert("last_check".into(), timestamp(last_check));
    data.insert("last_state_change".into(), timestamp(last_state_change));
    if let Some(t @ (0 | 1)) = state_type.as_i64() {
        data.insert("is_hard_state".into(), json!(t == 1));
    }
    Value::Object(data)
}

/// dict.get(key, default): the default only when the key is absent (a null stays null).
fn get_or(obj: &Value, key: &str, default: Value) -> Value {
    obj.get(key).cloned().unwrap_or(default)
}

/// `list(d.keys())` as Python prints it.
fn keys_repr(obj: &Value) -> String {
    py_repr(&Value::Array(
        obj.as_object()
            .map(|m| m.keys().map(|k| json!(k)).collect())
            .unwrap_or_default(),
    ))
}

fn query(host: &str, service: &str) -> Value {
    json!({"op": "and", "expr": [{"op": "=", "left": "host_name", "right": host}, {"op": "=", "left": "description", "right": service}]})
}

pub struct ServiceHandler {
    base: Base,
}

impl ServiceHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    /// The collection takes the host filter itself and returns plugin_output.
    async fn get_services(&self, args: &Value) -> Vec<Value> {
        // Request state/plugin_output explicitly: without them every service shows as UNKNOWN.
        let mut params = json!({"columns": ["host_name", "description", "state", "plugin_output"]});
        if truthy(&args["host_name"]) {
            params["host_name"] = args["host_name"].clone();
        }
        let Ok(result) = self.base.client.get(COLLECTION, Some(&params), &[]).await else {
            return self.base.error_response(
                "Service retrieval failed",
                Some("Could not retrieve services using any available method"),
            );
        };
        let services = result.data["value"].as_array().filter(|s| !s.is_empty());
        let Some(services) = services else {
            return vec![json!({"type": "text", "text": "📭 No services found"})];
        };
        let lines: Vec<String> = services
            .iter()
            .take(MAX_SERVICES_DISPLAYED)
            .map(|s| {
                let ext = &s["extensions"];
                let name = |k: &str| ext.get(k).map_or_else(|| "Unknown".to_string(), pystr);
                format!(
                    "🔧 {}/{} (Status: {})",
                    name("host_name"),
                    name("description"),
                    status_text(&ext["state"])
                )
            })
            .collect();
        let text = format!(
            "🔧 **CheckMK Services** ({} total, showing first {}):\n\n{}",
            services.len(),
            lines.len(),
            lines.join("\n")
        );
        vec![json!({"type": "text", "text": text})]
    }

    async fn get_service_status(&self, args: &Value) -> Vec<Value> {
        let (host, service) = (&args["host_name"], &args["service_description"]);
        if !truthy(host) || !truthy(service) {
            return self.base.error_response(
                "Missing parameters",
                Some("host_name and service_description are required"),
            );
        }
        let (host, service) = (pystr(host), pystr(service));
        if let Some(r) = self.via_collection(&host, &service).await {
            return r;
        }
        if let Some(r) = self.via_show_service(&host, &service).await {
            return r;
        }
        if let Some(r) = self.via_query_api(&host, &service).await {
            return r;
        }
        if let Some(r) = self.via_legacy_query(&host, &service).await {
            return r;
        }
        self.base.error_text(&format!(
            "❌ **Service Status Retrieval Failed**\n\nService: {host}/{service}\n\n**Tried Methods:**\n1️⃣ Service collection with check output\n2️⃣ show_service action (state only)\n3️⃣ Query API\n\n**Possible Issues:**\n• Service not found in monitoring system\n• Service description name mismatch\n• CheckMK API version compatibility\n• Monitoring data not yet available\n\n**Recommendation:**\nVerify the service exists in CheckMK GUI and is being monitored."
        ))
    }

    /// The collection carries the check output; show_service does not (verified on 2.4.0p2 CRE).
    async fn via_collection(&self, host: &str, service: &str) -> Option<Vec<Value>> {
        let params = json!({
            "columns": ["host_name", "description", "state", "state_type", "plugin_output", "last_check", "last_state_change"],
            "host_name": host,
        });
        let result = self
            .base
            .client
            .get(COLLECTION, Some(&params), &[])
            .await
            .ok()?;
        for entry in result.data["value"].as_array().into_iter().flatten() {
            // An entry or extensions that is not an object is an exception in Python: fall through.
            let ext = match entry.as_object()?.get("extensions") {
                None => continue,
                Some(e) if e.is_object() => e,
                Some(_) => return None,
            };
            if ext["description"] != service {
                continue;
            }
            let text = format_service_status(ext)?;
            let data = status_data(
                &get_or(ext, "host_name", json!(host)),
                service,
                &ext["state"],
                &ext["plugin_output"],
                &ext["last_check"],
                &ext["last_state_change"],
                &ext["state_type"],
            );
            return Some(self.base.structured_response(&text, data));
        }
        None
    }

    /// The documented show_service action, state only.
    async fn via_show_service(&self, host: &str, service: &str) -> Option<Vec<Value>> {
        let endpoint = format!(
            "objects/host/{}/actions/show_service/invoke",
            path_segment(host).ok()?
        );
        let params = json!({"service_description": service});
        let result = self
            .base
            .client
            .get(&endpoint, Some(&params), &[])
            .await
            .ok()?;
        let data = &result.data;
        if data.get("extensions").is_none() || !data.is_object() {
            let got = match data {
                Value::Object(_) => keys_repr(data),
                Value::Array(_) => "<class 'list'>".into(),
                Value::String(_) => "<class 'str'>".into(),
                Value::Number(n) => if n.is_f64() {
                    "<class 'float'>"
                } else {
                    "<class 'int'>"
                }
                .into(),
                Value::Bool(_) => "<class 'bool'>".into(),
                Value::Null => "<class 'NoneType'>".into(),
            };
            return Some(self.base.error_response(
                "Unexpected response format",
                Some(&format!("Expected extensions in response, got: {got}")),
            ));
        }
        let ext = &data["extensions"];
        if !ext.is_object() {
            return None;
        }
        let state = &ext["state"];
        let description = get_or(ext, "description", json!(service));
        if state.is_null() {
            let text = format!("📊 **Service Found: {host}/{}**\n\n❌ **No state information available**\nAvailable fields: {}", pystr(&description), keys_repr(ext));
            return Some(self.base.structured_response(
                &text,
                status_data(
                    &json!(host),
                    &pystr(&description),
                    &Value::Null,
                    &Value::Null,
                    &Value::Null,
                    &Value::Null,
                    &Value::Null,
                ),
            ));
        }
        let host_from_api = get_or(ext, "host_name", json!(host));
        let icon = match state.as_i64() {
            Some(0) => "✅",
            Some(1) => "⚠️",
            Some(2) => "🔴",
            _ => "❓",
        };
        let text = format!(
            "{icon} **Service Status: {}/{}**\n\n**Status:** {}\n**State Code:** {}\n**Last Check:** {}\n**State Type:** {}\n\n✅ **Live monitoring data from CheckMK REST API**",
            pystr(&host_from_api),
            pystr(&description),
            status_text(state),
            pystr(state),
            time_ago(&ext["last_check"]).unwrap_or_else(|| "Unknown".into()),
            if ext["state_type"].as_i64() == Some(1) { "Hard" } else { "Soft" },
        );
        let data = status_data(
            &host_from_api,
            &pystr(&description),
            state,
            &Value::Null,
            &ext["last_check"],
            &Value::Null,
            &ext["state_type"],
        );
        Some(self.base.structured_response(&text, data))
    }

    /// The query API with explicit columns and a query filter.
    async fn via_query_api(&self, host: &str, service: &str) -> Option<Vec<Value>> {
        let params = json!({
            "columns": ["host_name", "description", "state", "plugin_output", "last_check", "last_state_change"],
            "query": query(host, service),
        });
        let result = self
            .base
            .client
            .get(COLLECTION, Some(&params), &[])
            .await
            .ok()?;
        let first = result.data["value"].as_array()?.first()?;
        match first {
            Value::Array(row) if row.len() > COL_STATE => {
                let cell = |i: usize, d: &str| row.get(i).cloned().unwrap_or_else(|| json!(d));
                let (state, output, last_check, changed) = (
                    cell(COL_STATE, "None"),
                    cell(COL_PLUGIN_OUTPUT, "No output"),
                    cell(COL_LAST_CHECK, "Never"),
                    cell(COL_LAST_STATE_CHANGE, "Unknown"),
                );
                let text = format!(
                    "📊 **Service Status: {host}/{service}** (Correct API Query)\n\nStatus: {}\nOutput: {}\nLast Check: {}\nLast State Change: {}\n\n🔍 **Debug Info:**\nRaw State: {}\nQuery Result: {}\n✅ **Data Source:** Correct CheckMK Query API",
                    status_text(&state),
                    pystr(&output),
                    pystr(&last_check),
                    pystr(&changed),
                    pystr(&state),
                    py_repr(first),
                );
                Some(self.base.structured_response(
                    &text,
                    status_data(
                        &json!(host),
                        service,
                        &state,
                        &output,
                        &last_check,
                        &changed,
                        &Value::Null,
                    ),
                ))
            }
            Value::Object(_) => self.dict_status(host, service, first, "Dict Format"),
            _ => None,
        }
    }

    /// The older query format, as a last resort.
    async fn via_legacy_query(&self, host: &str, service: &str) -> Option<Vec<Value>> {
        let params = json!({"query": query(host, service)});
        let result = self
            .base
            .client
            .get(COLLECTION, Some(&params), &[])
            .await
            .ok()?;
        let first = result.data["value"].as_array()?.first()?;
        self.dict_status(host, service, first, "Fallback Query")
    }

    /// Both dict-shaped fallbacks print the same body; only the label differs.
    fn dict_status(
        &self,
        host: &str,
        service: &str,
        entry: &Value,
        label: &str,
    ) -> Option<Vec<Value>> {
        let ext = match entry.as_object()?.get("extensions") {
            None => &json!({}),
            Some(e) if e.is_object() => e,
            Some(_) => return None,
        };
        let state = &ext["state"];
        if state.is_null() {
            return None;
        }
        let output = get_or(ext, "plugin_output", json!("No output available"));
        let last_check = get_or(ext, "last_check", json!("Never"));
        let text = format!(
            "📊 **Service Status: {host}/{service}** ({label})\n\nStatus: {}\nOutput: {}\nLast Check: {}\n\n🔍 **Debug Info:**\nRaw State: {}\nExtensions: {}",
            status_text(state),
            pystr(&output),
            pystr(&last_check),
            pystr(state),
            keys_repr(ext),
        );
        let data = status_data(
            &json!(host),
            service,
            state,
            &ext["plugin_output"],
            &ext["last_check"],
            &ext["last_state_change"],
            &Value::Null,
        );
        Some(self.base.structured_response(&text, data))
    }
}

/// Render one service's state, its check output and when it last changed.
/// None where the Python raises (a non-text output, a non-numeric timestamp).
fn format_service_status(ext: &Value) -> Option<String> {
    let state = &ext["state"];
    let icon = match state.as_i64() {
        Some(0) => "✅",
        Some(1) => "⚠️",
        Some(2) => "🔴",
        _ => "❓", // not ❌ for CRITICAL: a leading ❌ is the dispatcher's error marker
    };
    let mut lines = vec![
        format!(
            "{icon} **Service Status: {}/{}**",
            pystr(&ext["host_name"]),
            pystr(&ext["description"])
        ),
        String::new(),
        format!("**Status:** {}", status_text(state)),
        format!("**State Code:** {}", pystr(state)),
    ];
    let output = match &ext["plugin_output"] {
        Value::String(s) => s.trim(),
        v if !truthy(v) => "",
        _ => return None,
    };
    if !output.is_empty() {
        lines.push(format!("**Output:** {output}"));
    }
    let changed = &ext["last_state_change"];
    if truthy(changed) {
        let since = DateTime::from_timestamp(changed.as_f64()? as i64, 0)?;
        lines.push(format!("**Since:** {} UTC", since.format("%Y-%m-%d %H:%M")));
    }
    Some(lines.join("\n"))
}

#[async_trait]
impl Handler for ServiceHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        match tool {
            "vibemk_get_checkmk_services" => self.get_services(args).await,
            "vibemk_get_service_status" => self.get_service_status(args).await,
            _ => self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            ),
        }
    }
}
