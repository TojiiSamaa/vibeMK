//! Port of src/vibemk/handlers/discovery.py (DiscoveryHandler).
//!
//! The Python client answers `success: False` only for a non-2xx, which it raises on instead;
//! the Rust client returns `Err` there, so the unreachable "success false" branches are not ported.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::client::{ApiError, ApiErrorKind, CheckMkClient, Method, RequestOptions};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::handlers::services::{pystr, truthy};
use crate::paths::path_segment;

/// How long wait_for_discovery polls by default, and how often (302 = still running).
const WAIT_TIMEOUT_SECONDS: f64 = 120.0;
const WAIT_POLL_SECONDS: u64 = 2;
const REDIRECTS: [u16; 4] = [301, 302, 303, 307];
const BULK_START: &str = "domain-types/discovery_run/actions/bulk-discovery-start/invoke";

/// Every mode the discovery schema offers, sorted by name. Flags:
/// monitor_undecided_services, remove_vanished_services, update_service_labels, update_host_labels, do_full_scan.
const BULK_OPTIONS_BY_MODE: [(&str, [bool; 5]); 7] = [
    ("fix_all", [true, true, true, true, true]),
    ("new", [true, false, false, false, false]),
    ("only_host_labels", [false, false, false, true, false]),
    ("only_service_labels", [false, false, true, false, false]),
    ("refresh", [true, false, true, true, true]),
    ("remove", [false, true, false, false, false]),
    ("tabula_rasa", [true, true, true, true, true]),
];

fn mode_flags(mode: &str) -> Option<[bool; 5]> {
    BULK_OPTIONS_BY_MODE
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, f)| *f)
}

fn sorted_modes() -> Vec<&'static str> {
    BULK_OPTIONS_BY_MODE.iter().map(|(m, _)| *m).collect()
}

fn text_block(text: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": text})]
}

pub struct DiscoveryHandler {
    base: Base,
}

impl DiscoveryHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn start_service_discovery(&self, args: &Value) -> Vec<Value> {
        let host = &args["host_name"];
        if !truthy(host) {
            return self.base.error_text("❌ Error: host_name is required");
        }
        let host_name = pystr(host);
        let default_mode = Value::String("refresh".into());
        let mode_value = args.get("mode").unwrap_or(&default_mode);
        let Some(mode) = mode_value.as_str().filter(|m| mode_flags(m).is_some()) else {
            return self.base.error_text(&format!(
                "❌ Error: mode must be one of {}",
                py_repr(&json!(sorted_modes()))
            ));
        };

        let data = json!({"host_name": host_name, "mode": mode});
        // A started job answers 303 to the wait endpoint: followed, it lands in a 302 loop.
        let opts = RequestOptions {
            follow_redirects: false,
            ..RequestOptions::default()
        };
        match self.base.client.request(Method::Post, "domain-types/service_discovery_run/actions/start/invoke", None, Some(&data), &[], opts).await {
            Ok(r) if REDIRECTS.contains(&r.status) => text_block(format!(
                "✅ **Service Discovery Started**\n\nHost: **{host_name}**\nMode: {mode}\n\n🔄 Discovery is running in the background.\nUse 'wait_for_discovery' or 'get_discovery_status' to check progress."
            )),
            Ok(_) => text_block(format!(
                "✅ **Service Discovery Completed**\n\nHost: **{host_name}**\nMode: {mode}\n\nApplied to the services found by the last scan; run mode 'refresh' first to rescan the host. Use 'activate_changes' to put the result into monitoring."
            )),
            // A second job would only queue behind the first one.
            Err(e) if e.status_code == Some(409) => text_block(format!(
                "ℹ️ **Service Discovery already running**\n\nHost: **{host_name}**\n\nUse 'wait_for_discovery' to wait for it, then 'get_discovery_status' for the result."
            )),
            Err(e) => {
                tracing::warn!("Single host discovery API error for {host_name}: {e}. Falling back to bulk discovery");
                self.fallback_to_bulk_discovery(&host_name, mode).await
            }
        }
    }

    /// Start a bulk discovery; return the job id. 2.5 answers 303 to
    /// objects/background_job/<id> (following it drops auth, 401), so the redirect is
    /// not followed and the id is read from it; 2.3 answers 200 with the id in the body.
    async fn post_bulk_start(&self, data: &Value) -> Result<String, ApiError> {
        let opts = RequestOptions {
            follow_redirects: false,
            ..RequestOptions::default()
        };
        let r = self
            .base
            .client
            .request(Method::Post, BULK_START, None, Some(data), &[], opts)
            .await?;
        let from_location = r
            .header("Location")
            .and_then(|l| l.trim_end_matches('/').rsplit('/').next())
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        Ok(from_location.unwrap_or_else(|| {
            r.data
                .get("id")
                .map_or_else(|| "Unknown".to_string(), pystr)
        }))
    }

    async fn fallback_to_bulk_discovery(&self, host_name: &str, mode: &str) -> Vec<Value> {
        let Some(f) = mode_flags(mode) else {
            return self.base.error_text(&format!(
                "❌ Error: unknown discovery mode '{mode}'. Known modes: {}",
                sorted_modes().join(", ")
            ));
        };
        let bulk_data = json!({
            "hostnames": [host_name],
            "options": {
                "monitor_undecided_services": f[0],
                "remove_vanished_services": f[1],
                "update_service_labels": f[2],
                "update_host_labels": f[3],
            },
            "do_full_scan": f[4],
            "bulk_size": 1,
            "ignore_errors": false,
        });
        match self.post_bulk_start(&bulk_data).await {
            Ok(job_id) => {
                text_block(format!(
                    "✅ **Service Discovery Started** (via bulk discovery)\n\nHost: **{host_name}**\nMode: {mode} (mapped to bulk options)\nJob ID: {job_id}\n\n🔄 Discovery is running in the background.\nUse 'get_bulk_discovery_status' with Job ID {job_id} or 'get_discovery_status' to check progress.\n\n💡 Note: Used bulk discovery as fallback due to API limitations."
                ))
            }
            Err(e) => self
                .base
                .error_text(&format!("❌ Error in fallback discovery method: {e}")),
        }
    }

    async fn start_bulk_discovery(&self, args: &Value) -> Vec<Value> {
        let hostnames = &args["hostnames"];
        let Some(hosts) = hostnames.as_array().filter(|h| !h.is_empty()) else {
            return self.base.error_text("❌ Error: hostnames list is required");
        };
        let or = |k: &str, d: Value| args.get(k).cloned().unwrap_or(d);
        let (do_full_scan, bulk_size, ignore_errors) = (
            or("do_full_scan", json!(true)),
            or("bulk_size", json!(10)),
            or("ignore_errors", json!(true)),
        );

        // Only the additive flag is on by default: "discover services" is not a request to delete any.
        let mut options: Map<String, Value> = [
            ("monitor_undecided_services", true),
            ("remove_vanished_services", false),
            ("update_service_labels", false),
            ("update_host_labels", false),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
        if let Some(extra) = args["options"].as_object() {
            options.extend(extra.clone());
        }
        let shown = |k: &str| pystr(&options[k]);
        let first: Vec<String> = hosts.iter().take(5).map(pystr).collect();
        let (monitor, remove) = (
            shown("monitor_undecided_services"),
            shown("remove_vanished_services"),
        );

        let data = json!({"hostnames": hostnames, "options": options, "do_full_scan": do_full_scan, "bulk_size": bulk_size, "ignore_errors": ignore_errors});
        match self.post_bulk_start(&data).await {
            Ok(job_id) => {
                text_block(format!(
                    "✅ **Bulk Discovery Started**\n\nJob ID: **{job_id}**\nHosts: {} hosts\n  • {}{}\nOptions:\n  • Full scan: {}\n  • Bulk size: {}\n  • Ignore errors: {}\n  • Monitor undecided: {monitor}\n  • Remove vanished: {remove}\n\n🔄 Bulk discovery is running in the background.\nUse 'get_bulk_discovery_status' with Job ID {job_id} to check progress.",
                    hosts.len(),
                    first.join(", "),
                    if hosts.len() > 5 { "..." } else { "" },
                    pystr(&do_full_scan),
                    pystr(&bulk_size),
                    pystr(&ignore_errors),
                ))
            }
            Err(e) => self
                .base
                .error_text(&format!("❌ Error starting bulk discovery: {e}")),
        }
    }

    async fn get_discovery_status(&self, args: &Value) -> Vec<Value> {
        let host = &args["host_name"];
        if !truthy(host) {
            return self.base.error_text("❌ Error: host_name is required");
        }
        let host_name = pystr(host);
        let fail = |e: &dyn std::fmt::Display| {
            self.base
                .error_text(&format!("❌ Error getting discovery status: {e}"))
        };
        let endpoint = match path_segment(&host_name) {
            Ok(s) => format!("objects/service_discovery/{s}"),
            Err(e) => return fail(&e),
        };
        let r = match self.base.client.get(&endpoint, None, &[]).await {
            Ok(r) => r,
            Err(e) => return fail(&e),
        };
        let ext = &r.data["extensions"];
        let empty = Vec::new();
        let table = ext["check_table"].as_array().unwrap_or(&empty);
        let in_state = |s: &str| -> Vec<&Value> {
            table
                .iter()
                .filter(|i| i.is_object() && i["state"] == s)
                .collect()
        };
        let (new, unchanged, vanished) =
            (in_state("new"), in_state("unchanged"), in_state("vanished"));

        let mut out = format!(
            "📊 **Service Discovery Status**\n\nHost: **{host_name}**\n\n📋 **Service Summary:**\n  • New services: {}\n  • Unchanged services: {}\n  • Vanished services: {}\n  • Total services: {}\n\n",
            new.len(),
            unchanged.len(),
            vanished.len(),
            table.len()
        );
        if let Some(labels) = ext["host_labels"].as_object().filter(|l| !l.is_empty()) {
            out.push_str(&format!("🏷️ **Host Labels:** {} labels\n\n", labels.len()));
        }
        for (title, items) in [
            ("🆕 **New Services Found:**\n", &new),
            ("👻 **Vanished Services:**\n", &vanished),
        ] {
            if items.is_empty() {
                continue;
            }
            out.push_str(title);
            for i in items {
                out.push_str(&format!(
                    "  • {}\n",
                    i.get("service_name")
                        .map_or_else(|| "Unknown".to_string(), pystr)
                ));
            }
            out.push('\n');
        }
        text_block(out)
    }

    async fn get_bulk_discovery_status(&self, args: &Value) -> Vec<Value> {
        let job = &args["job_id"];
        if !truthy(job) {
            return self.base.error_text("❌ Error: job_id is required");
        }
        let job_id = pystr(job);
        let fail = |e: &dyn std::fmt::Display| {
            self.base
                .error_text(&format!("❌ Error getting bulk discovery status: {e}"))
        };
        // 2.5 serves bulk-discovery progress as a background job; 2.3 only as
        // objects/discovery_run/{id} (its background_job route does not exist).
        let (job_endpoint, run_endpoint) = match path_segment(&job_id) {
            Ok(s) => (
                format!("objects/background_job/{s}"),
                format!("objects/discovery_run/{s}"),
            ),
            Err(e) => return fail(&e),
        };
        let client = &self.base.client;
        let r = match client.get(&job_endpoint, None, &[]).await {
            // A 404 is ApiErrorKind::NotFound (CheckMKNotFoundError in Python).
            Err(e) if e.kind == ApiErrorKind::NotFound => {
                client.get(&run_endpoint, None, &[]).await
            }
            other => other,
        };
        let r = match r {
            Ok(r) => r,
            Err(e) => return fail(&e),
        };
        let ext = &r.data["extensions"];
        let status = &ext["status"];
        let state_value = [&status["state"], &ext["state"]]
            .into_iter()
            .find(|v| truthy(v));
        let job_state = state_value.map_or_else(|| "unknown".to_string(), pystr);
        let log_info = &status["log_info"];
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .map(|a| a.iter().map(pystr).collect())
                .unwrap_or_default()
        };
        let lines = |k: &str| strings(&log_info[k]);
        let mut progress = lines("JobProgressUpdate");
        if progress.is_empty() {
            // 2.3's discovery_run lists progress under extensions.logs.progress.
            progress = strings(&ext["logs"]["progress"]);
        }
        let progress: Vec<String> = progress
            .into_iter()
            .filter(|l| {
                !l.starts_with("Waiting to acquire lock") && !l.starts_with("Acquired lock")
            })
            .collect();
        let mut progress_text = String::new();
        if !progress.is_empty() {
            let bullets: Vec<String> = progress.iter().map(|l| format!("  • {l}")).collect();
            progress_text = format!("📊 **Progress:**\n{}\n\n", bullets.join("\n"));
        }
        for l in lines("JobException") {
            progress_text.push_str(&format!("⚠️ {l}\n"));
        }
        let emoji = match job_state.as_str() {
            "running" => "🔄",
            "finished" => "✅",
            "stopped" => "⏹️",
            "exception" => "⚠️",
            _ => "❓",
        };
        text_block(format!(
            "{emoji} **Bulk Discovery Job Status**\n\nJob ID: **{job_id}**\nState: {}\n\n{progress_text}💡 Use 'get_discovery_status' on individual hosts for detailed results.",
            job_state.to_uppercase()
        ))
    }

    async fn wait_for_discovery(&self, args: &Value) -> Vec<Value> {
        let host = &args["host_name"];
        if !truthy(host) {
            return self.base.error_text("❌ Error: host_name is required");
        }
        let host_name = pystr(host);
        let fail = |e: &dyn std::fmt::Display| {
            self.base
                .error_text(&format!("❌ Error waiting for discovery completion: {e}"))
        };
        let timeout = match args.get("timeout") {
            None => WAIT_TIMEOUT_SECONDS,
            Some(Value::String(s)) => match s.trim().parse::<f64>() {
                Ok(t) => t,
                Err(_) => {
                    return fail(&format!(
                        "could not convert string to float: {}",
                        py_repr(&args["timeout"])
                    ))
                }
            },
            Some(v) => v.as_f64().unwrap_or(WAIT_TIMEOUT_SECONDS),
        };
        let endpoint = match path_segment(&host_name) {
            Ok(s) => {
                format!("objects/service_discovery_run/{s}/actions/wait-for-completion/invoke")
            }
            Err(e) => return fail(&e),
        };
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.clamp(0.0, 86_400.0));
        let opts = RequestOptions {
            follow_redirects: false,
            ..RequestOptions::default()
        };
        let ask = || {
            self.base
                .client
                .request(Method::Get, &endpoint, None, None, &[], opts)
        };

        let mut result = ask().await;
        while matches!(&result, Ok(r) if REDIRECTS.contains(&r.status)) && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_secs(WAIT_POLL_SECONDS)).await;
            result = ask().await;
        }
        match result {
            Ok(r) if REDIRECTS.contains(&r.status) => text_block(format!(
                "⏳ **Discovery still running**\n\nHost: **{host_name}**\n\nThe job had not finished after {}s. Call 'wait_for_discovery' again, or check 'get_discovery_background_job'.",
                timeout as i64
            )),
            Ok(_) => text_block(format!(
                "✅ **Discovery Completed**\n\nHost: **{host_name}**\n\n🔄 Service discovery has finished.\nUse 'get_discovery_status' to see the results."
            )),
            Err(e) => fail(&e),
        }
    }

    async fn get_discovery_background_job(&self, args: &Value) -> Vec<Value> {
        let host = &args["host_name"];
        if !truthy(host) {
            return self.base.error_text("❌ Error: host_name is required");
        }
        let host_name = pystr(host);
        let fail = |e: &dyn std::fmt::Display| {
            self.base
                .error_text(&format!("❌ Error getting discovery background job: {e}"))
        };
        let endpoint = match path_segment(&host_name) {
            Ok(s) => format!("objects/service_discovery_run/{s}"),
            Err(e) => return fail(&e),
        };
        let r = match self.base.client.get(&endpoint, None, &[]).await {
            Ok(r) => r,
            Err(e) => return fail(&e),
        };
        let ext = &r.data["extensions"];
        let field = |k: &str, d: &str| ext.get(k).map_or_else(|| d.to_string(), pystr);
        let job_state = field("state", "unknown");
        let emoji = match job_state.as_str() {
            "running" => "🔄",
            "finished" => "✅",
            "stopped" => "⏹️",
            "exception" => "❌",
            _ => "❓",
        };
        text_block(format!(
            "{emoji} **Discovery Background Job**\n\nHost: **{host_name}**\nState: {}\nStarted: {}\nFinished: {}\nDuration: {}\n\n💡 Use 'get_discovery_status' to see discovery results.",
            job_state.to_uppercase(),
            field("started", "Unknown"),
            field("finished", "Not finished"),
            field("duration", "Unknown"),
        ))
    }
}

#[async_trait]
impl Handler for DiscoveryHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        match tool {
            "vibemk_start_service_discovery" => self.start_service_discovery(args).await,
            "vibemk_start_bulk_discovery" => self.start_bulk_discovery(args).await,
            "vibemk_get_discovery_status" => self.get_discovery_status(args).await,
            "vibemk_get_bulk_discovery_status" => self.get_bulk_discovery_status(args).await,
            "vibemk_wait_for_discovery" => self.wait_for_discovery(args).await,
            "vibemk_get_discovery_background_job" => self.get_discovery_background_job(args).await,
            _ => self
                .base
                .error_text(&format!("❌ Unknown discovery tool: {tool}")),
        }
    }
}
