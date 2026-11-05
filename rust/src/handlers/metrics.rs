//! Port of src/vibemk/handlers/metrics.py (MetricsHandler).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

const MAX_DISPLAYED_METRICS: usize = 5;
const MAX_LISTED_METRICS: usize = 20;
const METRIC_ENDPOINT: &str = "domain-types/metric/actions";

/// Python `str()` of a JSON value: a string stays as is.
fn st(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// `d.get(key, default)` rendered with str().
fn field(d: &Value, key: &str, default: &str) -> String {
    d.get(key).map_or_else(|| default.to_string(), st)
}

/// A non-empty string argument (Python truthiness for the required ones).
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn arg_or<'a>(args: &'a Value, key: &str, default: &'a str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn reduce_of(args: &Value) -> Value {
    args.get("reduce").cloned().unwrap_or_else(|| json!("max"))
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn points_of(metric: &Value) -> &[Value] {
    metric
        .get("data_points")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn metrics_of(data: &Value) -> &[Value] {
    data.get("metrics")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// One curve of a metric answer, reduced to what a reader compares (series_summary).
/// The newest bucket is usually empty, so "latest" is the last point carrying a value.
pub fn series_summary(metric: &Value) -> Value {
    let points = points_of(metric);
    let values: Vec<&Value> = points.iter().filter(|p| num(p).is_some()).collect();
    // First extreme wins on ties, like Python's min()/max().
    let pick = |better: fn(f64, f64) -> bool| {
        values.iter().copied().reduce(|a, b| {
            if better(num(b).unwrap_or(0.0), num(a).unwrap_or(0.0)) {
                b
            } else {
                a
            }
        })
    };
    let avg = (!values.is_empty()).then(|| {
        let mean = values.iter().filter_map(|v| num(v)).sum::<f64>() / values.len() as f64;
        json!((mean * 1e4).round() / 1e4)
    });
    json!({
        "title": field(metric, "title", ""),
        "points": points.len(),
        "latest": values.last().map_or(Value::Null, |v| (*v).clone()),
        "min": pick(|b, a| b < a).cloned(),
        "max": pick(|b, a| b > a).cloned(),
        "avg": avg,
    })
}

pub struct MetricsHandler {
    base: Base,
}

impl MetricsHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    /// UTC window with a Z suffix: CheckMK reads an offset-less stamp as site local time.
    pub fn parse_time_range(time_range: &str) -> Value {
        let span = match time_range {
            "4h" => Duration::hours(4),
            "24h" => Duration::days(1),
            "7d" => Duration::days(7),
            "30d" => Duration::days(30),
            _ => Duration::hours(1),
        };
        let now = Utc::now();
        let fmt = "%Y-%m-%dT%H:%M:%SZ";
        json!({"start": (now - span).format(fmt).to_string(), "end": now.format(fmt).to_string()})
    }

    /// Diagnostic of a recognised error status; None for the others.
    fn http_error_message(e: &ApiError, host: &str, service: &str, metric: &str) -> Option<String> {
        let data = &e.response_data;
        match e.status_code.unwrap_or(0) {
            400 => Some(Self::handle_400(data, host, service, metric)),
            406 => Some(Self::handle_406(data)),
            415 => Some(Self::handle_415(data)),
            _ => None,
        }
    }

    pub fn handle_400(data: &Value, host: &str, service: &str, metric: &str) -> String {
        let title = field(data, "title", "Bad Request");
        let detail = field(data, "detail", "");
        let has = |s: &str| detail.contains(s);
        if has("time_range") || has("start") || has("end") {
            format!("Time range parameter error: {detail}. Timestamps are sent as ISO-8601 UTC, e.g. 2026-09-14T12:00:00Z")
        } else if has("metric_id") || has(metric) {
            format!("Invalid metric ID '{metric}': {detail}. Metric may not exist for this service")
        } else if has("host_name") || has(host) {
            format!("Host parameter error: {detail}. Host '{host}' may not exist")
        } else if has("service_description") || has(service) {
            format!("Service parameter error: {detail}. Service '{service}' may not exist on host '{host}'")
        } else if has("site") {
            format!("Site parameter error: {detail}. Check CheckMK site configuration")
        } else {
            format!("Parameter validation failed: {title} - {detail}")
        }
    }

    fn handle_406(data: &Value) -> String {
        let (title, detail) = (
            field(data, "title", "Not Acceptable"),
            field(data, "detail", ""),
        );
        format!("Accept header issue: {title}. CheckMK API cannot satisfy the requested content type. Detail: {detail}")
    }

    fn handle_415(data: &Value) -> String {
        let (title, detail) = (
            field(data, "title", "Unsupported Media Type"),
            field(data, "detail", ""),
        );
        format!("Content-Type issue: {title}. Request content type not supported by CheckMK API. Detail: {detail}")
    }

    /// Host and service requests: "HTTP {status}: {title or str(e)}" when no dedicated diagnostic.
    fn error_message(e: &ApiError, host: &str, service: &str, metric: &str) -> String {
        Self::http_error_message(e, host, service, metric).unwrap_or_else(|| {
            let title = e
                .response_data
                .get("title")
                .map_or_else(|| e.to_string(), st);
            format!("HTTP {}: {title}", e.status_code.unwrap_or(0))
        })
    }

    async fn get_host_metrics(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let Some(host) = arg(args, "host_name") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_name is required")));
        };
        let time_range = arg_or(args, "time_range", "1h");
        let Some(metric) = arg(args, "metric_name") else {
            return self.metric_listing(host, None).await;
        };
        // Host metrics sit under CheckMK's pseudo-service _HOST_ (without it: 400).
        let body = json!({
            "time_range": Self::parse_time_range(time_range),
            "reduce": reduce_of(args),
            "site": self.base.client.config.site,
            "host_name": host,
            "service_description": "_HOST_",
            "type": "single_metric",
            "metric_id": metric,
        });
        match self
            .base
            .client
            .post(&format!("{METRIC_ENDPOINT}/get/invoke"), Some(&body), &[])
            .await
        {
            Ok(r) => Ok(self.base.structured_response(
                &Self::format_series("Host", host, None, metric, &r.data, time_range),
                Self::series_data(host, None, metric, time_range, &r.data),
            )),
            Err(e) => {
                let msg = Self::error_message(&e, host, "", metric);
                Ok(self.base.error_response(
                    "Failed to retrieve host metrics",
                    Some(&format!(
                        "Could not get metric '{metric}' for host '{host}': {msg}"
                    )),
                ))
            }
        }
    }

    async fn get_service_metrics(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let (Some(host), Some(service)) =
            (arg(args, "host_name"), arg(args, "service_description"))
        else {
            return Ok(self.base.error_response(
                "Missing parameters",
                Some("host_name and service_description are required"),
            ));
        };
        let time_range = arg_or(args, "time_range", "1h");
        let Some(metric) = arg(args, "metric_name") else {
            return self.metric_listing(host, Some(service)).await;
        };
        let body = json!({
            "time_range": Self::parse_time_range(time_range),
            "reduce": reduce_of(args),
            "site": self.base.client.config.site,
            "host_name": host,
            "service_description": service,
            "type": "single_metric",
            "metric_id": metric,
        });
        match self
            .base
            .client
            .post(&format!("{METRIC_ENDPOINT}/get/invoke"), Some(&body), &[])
            .await
        {
            Ok(r) => Ok(self.base.structured_response(
                &Self::format_series("Service", host, Some(service), metric, &r.data, time_range),
                Self::series_data(host, Some(service), metric, time_range, &r.data),
            )),
            Err(e) => {
                let msg = Self::error_message(&e, host, service, metric);
                // Name the metrics the service does have, so the next attempt can succeed.
                if let Ok(Some(available)) = self.available_metrics(host, Some(service)).await {
                    if !available.is_empty() {
                        return Ok(self.base.error_text(&format!(
                            "❌ **Metrics Request Failed**\n\nService: {host}/{service}\nRequested metric: {metric}\nError: {msg}\n\n✅ **Available Metrics:** {}\n\n💡 **Suggestion:** Try one of these metric IDs instead",
                            available.join(", ")
                        )));
                    }
                }
                Ok(self.base.error_response(
                    "Failed to retrieve service metrics",
                    Some(&format!(
                        "Could not get metrics for '{metric}' on '{host}/{service}': {msg}"
                    )),
                ))
            }
        }
    }

    async fn list_available_metrics(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let Some(host) = arg(args, "host_name") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_name is required")));
        };
        self.metric_listing(host, arg(args, "service_description"))
            .await
    }

    /// The metric IDs CheckMK records for a host or service; None when it does not exist.
    /// The `metrics` column is only returned when asked for.
    async fn available_metrics(
        &self,
        host: &str,
        service: Option<&str>,
    ) -> Result<Option<Vec<String>>, ApiError> {
        let (endpoint, params) = match service {
            Some(s) => (
                "domain-types/service/collections/all",
                json!({
                    "columns": ["host_name", "description", "metrics"],
                    "query": {"op": "and", "expr": [
                        {"op": "=", "left": "host_name", "right": host},
                        {"op": "=", "left": "description", "right": s},
                    ]},
                }),
            ),
            None => (
                "domain-types/host/collections/all",
                json!({"columns": ["name", "metrics"], "query": {"op": "=", "left": "name", "right": host}}),
            ),
        };
        let result = self.base.client.get(endpoint, Some(&params), &[]).await?;
        let Some(first) = result
            .data
            .get("value")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        else {
            return Ok(None);
        };
        let metrics = first
            .pointer("/extensions/metrics")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |m| m.iter().map(st).collect());
        Ok(Some(metrics))
    }

    /// Answers "which metrics are there" for a host or one of its services.
    async fn metric_listing(
        &self,
        host: &str,
        service: Option<&str>,
    ) -> Result<Vec<Value>, ApiError> {
        let target = service.map_or_else(|| host.to_string(), |s| format!("{host}/{s}"));
        let Some(available) = self.available_metrics(host, service).await? else {
            return Ok(self.base.error_response(
                "Host/service not found",
                Some(&format!("No data found for {target}")),
            ));
        };
        let data = json!({
            "host_name": host,
            "service_description": service,
            "metric_id": null,
            "available_metrics": available,
            "series": [],
        });
        if available.is_empty() {
            return Ok(self.base.structured_response(
                &format!("📊 **No Metrics Available**\n\nNo historical metrics found for {target}"),
                data,
            ));
        }
        let list = available
            .iter()
            .take(MAX_LISTED_METRICS)
            .map(|m| format!("📈 {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        let more = if available.len() > MAX_LISTED_METRICS {
            format!(
                "\n\n... and {} more metrics",
                available.len() - MAX_LISTED_METRICS
            )
        } else {
            String::new()
        };
        Ok(self.base.structured_response(
            &format!(
                "📊 **Available Metrics**\n\nTarget: {target}\nMetrics ({} total):\n\n{list}{more}\n\n💡 **Usage:** pass one of these as metric_name to get its values",
                available.len()
            ),
            data,
        ))
    }

    /// The METRIC_DATA object for a metric answer.
    fn series_data(
        host: &str,
        service: Option<&str>,
        metric: &str,
        time_range: &str,
        data: &Value,
    ) -> Value {
        let window = data.get("time_range").cloned().unwrap_or(Value::Null);
        let step = data
            .get("step")
            .filter(|s| s.is_i64() || s.is_u64())
            .cloned();
        json!({
            "host_name": host,
            "service_description": service,
            "metric_id": metric,
            "time_range": time_range,
            "start": window.get("start"),
            "end": window.get("end"),
            "step": step,
            "series": metrics_of(data).iter().map(series_summary).collect::<Vec<_>>(),
        })
    }

    /// Host and service answers differ only in their headline and empty-case wording.
    fn format_series(
        kind: &str,
        host: &str,
        service: Option<&str>,
        metric: &str,
        data: &Value,
        time_range: &str,
    ) -> String {
        let metrics = metrics_of(data);
        let target = service.map_or_else(|| host.to_string(), |s| format!("{host}/{s}"));
        if metrics.is_empty() {
            let place = if service.is_some() {
                target.clone()
            } else {
                format!("host {host}")
            };
            return format!("📊 **No Metrics Data**\n\nNo data available for metric '{metric}' on {place} in the last {time_range}");
        }
        let mut out = format!("📊 **{kind} Metrics: {target}**\n\nMetric: {metric}\nTime Range: {time_range}\nMetrics: {}\n\n", metrics.len());
        for (i, m) in metrics.iter().enumerate() {
            let points = points_of(m);
            if points.is_empty() {
                continue;
            }
            let s = series_summary(m);
            let (latest, stats) = if s["latest"].is_null() {
                (
                    "No data".to_string(),
                    "N/A (no values in this window)".to_string(),
                )
            } else {
                (
                    st(&s["latest"]),
                    format!(
                        "{} / {} / {:.2}",
                        st(&s["min"]),
                        st(&s["max"]),
                        s["avg"].as_f64().unwrap_or(0.0)
                    ),
                )
            };
            out.push_str(&format!(
                "📈 **{}**\n   Latest Value: {latest}\n   Min/Max/Avg: {stats}\n   Data Points: {}\n   Line Type: {}\n   Color: {}\n\n",
                field(m, "title", &format!("Metric {}", i + 1)),
                points.len(),
                field(m, "line_type", "line"),
                field(m, "color", "#000000"),
            ));
        }
        out.push_str(
            "💡 **Tip:** Use different time_range values (4h, 24h, 7d, 30d) for longer periods",
        );
        out
    }

    /// List formatter shared by the custom graph and search answers.
    fn format_metrics(target: &str, target_type: &str, data: &Value, time_range: &str) -> String {
        let metrics = metrics_of(data);
        if metrics.is_empty() {
            return format!(
                "📊 **No Metrics Data**\n\nNo data available for {target} in the last {time_range}"
            );
        }
        let mut out = format!("📊 **{target_type} Metrics: {target}**\n\nTime Range: {time_range}\nMetrics: {} found\n\n", metrics.len());
        for (i, m) in metrics.iter().take(MAX_DISPLAYED_METRICS).enumerate() {
            let points = points_of(m);
            if points.is_empty() {
                continue;
            }
            // The most recent bucket is often empty: report the last point with a value.
            let latest = points
                .iter()
                .rev()
                .find(|p| !p.is_null())
                .map_or_else(|| "No data".to_string(), st);
            out.push_str(&format!(
                "📈 **{}**\n   Latest: {latest}\n   Data points: {}\n   Line type: {}\n   Color: {}\n\n",
                field(m, "title", &format!("Metric {}", i + 1)),
                points.len(),
                field(m, "line_type", "line"),
                field(m, "color", "#000000"),
            ));
        }
        if metrics.len() > MAX_DISPLAYED_METRICS {
            out.push_str(&format!(
                "... and {} more metrics\n",
                metrics.len() - MAX_DISPLAYED_METRICS
            ));
        }
        out.push_str("\n💡 **Use specific metric_name for detailed data**");
        out
    }

    async fn get_custom_graph(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let Some(graph) = arg(args, "custom_graph_id") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("custom_graph_id is required")));
        };
        let time_range = arg_or(args, "time_range", "1h");
        let body = json!({"time_range": Self::parse_time_range(time_range), "reduce": reduce_of(args), "custom_graph_id": graph});
        match self
            .base
            .client
            .post(
                &format!("{METRIC_ENDPOINT}/get_custom_graph/invoke"),
                Some(&body),
                &[],
            )
            .await
        {
            Ok(r) => {
                let text = format!(
                    "📊 **Custom Graph: {graph}**\n\nTime Range: {time_range}\n\n{}",
                    Self::format_metrics(graph, "Custom Graph", &r.data, time_range)
                );
                Ok(vec![json!({"type": "text", "text": text})])
            }
            Err(e) => {
                // Unlike host/service, the generic branch prints str(e), not the CheckMK title.
                let msg = Self::http_error_message(&e, "", "", graph)
                    .unwrap_or_else(|| format!("HTTP {}: {e}", e.status_code.unwrap_or(0)));
                Ok(self.base.error_response(
                    "Failed to retrieve custom graph",
                    Some(&format!("Could not get custom graph '{graph}': {msg}")),
                ))
            }
        }
    }

    async fn search_metrics(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let Some(host_filter) = arg(args, "host_filter") else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_filter is required")));
        };
        let site = args
            .get("site_filter")
            .cloned()
            .unwrap_or_else(|| json!(self.base.client.config.site));
        let time_range = arg_or(args, "time_range", "1h");
        let (graph_id, metric_id) = (arg(args, "graph_id"), arg(args, "metric_id"));
        if graph_id.is_some() && metric_id.is_some() {
            return Ok(self.base.error_response(
                "Conflicting parameters",
                Some("Give either graph_id or metric_id, not both"),
            ));
        }
        if graph_id.is_none() && metric_id.is_none() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("Either graph_id or metric_id is required. Both are shown in the service view once 'Show internal IDs' is enabled in its display options — a graph ID in the graph title, a metric ID in the legend."),
            ));
        }
        let service = arg(args, "service_filter");
        let mut filter = Map::new();
        filter.insert("siteopt".into(), json!({"site": site}));
        filter.insert("host".into(), json!({"host": host_filter}));
        if let Some(s) = service {
            filter.insert("service".into(), json!({"service": s}));
        }
        let mut body = json!({"time_range": Self::parse_time_range(time_range), "reduce": reduce_of(args), "filter": filter});
        if let Some(g) = graph_id {
            body["type"] = json!("predefined_graph");
            body["graph_id"] = json!(g);
        } else {
            body["type"] = json!("single_metric");
            body["metric_id"] = json!(metric_id);
        }
        let r = self
            .base
            .client
            .post(
                &format!("{METRIC_ENDPOINT}/filter/invoke"),
                Some(&body),
                &[],
            )
            .await?;
        let target = format!(
            "{host_filter}{}",
            service.map_or(String::new(), |s| format!("/{s}"))
        );
        let text = format!(
            "🔍 **Metrics Search Results**\n\nFilter: {target}\n\n{}",
            Self::format_metrics(&target, "Search", &r.data, time_range)
        );
        Ok(vec![json!({"type": "text", "text": text})])
    }
}

#[async_trait]
impl Handler for MetricsHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_get_host_metrics" => self.get_host_metrics(args).await,
            "vibemk_get_service_metrics" => self.get_service_metrics(args).await,
            "vibemk_get_custom_graph" => self.get_custom_graph(args).await,
            "vibemk_search_metrics" => self.search_metrics(args).await,
            "vibemk_list_available_metrics" => self.list_available_metrics(args).await,
            _ => Ok(self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            )),
        };
        result.unwrap_or_else(|e| {
            self.base
                .error_response("CheckMK API Error", Some(&e.to_string()))
        })
    }
}
