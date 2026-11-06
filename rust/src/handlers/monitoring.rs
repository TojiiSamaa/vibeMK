//! Port of src/vibemk/handlers/monitoring.py (MonitoringHandler).
//!
//! `Err` from the client is the Python exception (the Python client raises on non-2xx), so the
//! "success is false" branches cannot happen and are not ported.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::handlers::downtimes::unix_time;

/// Failure of one tool call: a CheckMK error or anything else ("Unexpected Error").
enum Fail {
    Api(ApiError),
    Other(String),
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Fail::Api(e)
    }
}

type R<T> = Result<T, Fail>;

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

/// `str(d.get(key, default))`.
fn get_str(v: &Value, key: &str, default: &str) -> String {
    v.get(key).map_or_else(|| default.to_string(), py_str)
}

/// `args.get(key)` that is truthy, else None.
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| truthy(v))
}

/// `args.get(key, default)`, kept as sent.
fn flag(args: &Value, key: &str, default: bool) -> Value {
    args.get(key).cloned().unwrap_or(Value::Bool(default))
}

/// `item.get("extensions", {})`, tolerant of null or non-object (AUDIT E2).
fn ext_of(item: &Value) -> &Value {
    static EMPTY: Value = Value::Null;
    item.get("extensions")
        .filter(|e| e.is_object())
        .unwrap_or(&EMPTY)
}

/// `data.get("value", [])`, tolerant of a missing or non-list value.
fn values(data: &Value) -> Vec<Value> {
    data.get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Whether someone already deals with a problem: acknowledged, or inside a downtime.
fn handling(ext: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "acknowledged".into(),
        json!(ext.get("acknowledged").is_some_and(truthy)),
    );
    m.insert(
        "in_downtime".into(),
        json!(ext.get("scheduled_downtime_depth").is_some_and(truthy)),
    );
    m
}

/// The same in prose.
fn handling_note(ext: &Value) -> String {
    let mut notes = Vec::new();
    if ext.get("acknowledged").is_some_and(truthy) {
        notes.push("acknowledged");
    }
    if ext.get("scheduled_downtime_depth").is_some_and(truthy) {
        notes.push("in downtime");
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join(", "))
    }
}

/// One problem line, carrying why it is failing and since when.
fn format_service_problem(
    host_name: &str,
    description: &str,
    state_name: &str,
    plugin_output: &str,
    last_state_change: &Value,
) -> String {
    let mut line = format!("🔧 SERVICE: {host_name}/{description} - {state_name}");
    if !plugin_output.is_empty() {
        line.push_str(&format!("\n    {}", plugin_output.trim()));
    }
    if truthy(last_state_change) {
        let secs = last_state_change.as_f64().unwrap_or(0.0) as i64;
        if let Some(since) = DateTime::<Utc>::from_timestamp(secs, 0) {
            line.push_str(&format!(
                "\n    since {} UTC",
                since.format("%Y-%m-%d %H:%M")
            ));
        }
    }
    line
}

fn state_name(state: &Value, names: &[(i64, &str)]) -> String {
    state
        .as_i64()
        .and_then(|s| names.iter().find(|(k, _)| *k == s))
        .map_or_else(
            || format!("STATE({})", py_str(state)),
            |(_, n)| (*n).to_string(),
        )
}

/// One entry of the DOWNTIME_LIST output schema (downtimes.py `downtime_record`).
fn downtime_record(downtime: &Value, now: f64) -> Value {
    let ext = ext_of(downtime);
    let is_service = match ext.get("is_service") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(s)) => s == "1" || s == "yes",
        _ => false,
    };
    let at = |k: &str| unix_time(ext.get(k).unwrap_or(&Value::Null));
    json!({
        "downtime_id": get_str(downtime, "id", ""),
        "host_name": get_str(ext, "host_name", ""),
        "service_description": if is_service { ext.get("service_description").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "is_service": is_service,
        "start_time": get_str(ext, "start_time", ""),
        "end_time": get_str(ext, "end_time", ""),
        "active": at("start_time") <= now && now <= at("end_time"),
        "comment": get_str(ext, "comment", ""),
        "author": get_str(ext, "author", ""),
        "recurring": ext.get("recurring").is_some_and(truthy),
    })
}

pub struct MonitoringHandler {
    base: Base,
}

impl MonitoringHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    fn text(t: String) -> Vec<Value> {
        vec![json!({"type": "text", "text": t})]
    }

    async fn get_current_problems(&self, args: &Value) -> R<Vec<Value>> {
        let target = arg(args, "host_name").map(py_str);
        let mut problems: Vec<String> = Vec::new();
        let (mut host_problems, mut service_problems) = (Vec::new(), Vec::new());
        let client = &self.base.client;

        // One collection call per kind, filtered in memory (not one call per host/service).
        let host_cols =
            json!({"columns": ["name", "state", "acknowledged", "scheduled_downtime_depth"]});
        let hosts = client
            .get("domain-types/host/collections/all", Some(&host_cols), &[])
            .await?;
        for host in values(&hosts.data) {
            let ext = ext_of(&host);
            let host_name = ext
                .get("name")
                .filter(|n| truthy(n))
                .or_else(|| host.get("id"))
                .map_or_else(|| "Unknown".to_string(), py_str);
            if target.as_ref().is_some_and(|t| *t != host_name) {
                continue;
            }
            let state = ext.get("state").cloned().unwrap_or(json!(0));
            if state != json!(0) {
                let name = state_name(&state, &[(1, "DOWN"), (2, "UNREACHABLE")]);
                problems.push(format!(
                    "🖥️ HOST: {host_name} - {name}{}",
                    handling_note(ext)
                ));
                let mut rec = Map::new();
                rec.insert("host_name".into(), json!(host_name));
                rec.insert("state".into(), json!(name));
                rec.insert("state_code".into(), state);
                rec.extend(handling(ext));
                host_problems.push(Value::Object(rec));
            }
        }

        let columns = [
            "host_name",
            "description",
            "state",
            "plugin_output",
            "last_state_change",
            "acknowledged",
            "scheduled_downtime_depth",
        ];
        let services = client
            .get(
                "domain-types/service/collections/all",
                Some(&json!({"columns": columns})),
                &[],
            )
            .await?;
        for service in values(&services.data) {
            let ext = ext_of(&service);
            let host_name = get_str(ext, "host_name", "Unknown");
            if target.as_ref().is_some_and(|t| *t != host_name) {
                continue;
            }
            let state = ext.get("state").cloned().unwrap_or(json!(0));
            if state != json!(0) {
                let description = get_str(ext, "description", "Unknown");
                let name = state_name(&state, &[(1, "WARNING"), (2, "CRITICAL"), (3, "UNKNOWN")]);
                let output = ext
                    .get("plugin_output")
                    .filter(|o| truthy(o))
                    .map(py_str)
                    .unwrap_or_default();
                let changed = ext.get("last_state_change").cloned().unwrap_or(json!(0));
                problems.push(format_service_problem(
                    &host_name,
                    &description,
                    &format!("{name}{}", handling_note(ext)),
                    &output,
                    &changed,
                ));
                let mut rec = Map::new();
                rec.insert("host_name".into(), json!(host_name));
                rec.insert("service_description".into(), json!(description));
                rec.insert("state".into(), json!(name));
                rec.insert("state_code".into(), state);
                rec.insert("plugin_output".into(), json!(output.trim()));
                rec.insert(
                    "last_state_change".into(),
                    changed
                        .as_i64()
                        .filter(|c| *c > 0)
                        .map_or(Value::Null, |c| json!(c)),
                );
                rec.extend(handling(ext));
                service_problems.push(Value::Object(rec));
            }
        }

        let data = json!({"total": problems.len(), "host_problems": host_problems, "service_problems": service_problems});
        if problems.is_empty() {
            return Ok(self
                .base
                .structured_response("✅ No current problems found", data));
        }
        Ok(self.base.structured_response(
            &format!(
                "🚨 **Current Problems** ({} total):\n\n{}",
                problems.len(),
                problems.join("\n")
            ),
            data,
        ))
    }

    async fn acknowledge_problem(&self, args: &Value) -> R<Vec<Value>> {
        let (Some(ack_type), Some(host), Some(comment)) = (
            arg(args, "acknowledge_type"),
            arg(args, "host_name"),
            arg(args, "comment"),
        ) else {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("acknowledge_type, host_name, and comment are required"),
            ));
        };
        let mut data = Map::new();
        data.insert("acknowledge_type".into(), ack_type.clone());
        data.insert("host_name".into(), host.clone());
        let (kind, target) = match ack_type.as_str() {
            Some("host") => ("host", format!("host '{}'", py_str(host))),
            Some("service") => {
                let Some(service) = arg(args, "service_description") else {
                    return Ok(self.base.error_response(
                        "Missing parameter",
                        Some("service_description is required for service acknowledgment"),
                    ));
                };
                data.insert("service_description".into(), service.clone());
                (
                    "service",
                    format!("service '{}/{}'", py_str(host), py_str(service)),
                )
            }
            _ => {
                return Ok(self.base.error_response(
                    "Invalid acknowledge_type",
                    Some("acknowledge_type must be 'host' or 'service'"),
                ))
            }
        };
        data.insert("comment".into(), comment.clone());
        data.insert("sticky".into(), flag(args, "sticky", true));
        data.insert("notify".into(), flag(args, "notify", true));
        data.insert("persistent".into(), flag(args, "persistent", false));
        self.base
            .client
            .post(
                &format!("domain-types/acknowledge/collections/{kind}"),
                Some(&Value::Object(data)),
                &[],
            )
            .await?;
        Ok(Self::text(format!(
            "✅ **Problem Acknowledged**\n\nTarget: {target}\nComment: {}",
            py_str(comment)
        )))
    }

    async fn get_downtimes(&self, args: &Value) -> R<Vec<Value>> {
        let params = arg(args, "host_name").map(|h| json!({"host_name": h}));
        let result = self
            .base
            .client
            .get(
                "domain-types/downtime/collections/all",
                params.as_ref(),
                &[],
            )
            .await?;
        let downtimes = values(&result.data);
        let now = Utc::now().timestamp_millis() as f64 / 1000.0;
        let data = json!({"total": downtimes.len(), "downtimes": downtimes.iter().map(|d| downtime_record(d, now)).collect::<Vec<_>>()});
        if downtimes.is_empty() {
            return Ok(self
                .base
                .structured_response("ℹ️ No scheduled downtimes", data));
        }
        let list: Vec<String> = downtimes
            .iter()
            .take(20)
            .map(|d| {
                let ext = ext_of(d);
                let host = get_str(ext, "host_name", "Unknown");
                let service = get_str(ext, "service_description", "");
                let target = if service.is_empty() {
                    host
                } else {
                    format!("{host}/{service}")
                };
                format!(
                    "⏰ ID:{} - {target} ({} - {}) - {}",
                    get_str(d, "id", "Unknown"),
                    get_str(ext, "start_time", "Unknown"),
                    get_str(ext, "end_time", "Unknown"),
                    get_str(ext, "comment", "No comment")
                )
            })
            .collect();
        Ok(self.base.structured_response(
            &format!(
                "⏰ **Scheduled Downtimes** ({} total):\n\n{}",
                downtimes.len(),
                list.join("\n")
            ),
            data,
        ))
    }

    async fn get_comments(&self, args: &Value) -> R<Vec<Value>> {
        let mut params = Map::new();
        for k in ["host_name", "service_description"] {
            if let Some(v) = arg(args, k) {
                params.insert(k.into(), v.clone());
            }
        }
        let params = (!params.is_empty()).then_some(Value::Object(params));
        let client = &self.base.client;
        let host_result = client
            .get(
                "domain-types/comment/collections/host",
                params.as_ref(),
                &[],
            )
            .await?;
        let service_result = client
            .get(
                "domain-types/comment/collections/service",
                params.as_ref(),
                &[],
            )
            .await?;

        let mut comments = Vec::new();
        for c in values(&host_result.data) {
            let ext = ext_of(&c);
            comments.push(format!(
                "💬 HOST:{} - {} - {} (by {})",
                get_str(&c, "id", "Unknown"),
                get_str(ext, "host_name", "Unknown"),
                get_str(ext, "comment", "No comment"),
                get_str(ext, "author", "Unknown")
            ));
        }
        for c in values(&service_result.data) {
            let ext = ext_of(&c);
            comments.push(format!(
                "💬 SERVICE:{} - {}/{} - {} (by {})",
                get_str(&c, "id", "Unknown"),
                get_str(ext, "host_name", "Unknown"),
                get_str(ext, "service_description", "Unknown"),
                get_str(ext, "comment", "No comment"),
                get_str(ext, "author", "Unknown")
            ));
        }
        if comments.is_empty() {
            return Ok(Self::text("💬 No comments found".into()));
        }
        let shown = comments
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Self::text(format!(
            "💬 **Comments** ({} total):\n\n{shown}",
            comments.len()
        )))
    }

    async fn add_comment(&self, args: &Value) -> R<Vec<Value>> {
        let (Some(kind), Some(host), Some(comment)) = (
            arg(args, "comment_type"),
            arg(args, "host_name"),
            arg(args, "comment"),
        ) else {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("comment_type, host_name, and comment are required"),
            ));
        };
        let persistent = flag(args, "persistent", true);
        let mut data = Map::new();
        data.insert("comment_type".into(), kind.clone());
        data.insert("host_name".into(), host.clone());
        data.insert("comment".into(), comment.clone());
        data.insert("persistent".into(), persistent.clone());
        let (kind, target) = match kind.as_str() {
            Some("host") => ("host", format!("host '{}'", py_str(host))),
            Some("service") => {
                let Some(service) = arg(args, "service_description") else {
                    return Ok(self.base.error_response(
                        "Missing parameter",
                        Some("service_description is required for service comments"),
                    ));
                };
                data.insert("service_description".into(), service.clone());
                (
                    "service",
                    format!("service '{}/{}'", py_str(host), py_str(service)),
                )
            }
            _ => {
                return Ok(self.base.error_response(
                    "Invalid comment_type",
                    Some("comment_type must be 'host' or 'service'"),
                ))
            }
        };
        self.base
            .client
            .post(
                &format!("domain-types/comment/collections/{kind}"),
                Some(&Value::Object(data)),
                &[],
            )
            .await?;
        Ok(Self::text(format!(
            "💬 **Comment Added**\n\nTarget: {target}\nComment: {}\nPersistent: {}",
            py_str(comment),
            py_str(&persistent)
        )))
    }

    async fn delete_comment(&self, args: &Value) -> R<Vec<Value>> {
        let delete_type = args.get("delete_type").cloned().unwrap_or(json!("by_id"));
        let (comment_id, host, service) = (
            arg(args, "comment_id"),
            arg(args, "host_name"),
            arg(args, "service_description"),
        );
        let data = match delete_type.as_str() {
            Some("by_id") => {
                let Some(id) = comment_id else {
                    return Ok(self.base.error_response(
                        "Missing parameter",
                        Some("comment_id is required for delete_type=by_id"),
                    ));
                };
                let n = match id {
                    Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
                    Value::String(s) => s.trim().parse::<i64>().ok(),
                    Value::Bool(b) => Some(i64::from(*b)),
                    _ => None,
                };
                let Some(n) = n else {
                    return Err(Fail::Other(format!(
                        "invalid literal for int() with base 10: {}",
                        py_repr(id)
                    )));
                };
                let site = args
                    .get("site_id")
                    .cloned()
                    .unwrap_or_else(|| json!(self.base.client.config.site));
                json!({"delete_type": "by_id", "comment_id": n, "site_id": site})
            }
            Some("by_query") => {
                let Some(host) = host else {
                    return Ok(self.base.error_response(
                        "Missing parameter",
                        Some("host_name is required for delete_type=by_query"),
                    ));
                };
                let mut d = json!({"delete_type": "params", "host_name": host});
                if let Some(s) = service {
                    d["service_descriptions"] = json!([s]);
                }
                d
            }
            _ => {
                return Ok(self.base.error_response(
                    "Invalid delete_type",
                    Some("delete_type must be 'by_id' or 'by_query'"),
                ))
            }
        };
        self.base
            .client
            .post(
                "domain-types/comment/actions/delete/invoke",
                Some(&data),
                &[],
            )
            .await?;
        let target = match (delete_type.as_str(), comment_id) {
            (Some("by_id"), Some(id)) => format!("comment #{}", py_str(id)),
            _ => format!(
                "{}{}",
                host.map(py_str).unwrap_or_default(),
                service
                    .map(|s| format!("/{}", py_str(s)))
                    .unwrap_or_default()
            ),
        };
        Ok(Self::text(format!(
            "💬 **Comment Deleted**\n\nTarget: {target}"
        )))
    }
}

#[async_trait]
impl Handler for MonitoringHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_current_problems" => match self.get_current_problems(args).await {
                Err(Fail::Api(e)) => {
                    return self
                        .base
                        .error_response("Error retrieving problems", Some(&e.to_string()))
                }
                Err(Fail::Other(m)) => {
                    return self
                        .base
                        .error_response("Error retrieving problems", Some(&m))
                }
                ok => ok,
            },
            "vibemk_acknowledge_problem" => self.acknowledge_problem(args).await,
            "vibemk_get_downtimes" => self.get_downtimes(args).await,
            "vibemk_delete_comment" => self.delete_comment(args).await,
            "vibemk_get_comments" => self.get_comments(args).await,
            "vibemk_add_comment" => self.add_comment(args).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        match result {
            Ok(blocks) => blocks,
            Err(Fail::Api(e)) => self
                .base
                .error_response("CheckMK API Error", Some(&e.to_string())),
            Err(Fail::Other(m)) => self.base.error_response("Unexpected Error", Some(&m)),
        }
    }
}
