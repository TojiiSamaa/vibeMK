//! Port of src/vibemk/handlers/acknowledgements.py (AcknowledgementHandler).
//!
//! A failed CheckMK call is `Err` here (Python raised `CheckMKError`, caught by each tool's
//! broad `except`), so the Python "success is false" branches cannot happen and are not ported.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, ApiErrorKind, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

const ACKNOWLEDGEMENT_COMMENTS: &str = "domain-types/comment/collections/all";
/// Livestatus comments.entry_type: 1 user, 2 downtime, 3 flapping, 4 acknowledgement.
const ACKNOWLEDGEMENT_ENTRY_TYPE: &str = "4";
const REMOVE_ACKNOWLEDGEMENT: &str = "domain-types/acknowledge/actions/delete/invoke";
const NO_REMOVE_ENDPOINT: &str = "❌ This CheckMK version has no REST endpoint to remove an acknowledgement\n\nRemove it in the GUI; a non-sticky acknowledgement also clears itself when the state changes.";

pub struct AcknowledgementHandler {
    base: Base,
    /// Detected once per handler ("2.4+" or "2.3").
    version: Mutex<Option<&'static str>>,
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

fn text(t: String) -> Vec<Value> {
    vec![json!({"type": "text", "text": t})]
}

/// Query asking CheckMK for acknowledgements only (exact, not guessed from the comment text).
fn ack_query() -> Value {
    json!({"query": {"op": "=", "left": "entry_type", "right": ACKNOWLEDGEMENT_ENTRY_TYPE}})
}

type R = Result<Vec<Value>, ApiError>;

impl AcknowledgementHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
            version: Mutex::new(None),
        }
    }

    /// Port 8081 means 2.4+, as the Python fallback guesses.
    fn version_from_port(&self) -> &'static str {
        if self.base.client.config.server_url.contains(":8081") {
            "2.4+"
        } else {
            "2.3"
        }
    }

    async fn detect_version(&self) -> &'static str {
        if let Some(v) = *self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return v;
        }
        let detected = match self.base.client.get("version", None, &[]).await {
            Ok(r) => {
                let v = r
                    .data
                    .pointer("/versions/checkmk")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if v.starts_with("2.4") || v.starts_with("2.5") {
                    "2.4+"
                } else {
                    "2.3"
                }
            }
            Err(e) => {
                tracing::warn!("Could not detect CheckMK version, using port fallback: {e}");
                self.version_from_port()
            }
        };
        *self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detected);
        detected
    }

    /// Acknowledge a host (`service` None) or a service problem.
    async fn acknowledge(&self, args: &Value, service: bool) -> Result<Vec<Value>, String> {
        let kind = if service { "service" } else { "host" };
        let host = arg(args, "host_name");
        let desc = arg(args, "service_description");
        if host.is_none() || (service && desc.is_none()) {
            return Ok(self.base.error_text(if service {
                "❌ Error: host_name and service_description are required"
            } else {
                "❌ Error: host_name is required"
            }));
        }
        let host = host.cloned().unwrap_or(Value::Null);
        let comment = args
            .get("comment")
            .cloned()
            .unwrap_or_else(|| json!("Problem acknowledged via vibeMK"));
        let flag = |k: &str| args.get(k).cloned().unwrap_or(json!(false));
        let (sticky, persistent, notify) = (flag("sticky"), flag("persistent"), flag("notify"));
        let expire_on = arg(args, "expire_on");

        let version = self.detect_version().await;
        let mut data = json!({
            "acknowledge_type": kind, "sticky": sticky, "persistent": persistent,
            "notify": notify, "comment": comment, "host_name": host,
        });
        if let Some(d) = desc.filter(|_| service) {
            data["service_description"] = d.clone();
        }
        if let Some(e) = expire_on.filter(|_| version == "2.4+") {
            data["expire_on"] = e.clone();
        }

        self.base
            .client
            .post(
                &format!("domain-types/acknowledge/collections/{kind}"),
                Some(&data),
                &[],
            )
            .await
            .map_err(|e| e.to_string())?;

        let expire_info = if version == "2.4+" {
            format!(
                "Expire on: {}\n",
                expire_on.map_or_else(|| "Never".to_string(), py_str)
            )
        } else {
            String::new()
        };
        let title = if service { "Service" } else { "Host" };
        let service_line = desc
            .filter(|_| service)
            .map_or_else(String::new, |d| format!("Service: **{}**\n", py_str(d)));
        Ok(text(format!(
            "✅ **{title} Acknowledgement Created**\n\nHost: **{}**\n{service_line}Comment: {}\nSticky: {}\nPersistent: {}\nNotify: {}\n{expire_info}\nCheckMK Version: {version}\n\nThe {kind} problem has been acknowledged and will not generate further alerts.",
            py_str(&host),
            py_str(&comment),
            py_str(&sticky),
            py_str(&persistent),
            py_str(&notify),
        )))
    }

    /// Remove the acknowledgement of one host or service. Deleting its comment leaves the problem
    /// acknowledged (measured on 2.3 and 2.5); CheckMK 2.3 lacks this action and answers 404.
    async fn remove_acknowledgement_of(
        &self,
        host: &str,
        service: Option<&str>,
    ) -> Result<(), ApiError> {
        let kind = if service.is_some() { "service" } else { "host" };
        let mut body = json!({"acknowledge_type": kind, "host_name": host});
        if let Some(s) = service {
            body["service_description"] = json!(s);
        }
        self.base
            .client
            .post(REMOVE_ACKNOWLEDGEMENT, Some(&body), &[])
            .await?;
        Ok(())
    }

    async fn list_acknowledgements(&self) -> Result<Vec<Value>, String> {
        let result = self
            .base
            .client
            .get(ACKNOWLEDGEMENT_COMMENTS, Some(&ack_query()), &[])
            .await
            .map_err(|e| e.to_string())?;
        let empty = vec![];
        let acks = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        if acks.is_empty() {
            return Ok(text(
                "✅ **Acknowledgements List**\n\nNo active acknowledgements found.".into(),
            ));
        }

        let mut out = String::from("✅ **Current Acknowledgements**\n");
        for (i, ack) in acks.iter().enumerate() {
            let i = i + 1;
            let ext = ack.get("extensions").cloned().unwrap_or_else(|| json!({}));
            let id = ack.get("id").map_or_else(|| format!("ack-{i}"), py_str);
            let is_service = ext.get("is_service").is_some_and(truthy);
            let service_info = if is_service {
                "\nType: Service acknowledgement"
            } else {
                ""
            };
            out.push_str(&format!(
                "\n**{i}. Acknowledgement #{id}**\nType: {}\nHost: **{}**{service_info}\nComment: {}\nAuthor: {}\nEntry time: {}\nPersistent: {}\n",
                if is_service { "Service" } else { "Host" },
                get_or(&ext, "host_name", "Unknown"),
                get_or(&ext, "comment", "No comment"),
                get_or(&ext, "author", "Unknown"),
                get_or(&ext, "entry_time", "Unknown"),
                get_or(&ext, "persistent", "False"),
            ));
        }
        Ok(text(out))
    }

    async fn remove_acknowledgement(&self, args: &Value) -> Result<Vec<Value>, String> {
        self.remove(args).await.map_err(|e| e.to_string())
    }

    async fn remove(&self, args: &Value) -> R {
        let id = arg(args, "acknowledgement_id").map(py_str);
        let pattern = arg(args, "comment_pattern")
            .filter(|_| args.get("delete_all_matching").is_some_and(truthy))
            .map(|p| py_str(p).to_lowercase());
        let mut targets: Vec<(String, Option<String>)> = if id.is_some() || pattern.is_some() {
            // Ids and texts belong to the acknowledgement comments; each names the host or service to clear.
            let list = self
                .base
                .client
                .get(ACKNOWLEDGEMENT_COMMENTS, Some(&ack_query()), &[])
                .await?;
            let empty = vec![];
            let found: Vec<_> = list
                .data
                .get("value")
                .and_then(Value::as_array)
                .unwrap_or(&empty)
                .iter()
                .filter(|c| match (&id, &pattern) {
                    (Some(id), _) => c.get("id").map(py_str).as_ref() == Some(id),
                    (None, Some(p)) => get_or(&c["extensions"], "comment", "")
                        .to_lowercase()
                        .contains(p.as_str()),
                    _ => false,
                })
                .map(|c| {
                    let ext = &c["extensions"];
                    let service = ext
                        .get("is_service")
                        .is_some_and(truthy)
                        .then(|| get_or(ext, "service_description", ""));
                    (get_or(ext, "host_name", ""), service)
                })
                .collect();
            if found.is_empty() {
                let label = id.as_ref().map_or_else(
                    || format!("matching pattern '{}'", py_str(&args["comment_pattern"])),
                    |id| format!("#{id}"),
                );
                return Ok(self
                    .base
                    .error_text(&format!("❌ No acknowledgements found {label}")));
            }
            found
        } else if let Some(host) = arg(args, "host_name") {
            vec![(py_str(host), arg(args, "service_description").map(py_str))]
        } else {
            return Ok(self.base.error_text(
                "❌ Error: acknowledgement_id, host_name, or comment_pattern is required",
            ));
        };
        targets.sort();
        targets.dedup();
        for (host, service) in &targets {
            match self
                .remove_acknowledgement_of(host, service.as_deref())
                .await
            {
                Err(e) if e.kind == ApiErrorKind::NotFound => {
                    return Ok(self.base.error_text(NO_REMOVE_ENDPOINT))
                }
                r => r?,
            }
        }
        let names: Vec<String> = targets
            .iter()
            .map(|(h, s)| s.as_ref().map_or_else(|| h.clone(), |s| format!("{h}/{s}")))
            .collect();
        Ok(text(format!(
            "✅ Acknowledgement removed for {}",
            names.join(", ")
        )))
    }
}

#[async_trait]
impl Handler for AcknowledgementHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let (result, context) = match tool {
            "vibemk_acknowledge_host_problem" => (
                self.acknowledge(args, false).await,
                "acknowledging host problem",
            ),
            "vibemk_acknowledge_service_problem" => (
                self.acknowledge(args, true).await,
                "acknowledging service problem",
            ),
            "vibemk_list_acknowledgements" => (
                self.list_acknowledgements().await,
                "listing acknowledgements",
            ),
            "vibemk_remove_acknowledgement" => (
                self.remove_acknowledgement(args).await,
                "removing acknowledgement",
            ),
            _ => {
                return self
                    .base
                    .error_text(&format!("❌ Unknown acknowledgement tool: {tool}"))
            }
        };
        result.unwrap_or_else(|e| self.base.error_text(&format!("❌ Error {context}: {e}")))
    }
}
