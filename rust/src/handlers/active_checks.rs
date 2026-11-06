//! Port of src/vibemk/handlers/active_checks.py (ActiveChecksHandler).
//! Active checks are rules on active_checks:* rulesets; `value_raw` must be a Python literal, not JSON.

use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};
use crate::paths::path_segment;

const DAY: f64 = 86400.0;
const ALL_RULESETS: [&str; 11] = [
    "active_checks:http",
    "active_checks:tcp",
    "active_checks:icmp",
    "active_checks:dns",
    "active_checks:smtp",
    "active_checks:ftp",
    "active_checks:ldap",
    "active_checks:disk_smb",
    "active_checks:mkevents",
    "active_checks:cmk_inv",
    "custom_checks",
];

// --- Python-literal building (the rulesets' form specs want tuples, which JSON cannot express) ---
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
fn txt(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}
fn s(v: &str) -> String {
    py_repr(&json!(v))
}
fn tup(items: &[String]) -> String {
    if items.len() == 1 {
        format!("({},)", items[0])
    } else {
        format!("({})", items.join(", "))
    }
}
fn dict(items: &[(&str, String)]) -> String {
    format!(
        "{{{}}}",
        items
            .iter()
            .map(|(k, v)| format!("{}: {v}", s(k)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn fl(f: f64) -> String {
    format!("{f:?}")
}
/// A value or a list of values, as the Python list `x if isinstance(x, list) else [x]`.
fn as_list(v: &Value) -> String {
    if v.is_array() {
        py_repr(v)
    } else {
        format!("[{}]", py_repr(v))
    }
}
fn int(v: &Value) -> Result<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| anyhow!("int too large")),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(t) => t
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid literal for int() with base 10: {}", py_repr(v))),
        _ => Err(anyhow!(
            "int() argument must be a string or a real number, not {}",
            py_repr(v)
        )),
    }
}
fn flt(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| anyhow!("float too large")),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        Value::String(t) => t
            .trim()
            .parse()
            .map_err(|_| anyhow!("could not convert string to float: {}", py_repr(v))),
        _ => Err(anyhow!(
            "float() argument must be a string or a real number, not {}",
            py_repr(v)
        )),
    }
}
/// SimpleLevels in their stored form; `scale` converts to the spec's unit (seconds).
fn fixed_levels(warn: &Value, crit: &Value, scale: f64) -> Result<String> {
    Ok(tup(&[
        s("fixed"),
        tup(&[fl(flt(warn)? * scale), fl(flt(crit)? * scale)]),
    ]))
}
/// A Password form spec holding the secret itself rather than a store reference.
fn explicit_password(pw: &Value) -> String {
    tup(&[
        s("cmk_postprocessed"),
        s("explicit_password"),
        tup(&[s(""), py_repr(pw)]),
    ])
}
fn description_line(d: &str) -> String {
    if d.is_empty() {
        String::new()
    } else {
        format!("Description: {d}\n")
    }
}
fn reply(text: String) -> Result<Vec<Value>> {
    Ok(vec![json!({"type": "text", "text": text})])
}

/// Argument accessors: absent and null are the same thing.
struct Args<'a>(&'a Value);
impl<'a> Args<'a> {
    fn get(&self, k: &str) -> Option<&'a Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    /// The argument if it is truthy (Python `if arguments.get(k)`).
    fn on(&self, k: &str) -> Option<&'a Value> {
        self.get(k).filter(|v| truthy(v))
    }
    fn or(&self, k: &str, default: Value) -> Value {
        self.get(k).cloned().unwrap_or(default)
    }
    fn text(&self, k: &str, default: &str) -> String {
        self.get(k).map_or_else(|| default.to_string(), txt)
    }
}

pub struct ActiveChecksHandler {
    base: Base,
    is_23: OnceLock<bool>,
}

impl ActiveChecksHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
            is_23: OnceLock::new(),
        }
    }

    /// True on 2.3, whose LDAP and SMB rulesets still store bare (warn, crit) tuples.
    async fn plain_levels(&self) -> bool {
        if let Some(v) = self.is_23.get() {
            return *v;
        }
        let version = match self.base.client.get("version", None, &[]).await {
            Ok(r) => r
                .data
                .pointer("/versions/checkmk")
                .map_or_else(String::new, txt),
            Err(_) => String::new(),
        };
        *self.is_23.get_or_init(|| version.starts_with("2.3"))
    }

    /// 2.3: the bare (warn, crit) tuple; later: SimpleLevels scaled by `scale`.
    async fn levels(&self, warn: &Value, crit: &Value, scale: f64) -> Result<String> {
        if self.plain_levels().await {
            Ok(tup(&[fl(flt(warn)?), fl(flt(crit)?)]))
        } else {
            fixed_levels(warn, crit, scale)
        }
    }

    /// Explicit folder, else the host's own folder as "~a~b"; the root when it cannot be read.
    async fn resolve_folder(&self, hostname: &str, explicit: Option<&Value>) -> String {
        if let Some(f) = explicit {
            return txt(f);
        }
        let host = async {
            Ok::<_, anyhow::Error>(
                self.base
                    .client
                    .get(
                        &format!("objects/host_config/{}", path_segment(hostname)?),
                        None,
                        &[],
                    )
                    .await?
                    .data,
            )
        };
        match host.await {
            Ok(data) => {
                let folder = data
                    .pointer("/extensions/folder")
                    .map_or_else(|| "/".to_string(), txt);
                format!("~{}", folder.trim_start_matches('/').replace('/', "~"))
            }
            Err(_) => "~".into(),
        }
    }

    /// Create the rule; returns its id.
    async fn post_rule(
        &self,
        ruleset: &str,
        value_raw: String,
        hostname: &str,
        folder: &str,
        description: &str,
    ) -> Result<String> {
        let mut props = json!({"disabled": false});
        if !description.is_empty() {
            props["description"] = json!(description);
        }
        let body = json!({
            "ruleset": ruleset,
            "folder": folder,
            "value_raw": value_raw,
            "conditions": {
                "host_name": {"match_on": [hostname], "operator": "one_of"},
                "host_tags": [],
                "host_label_groups": [],
                "service_label_groups": [],
            },
            "properties": props,
        });
        let r = self
            .base
            .client
            .post("domain-types/rule/collections/all", Some(&body), &[])
            .await?;
        Ok(r.data.get("id").map_or_else(String::new, txt))
    }

    fn missing(&self, what: &str) -> Result<Vec<Value>> {
        Ok(self.base.error_response("Missing parameter", Some(what)))
    }

    async fn http(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let name = a.or("name", json!("HTTP"));
        let uri = a.text("uri", "/");
        let use_ssl = a.or("ssl", json!(false));
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        let (expect_string, expect_regex, expect_response, method) = (
            a.on("expect_string"),
            a.on("expect_regex"),
            a.on("expect_response"),
            a.on("method"),
        );
        let (port, virt_host, direct_address, proxy_address) = (
            a.on("port"),
            a.on("virt_host"),
            a.on("direct_address"),
            a.on("proxy_address"),
        );
        let cert_mode = a.on("cert_mode").is_some();
        let (cert_warn, cert_crit) = (
            a.or("cert_days_warn", json!(14)),
            a.or("cert_days_crit", json!(7)),
        );
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }

        let mut host: Vec<(&str, String)> = vec![];
        if let Some(p) = port {
            host.push(("port", int(p)?.to_string()));
        }
        if let Some(f) = a.on("address_family") {
            let f = txt(f);
            host.push((
                "address_family",
                s(match f.as_str() {
                    "ipv4" => "ipv4_enforced",
                    "ipv6" => "ipv6_enforced",
                    o => o,
                }),
            ));
        }
        if let Some(p) = proxy_address {
            let pp = a.on("proxy_port").map_or(Ok(80), int)?;
            host.push((
                "address",
                tup(&[
                    s("proxy"),
                    dict(&[("address", py_repr(p)), ("port", pp.to_string())]),
                ]),
            ));
        } else if let Some(d) = direct_address {
            host.push(("address", tup(&[s("direct"), py_repr(d)])));
        }
        if virt_host.is_some() || direct_address.is_none() {
            host.push(("virthost", virt_host.map_or_else(|| s(&hostname), py_repr)));
        }

        let mode = if cert_mode {
            tup(&[
                s("cert"),
                dict(&[("cert_days", fixed_levels(&cert_warn, &cert_crit, DAY)?)]),
            ])
        } else {
            let mut u: Vec<(&str, String)> = vec![("uri", s(&uri))];
            if truthy(&use_ssl) {
                u.push(("ssl", s("auto")));
            }
            if let Some(e) = expect_string {
                u.push(("expect_string", py_repr(e)));
            }
            if let Some(e) = expect_regex {
                let flags = [
                    ("case_insensitive", "False"),
                    ("crit_if_found", "False"),
                    ("multiline", "False"),
                ];
                let mut d = vec![("regex", py_repr(e))];
                d.extend(flags.iter().map(|(k, v)| (*k, v.to_string())));
                u.push(("expect_regex", dict(&d)));
            }
            if let Some(e) = expect_response {
                u.push(("expect_response", as_list(e)));
            }
            if let Some(t) = a.on("timeout") {
                u.push(("timeout", int(t)?.to_string()));
            }
            if let (Some(w), Some(c)) = (a.on("response_time_warn"), a.on("response_time_crit")) {
                u.push(("response_time", fixed_levels(w, c, 1.0)?));
            }
            if let Some(m) = method {
                u.push(("method", s(&txt(m).to_uppercase())));
            }
            if a.on("no_body").is_some() {
                u.push(("no_body", "True".into()));
            }
            if let Some(o) = a.on("onredirect") {
                u.push(("onredirect", py_repr(o)));
            }
            if a.on("extended_perfdata").is_some() {
                u.push(("extended_perfdata", "True".into()));
            }
            if let (Some(user), Some(pw)) = (a.on("auth_user"), a.on("auth_password")) {
                u.push((
                    "auth",
                    dict(&[("user", py_repr(user)), ("password", explicit_password(pw))]),
                ));
            }
            tup(&[s("url"), dict(&u)])
        };
        let value = dict(&[
            ("name", py_repr(&name)),
            ("host", dict(&host)),
            ("mode", mode),
        ]);

        let rule_id = self
            .post_rule(
                "active_checks:http",
                value,
                &hostname,
                &folder,
                &description,
            )
            .await?;
        let mut details = vec![];
        if cert_mode {
            details.push(format!(
                "Mode: certificate check (warn {}d, crit {}d)",
                txt(&cert_warn),
                txt(&cert_crit)
            ));
        } else {
            details.push(format!(
                "URI: {uri}, SSL: {}, Port: {}",
                txt(&use_ssl),
                port.map_or_else(|| "standard".into(), txt)
            ));
            for (label, v) in [
                ("Expect string", expect_string),
                ("Expect regex", expect_regex),
                ("Expect response", expect_response),
                ("Method", method),
            ] {
                details.extend(v.map(|v| format!("{label}: {}", txt(v))));
            }
        }
        if !description.is_empty() {
            details.push(format!("Description: {description}"));
        }
        details.push(format!("Rule-ID: {rule_id}"));
        reply(format!(
            "✅ HTTP check '{}' created for '{hostname}'.\n{}",
            txt(&name),
            details.join("\n")
        ))
    }

    async fn tcp(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let (port, name) = (a.on("port"), a.on("name"));
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        let use_ssl = a.or("ssl", json!(false));
        let expect = a.on("expect");
        let Some(port) = port.filter(|_| !hostname.is_empty()) else {
            return self.missing("hostname and port are required");
        };

        let mut v = vec![("port", int(port)?.to_string())];
        if let Some(n) = name {
            v.push(("svc_description", py_repr(n)));
        }
        if truthy(&use_ssl) {
            v.push(("ssl", "True".into()));
        }
        if let (Some(w), Some(c)) = (a.on("cert_days_warn"), a.on("cert_days_crit")) {
            v.push(("cert_days", fixed_levels(w, c, DAY)?));
        }
        if let Some(e) = expect {
            v.push(("expect", as_list(e)));
        }
        if let Some(r) = a.on("refuse_state") {
            v.push(("refuse_state", py_repr(r)));
        }
        if let Some(m) = a.on("mismatch_state") {
            v.push(("mismatch_state", py_repr(m)));
        }
        if let Some(t) = a.on("timeout") {
            v.push(("timeout", int(t)?.to_string()));
        }
        if let (Some(w), Some(c)) = (a.on("response_time_warn"), a.on("response_time_crit")) {
            v.push(("response_time", fixed_levels(w, c, 1.0)?));
        }

        let rule_id = self
            .post_rule(
                "active_checks:tcp",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        let svc = name.map_or_else(|| format!("TCP Port {}", txt(port)), txt);
        let expect_line = expect.map_or_else(String::new, |e| format!("Expect: {}\n", txt(e)));
        reply(format!(
            "✅ TCP check on port {} created for '{hostname}'.\nService: {svc}, SSL: {}\n{expect_line}{}Rule-ID: {rule_id}",
            txt(port), txt(&use_ssl), description_line(&description)))
    }

    async fn icmp(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let name = a.or("name", json!("PING"));
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        let (packets, timeout) = (a.or("packets", json!(5)), a.or("timeout", json!(20)));
        let (address, rta_warn, rta_crit) = (
            a.on("explicit_address"),
            a.on("rta_warn_ms"),
            a.on("rta_crit_ms"),
        );
        let (loss_warn, loss_crit) = (a.on("loss_warn_percent"), a.on("loss_crit_percent"));
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }

        let mut v = vec![
            ("description", py_repr(&name)),
            ("packets", int(&packets)?.to_string()),
            ("timeout", int(&timeout)?.to_string()),
        ];
        if let Some(ad) = address {
            v.push(("address", tup(&[s("explicit"), py_repr(ad)])));
        }
        if let (Some(w), Some(c)) = (rta_warn, rta_crit) {
            v.push(("rta", tup(&[fl(flt(w)?), fl(flt(c)?)])));
        }
        if let (Some(w), Some(c)) = (loss_warn, loss_crit) {
            v.push(("loss", tup(&[fl(flt(w)?), fl(flt(c)?)])));
        }
        if let Some(m) = a.on("min_pings") {
            v.push(("min_pings", int(m)?.to_string()));
        }

        let rule_id = self
            .post_rule(
                "active_checks:icmp",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        let none = json!(null);
        let target = address.map_or_else(String::new, |x| format!("Target IP: {}\n", txt(x)));
        let rta = rta_warn.map_or_else(String::new, |w| {
            format!(
                "RTA: warn {}ms / crit {}ms\n",
                txt(w),
                txt(rta_crit.unwrap_or(&none))
            )
        });
        let loss = loss_warn.map_or_else(String::new, |w| {
            format!(
                "Loss: warn {}% / crit {}%\n",
                txt(w),
                txt(loss_crit.unwrap_or(&none))
            )
        });
        reply(format!(
            "✅ ICMP/PING check '{}' created for '{hostname}'.\nPackets: {}, timeout: {}s\n{target}{rta}{loss}{}Rule-ID: {rule_id}",
            txt(&name), txt(&packets), txt(&timeout), description_line(&description)))
    }

    async fn custom(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let (hostname, svc, cmd) = (
            a.text("hostname", ""),
            a.text("service_description", ""),
            a.text("command_line", ""),
        );
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() || svc.is_empty() || cmd.is_empty() {
            return self.missing("hostname, service_description and command_line are required");
        }
        let mut v = vec![("service_description", s(&svc)), ("command_line", s(&cmd))];
        if let Some(n) = a.on("command_name") {
            v.push(("command_name", py_repr(n)));
        }
        let rule_id = self
            .post_rule("custom_checks", dict(&v), &hostname, &folder, "")
            .await?;
        reply(format!(
            "✅ Custom check '{svc}' created for '{hostname}'.\nCommand: {cmd}\nRule ID: {rule_id}"
        ))
    }

    async fn dns(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let lookup = a.or("lookup_hostname", json!(hostname));
        let (server, expected) = (a.on("dns_server"), a.on("expected_addresses"));
        let expect_all = a.or("expect_all_addresses", json!(false));
        let (warn, crit) = (
            flt(&a.or("response_time_warn", json!(0.2)))?,
            flt(&a.or("response_time_crit", json!(0.3)))?,
        );
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }
        let mut v = vec![
            ("hostname", py_repr(&lookup)),
            (
                "server",
                a.get("dns_server").map_or_else(|| "None".into(), py_repr),
            ),
            ("response_time", tup(&[fl(warn), fl(crit)])),
        ];
        if let Some(e) = expected {
            v.push(("expected_addresses_list", as_list(e)));
            v.push(("expect_all_addresses", py_repr(&expect_all)));
        }
        let rule_id = self
            .post_rule(
                "active_checks:dns",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        let exp = expected.map_or_else(String::new, |e| {
            format!(
                "Expected IPs: {} (all required: {})\n",
                txt(e),
                txt(&expect_all)
            )
        });
        reply(format!(
            "✅ DNS check created for '{hostname}'.\nLookup: {}, DNS server: {}\n{exp}{}Rule-ID: {rule_id}",
            txt(&lookup), server.map_or_else(|| "system default".into(), txt), description_line(&description)))
    }

    async fn smtp(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let name = a.or("name", json!("SMTP"));
        let port = a.on("port");
        let starttls = a.or("starttls", json!(false));
        let (warn, crit) = (
            a.or("cert_days_warn", json!(14)),
            a.or("cert_days_crit", json!(7)),
        );
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }
        let mut v = vec![("name", py_repr(&name))];
        if let Some(p) = port {
            v.push(("port", int(p)?.to_string()));
        }
        if truthy(&starttls) {
            v.push(("starttls", "True".into()));
        }
        if truthy(&starttls) || a.on("check_cert").is_some() {
            v.push(("cert_days", fixed_levels(&warn, &crit, DAY)?));
        }
        let rule_id = self
            .post_rule(
                "active_checks:smtp",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!(
            "✅ SMTP check '{}' created for '{hostname}'.\nPort: {}, STARTTLS: {}\n{}Rule-ID: {rule_id}",
            txt(&name), port.map_or_else(|| "default".into(), txt), txt(&starttls), description_line(&description)))
    }

    async fn ftp(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let port = int(&a.or("port", json!(21)))?;
        let refuse = a.or("refuse_state", json!("crit"));
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }
        let mut v = vec![
            ("port", port.to_string()),
            ("refuse_state", py_repr(&refuse)),
        ];
        if let Some(t) = a.get("timeout") {
            v.push(("timeout", int(t)?.to_string()));
        }
        let rule_id = self
            .post_rule(
                "active_checks:ftp",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!(
            "✅ FTP check on port {port} created for '{hostname}'.\n{}Rule-ID: {rule_id}",
            description_line(&description)
        ))
    }

    async fn ldap(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let name = a.or("name", json!("LDAP"));
        let base_dn = a.text("base_dn", "");
        let (warn, crit) = (
            a.or("response_time_warn_ms", json!(500.0)),
            a.or("response_time_crit_ms", json!(800.0)),
        );
        flt(&warn)?;
        flt(&crit)?;
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() || base_dn.is_empty() {
            return self.missing("hostname and base_dn are required");
        }
        let mut v = vec![
            ("name", py_repr(&name)),
            ("base_dn", s(&base_dn)),
            // 2.3: milliseconds as a bare tuple; later: SimpleLevels in seconds.
            ("response_time", self.levels(&warn, &crit, 0.001).await?),
        ];
        if let (Some(b), Some(p)) = (a.on("bind_dn"), a.on("password")) {
            v.push((
                "authentication",
                dict(&[("bind_dn", py_repr(b)), ("password", explicit_password(p))]),
            ));
        }
        if let Some(p) = a.on("port") {
            v.push(("port", int(p)?.to_string()));
        }
        if let Some(at) = a.on("attribute") {
            v.push(("attribute", py_repr(at)));
        }
        let rule_id = self
            .post_rule(
                "active_checks:ldap",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!("✅ LDAP check '{}' created for '{hostname}'.\nBase DN: {base_dn}\n{}Rule-ID: {rule_id}", txt(&name), description_line(&description)))
    }

    async fn smb(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let share = a.text("share", "");
        let smb_host = a.or("smb_host", json!("use_parent_host"));
        let warn = a.or("warn_percent", json!(85.0));
        let crit = a.or("crit_percent", json!(95.0));
        let (warn_f, crit_f) = (flt(&warn)?, flt(&crit)?);
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() || share.is_empty() {
            return self.missing("hostname and share are required");
        }
        let host = if smb_host == json!("use_parent_host") {
            tup(&[s("use_parent_host"), s("")])
        } else {
            tup(&[s("define_host"), py_repr(&smb_host)])
        };
        let mut v = vec![
            ("share", s(&share)),
            ("host", host),
            ("levels", self.levels(&warn, &crit, 1.0).await?),
        ];
        if let (Some(u), Some(p)) = (a.on("username"), a.on("password")) {
            v.push((
                "auth",
                dict(&[("user", py_repr(u)), ("password", explicit_password(p))]),
            ));
        }
        if let Some(w) = a.on("workgroup") {
            v.push(("workgroup", py_repr(w)));
        }
        let rule_id = self
            .post_rule(
                "active_checks:disk_smb",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!(
            "✅ SMB check on share '{share}' created for '{hostname}'.\nWarn: {}%, Crit: {}%\n{}Rule-ID: {rule_id}",
            fl(warn_f), fl(crit_f), description_line(&description)))
    }

    async fn mkevents(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let ignore_ack = truthy(&a.or("ignore_acknowledged", json!(true)));
        let show = a.text("show_last_log", "summary");
        let remote = a.on("remote_ec_host");
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }
        let mut v = vec![(
            "hostspec",
            "['$HOSTNAME$', '$HOSTADDRESS$', '$HOSTALIAS$']".to_string(),
        )];
        if ignore_ack {
            v.push(("ignore_acknowledged", "True".into()));
        }
        v.push((
            "remote",
            remote.map_or_else(|| "None".into(), |r| tup(&[py_repr(r), "6558".into()])),
        ));
        v.push((
            "show_last_log",
            s(match show.as_str() {
                "none" => "no",
                "long" => "details",
                o => o,
            }),
        ));
        let rule_id = self
            .post_rule(
                "active_checks:mkevents",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!(
            "✅ Event Console check created for '{hostname}'.\n{}Rule-ID: {rule_id}",
            description_line(&description)
        ))
    }

    async fn inventory(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.text("hostname", "");
        let state = |k: &str| int(&a.or(k, json!(0)));
        let (sw_changes, sw_missing, hw_changes, fail) = (
            state("sw_changes_state")?,
            state("sw_missing_state")?,
            state("hw_changes_state")?,
            state("fail_status")?,
        );
        let status_data = a.or("status_data_inventory", json!(true));
        let description = a.text("description", "");
        let folder = self.resolve_folder(&hostname, a.on("folder")).await;
        if hostname.is_empty() {
            return self.missing("hostname is required");
        }
        let v = [
            ("sw_changes", sw_changes.to_string()),
            ("sw_missing", sw_missing.to_string()),
            ("hw_changes", hw_changes.to_string()),
            ("fail_status", fail.to_string()),
            ("status_data_inventory", py_repr(&status_data)),
        ];
        let rule_id = self
            .post_rule(
                "active_checks:cmk_inv",
                dict(&v),
                &hostname,
                &folder,
                &description,
            )
            .await?;
        reply(format!(
            "✅ HW/SW inventory check created for '{hostname}'.\n{}Rule-ID: {rule_id}",
            description_line(&description)
        ))
    }

    fn rulesets(a: &Args) -> Vec<String> {
        match a.on("check_type").map(txt) {
            Some(c) if c == "custom" => vec!["custom_checks".into()],
            Some(c) => vec![format!("active_checks:{c}")],
            None => ALL_RULESETS.map(String::from).to_vec(),
        }
    }

    /// (ruleset, rule) for every rule of the given rulesets, fetched at most 4 at a time.
    async fn rules_of(&self, rulesets: Vec<String>) -> Result<Vec<(String, Value)>> {
        let client = &self.base.client;
        let fetched = self
            .base
            .map_concurrently(rulesets.clone(), |rs| async move {
                client
                    .get(
                        "domain-types/rule/collections/all",
                        Some(&json!({"ruleset_name": rs})),
                        &[],
                    )
                    .await
            });
        let mut out = vec![];
        for (rs, res) in rulesets.into_iter().zip(fetched.await) {
            let rules = res?
                .data
                .get("value")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            out.extend(rules.into_iter().map(|r| (rs.clone(), r)));
        }
        Ok(out)
    }

    async fn list(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let hostname = a.on("hostname").map(txt);
        let mut lines = vec!["🔍 **Active Checks**\n".to_string()];
        for (ruleset, rule) in self.rules_of(Self::rulesets(&a)).await? {
            let hosts = rule_hosts(&rule);
            if hostname.as_ref().is_some_and(|h| !hosts.contains(h)) {
                continue;
            }
            let val_raw = rule
                .pointer("/extensions/value_raw")
                .map_or_else(String::new, txt);
            let disabled = rule
                .pointer("/extensions/properties/disabled")
                .is_some_and(truthy);
            let shown: String = val_raw.chars().take(120).collect();
            lines.push(format!(
                "• [{}] **{ruleset}** — {}\n  ID: {}\n  Value: `{shown}{}`",
                if disabled {
                    "🔴 disabled"
                } else {
                    "🟢 active"
                },
                if hosts.is_empty() {
                    "all hosts".to_string()
                } else {
                    hosts.join(", ")
                },
                rule.get("id").map_or_else(String::new, txt),
                if val_raw.chars().count() > 120 {
                    "..."
                } else {
                    ""
                },
            ));
        }
        if lines.len() == 1 {
            lines.push("No active check rules found.".into());
        }
        reply(lines.join("\n"))
    }

    async fn delete(&self, a: &Value) -> Result<Vec<Value>> {
        let a = Args(a);
        let mut rule_id = a.on("rule_id").map(txt);
        if let (None, Some(hostname)) = (&rule_id, a.on("hostname").map(txt)) {
            let service = a.on("service_name").map(txt);
            let wanted = service.as_ref().map(|sv| sv.to_lowercase());
            let matches: Vec<(String, String, String)> = self
                .rules_of(Self::rulesets(&a))
                .await?
                .into_iter()
                .filter(|(_, r)| rule_hosts(r).contains(&hostname))
                .map(|(rs, r)| {
                    (
                        rs,
                        r.get("id").map_or_else(|| "None".into(), txt),
                        r.pointer("/extensions/value_raw")
                            .map_or_else(String::new, txt),
                    )
                })
                .filter(|(_, _, vr)| {
                    wanted
                        .as_ref()
                        .is_none_or(|w| vr.to_lowercase().contains(w))
                })
                .collect();
            match matches.as_slice() {
                [] => {
                    let svc =
                        service.map_or_else(String::new, |sv| format!(" with service '{sv}'"));
                    return Ok(self.base.error_response(
                        "No rule found",
                        Some(&format!("No active check rule for '{hostname}'{svc}.")),
                    ));
                }
                [(_, id, _)] => rule_id = Some(id.clone()),
                many => {
                    let mut lines = vec![format!(
                        "Several rules match '{hostname}'. Name one with rule_id:\n"
                    )];
                    lines.extend(many.iter().map(|(rs, id, vr)| {
                        format!(
                            "• {rs}: {id}\n  Value: `{}`",
                            vr.chars().take(100).collect::<String>()
                        )
                    }));
                    return reply(lines.join("\n"));
                }
            }
        }
        let Some(rule_id) = rule_id else {
            return self.missing("Provide rule_id or hostname");
        };
        self.base
            .client
            .delete(
                &format!("objects/rule/{}", path_segment(&rule_id)?),
                None,
                &[],
            )
            .await?;
        reply(format!("✅ Active check rule '{rule_id}' deleted."))
    }
}

fn rule_hosts(rule: &Value) -> Vec<String> {
    rule.pointer("/extensions/conditions/host_name/match_on")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |a| a.iter().map(txt).collect())
}

#[async_trait]
impl Handler for ActiveChecksHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_create_http_check" => self.http(args).await,
            "vibemk_create_tcp_check" => self.tcp(args).await,
            "vibemk_create_icmp_check" => self.icmp(args).await,
            "vibemk_create_custom_check" => self.custom(args).await,
            "vibemk_create_dns_check" => self.dns(args).await,
            "vibemk_create_smtp_check" => self.smtp(args).await,
            "vibemk_create_ftp_check" => self.ftp(args).await,
            "vibemk_create_ldap_check" => self.ldap(args).await,
            "vibemk_create_smb_check" => self.smb(args).await,
            "vibemk_create_mkevents_check" => self.mkevents(args).await,
            "vibemk_create_inventory_check" => self.inventory(args).await,
            "vibemk_list_active_checks" => self.list(args).await,
            "vibemk_delete_active_check" => self.delete(args).await,
            _ => {
                return self.base.error_response(
                    "Unknown tool",
                    Some(&format!("Tool '{tool}' is not supported")),
                )
            }
        };
        result.unwrap_or_else(|e| match e.downcast_ref::<ApiError>() {
            Some(api) => self
                .base
                .error_response("CheckMK API Error", Some(&api.to_string())),
            None => {
                tracing::error!("Error in {tool}: {e}");
                self.base
                    .error_response("Unexpected Error", Some(&e.to_string()))
            }
        })
    }
}
