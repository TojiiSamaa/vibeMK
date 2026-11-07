//! Port of src/vibemk/handlers/configuration.py (ConfigurationHandler).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{activation_refusal, py_repr, ActivationRefusal, Base, Handler};

const PENDING: &str = "domain-types/activation_run/collections/pending_changes";
const MAX_LISTED_CHANGES: usize = 15;

/// Python `str()` of a JSON value: a string stays as is.
fn st(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// `change.get(key) or default`, as text.
fn or_default(change: &Value, key: &str, default: &str) -> String {
    match change.get(key) {
        Some(Value::Null) | None => default.to_string(),
        Some(Value::String(s)) if s.is_empty() => default.to_string(),
        Some(v) => st(v),
    }
}

fn pending_list(data: &Value) -> &[Value] {
    data.get("value")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub struct ConfigurationHandler {
    base: Base,
}

impl ConfigurationHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
        }
    }

    async fn activate_changes(&self, args: &Value) -> Result<Vec<Value>, ApiError> {
        let config = &self.base.client.config;
        if config.never_activate_changes {
            return Ok(vec![
                json!({"type": "text", "text": "🚫 **Change Activation Disabled**\n\nConfiguration changes activation has been disabled by the `NEVER_ACTIVATE_CHANGES` setting.\n\n**Reason:** This is typically used in development or testing environments to prevent accidental activation of configuration changes.\n\n**To enable activation:**\n• Remove the `NEVER_ACTIVATE_CHANGES` environment variable, or\n• Set `NEVER_ACTIVATE_CHANGES=false`\n\n**Current configuration:** `NEVER_ACTIVATE_CHANGES=true`\n\n💡 **Note:** You can still view pending changes using `get_pending_changes`."}),
            ]);
        }
        let sites: Vec<String> = args
            .get("sites")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |a| a.iter().map(st).collect());
        let force = args
            .get("force_foreign_changes")
            .cloned()
            .unwrap_or(Value::Bool(false));

        let used_sites = if sites.is_empty() {
            vec![config.site.clone()]
        } else {
            sites
        };
        let body = json!({"redirect": false, "sites": used_sites, "force_foreign_changes": force});
        // A 412 means the pending changes moved between the read and the POST
        // (e.g. a host rename job activates on its own): read them again once.
        self.base.wait_for_running_activation().await;
        let mut attempt = 0;
        let (result, count) = loop {
            let pending = self.base.client.get(PENDING, None, &[]).await?;
            let count = pending_list(&pending.data).len();
            if count == 0 {
                return Ok(vec![
                    json!({"type": "text", "text": "ℹ️ **No Pending Changes**

There are no configuration changes to activate."}),
                ]);
            }
            // activate-changes is etag="input": the ETag comes from the pending_changes answer above.
            let headers = vec![("If-Match".to_string(), Base::extract_etag(&pending))];
            match self
                .base
                .client
                .post(
                    "domain-types/activation_run/actions/activate-changes/invoke",
                    Some(&body),
                    &headers,
                )
                .await
            {
                Ok(r) => break (r, count),
                Err(e) if attempt == 0 && e.status_code == Some(412) => attempt += 1,
                Err(e) if activation_refusal(&e) == Some(ActivationRefusal::NoChanges) => {
                    return Ok(vec![
                        json!({"type": "text", "text": "ℹ️ **No Pending Changes**\n\nThere are no configuration changes to activate."}),
                    ]);
                }
                Err(e) if activation_refusal(&e) == Some(ActivationRefusal::ForeignChanges) => {
                    return Ok(self.base.error_response(
                        "Activation refused: other users have pending changes",
                        Some("CheckMK activates every pending change of a site at once. Review them with vibemk_get_pending_changes; to activate them as well, call vibemk_activate_changes again with force_foreign_changes=true."),
                    ));
                }
                Err(e) => {
                    return Ok(self
                        .base
                        .error_response("Activation failed", Some(&e.to_string())))
                }
            }
        };
        let id = result
            .data
            .get("id")
            .map_or_else(|| "unknown".to_string(), st);
        Ok(vec![json!({"type": "text", "text": format!(
            "✅ **Changes Activation Started Successfully**\n\nActivation ID: {id}\nSites: {}\nForce foreign changes: {}\nChanges to activate: {count}\n\n⏳ Activation is now running...\n💡 Check the CheckMK GUI for progress details.",
            used_sites.join(", "),
            py_repr(&force),
        )})])
    }

    async fn get_pending_changes(&self) -> Result<Vec<Value>, ApiError> {
        let result = self.base.client.get(PENDING, None, &[]).await?;
        let changes = pending_list(&result.data);
        if changes.is_empty() {
            return Ok(self.base.structured_response(
                "ℹ️ **No Pending Changes**\n\nAll configuration changes have been activated.\nThe CheckMK configuration is up to date.",
                json!({"count": 0, "changes": []}),
            ));
        }
        let mut counts = [
            ("create", 0),
            ("edit", 0),
            ("delete", 0),
            ("move", 0),
            ("other", 0),
        ];
        let (mut lines, mut entries) = (Vec::new(), Vec::new());
        for change in changes {
            // CheckMK sends each change flat: id, user_id, action_name, text, time.
            let action = or_default(change, "action_name", "change");
            let description = match change.get("text") {
                Some(t) if !t.is_null() && t.as_str() != Some("") => st(t),
                _ => action.clone(),
            };
            let user = or_default(change, "user_id", "unknown user");
            let lower = action.to_lowercase();
            let (slot, icon) = if lower.contains("create") {
                (0, "➕")
            } else if lower.contains("edit") || lower.contains("update") {
                (1, "📝")
            } else if lower.contains("delete") {
                (2, "🗑️")
            } else if lower.contains("move") {
                (3, "📁")
            } else {
                (4, "⚙️")
            };
            counts[slot].1 += 1;
            lines.push(format!("{icon} {description} (by {user})"));
            entries.push(json!({"change_id": change.get("id").map_or_else(String::new, st), "user": user, "text": description}));
        }
        let summary = counts
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(k, n)| format!("{k}: {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if changes.len() > MAX_LISTED_CHANGES {
            format!(
                "\n\n... and {} more changes",
                changes.len() - MAX_LISTED_CHANGES
            )
        } else {
            String::new()
        };
        Ok(self.base.structured_response(
            &format!(
                "📋 **Pending Changes** ({} total)\n\n**Summary:** {summary}\n\n**Details:**\n{}{more}\n\n💡 Use 'activate_changes' to apply these changes",
                changes.len(),
                lines[..lines.len().min(MAX_LISTED_CHANGES)].join("\n"),
            ),
            json!({"count": changes.len(), "changes": entries}),
        ))
    }
}

#[async_trait]
impl Handler for ConfigurationHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        let result = match tool {
            "vibemk_activate_changes" => self.activate_changes(args).await,
            "vibemk_get_pending_changes" => self.get_pending_changes().await,
            "vibemk_checkmk_rules_guide" => Ok(vec![json!({"type": "text", "text": RULES_GUIDE})]),
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

/// Byte-identical to the guide of configuration.py `_rules_guide`.
pub const RULES_GUIDE: &str = r####"# Checkmk Rule Management Guide

## Rule order (CRITICAL)

Checkmk evaluates rules by these principles:
1. **The first matching rule wins** — rules are searched top to bottom, and the first one that matches is applied.
2. **A subfolder rule ALWAYS beats a root folder rule** — a rule in `/muenchen/mue-0/` is evaluated before one in `/` (root), whatever position it holds.
3. **Within one folder**: position #0 (top) beats position #5.

### What follows from that
- ALWAYS create a new rule for a host in that host's own folder, not in root `~`.
- Before creating a rule, call `vibemk_get_ruleset` and check whether one already exists for this host.
- If one exists, prefer **updating** it (vibemk_update_rule) over adding another — otherwise two rules compete.
- After creating a rule, check the order and move it up with vibemk_move_rule if needed.

## value_raw — a Python literal, not JSON

Checkmk rule values are Python dicts containing tuples. JSON has no tuples, so always pass `value_raw` as a Python string:

```python
# CORRECT (value_raw as a string):
"{'levels': ('perc_used', (80.0, 90.0))}"
"{'levels_swap': ('perc_used', (30.0, 50.0)), 'levels_virtual': ('perc_used', (90.0, 95.0))}"

# WRONG (JSON / rule_config -> API 400):
{"levels": ["perc_used", [80.0, 90.0]]}  # lists instead of tuples -> error
```

## memory_linux parameter keys

For `checkgroup_parameters:memory_linux`:
- `levels_virtual` — total virtual memory (what Checkmk displays as "Total virtual memory")
- `levels_ram` — physical RAM
- `levels_swap` — swap usage
- `levels_committed` — committed memory

**NOT** `levels`: it does not exist in this ruleset, and sending it returns HTTP 400.

## Interface speed (checkgroup_parameters:interfaces)

For "expected speed" WARN messages:
- Ruleset: `checkgroup_parameters:interfaces`, NOT `checkgroup_parameters:if`
- Value: `{'speed': 1000000000}` — bits/s, not Mbit/s
- Typical values: 1 Gbit/s = 1_000_000_000, 10 Gbit/s = 10_000_000_000
- `vibemk_set_interface_params` with `expected_speed_mbit` converts for you.
- Only activate_changes is needed; no service discovery.

## Activation workflow

1. Create or change the rule (vibemk_create_rule / vibemk_update_rule / vibemk_set_*)
2. Activate the changes (vibemk_activate_changes) — the ETag is fetched for you
3. Wait for the next check cycle, normally one minute
4. Check the result (vibemk_get_service_status, or the Checkmk UI)

**INTERNAL:** vibeMK reads the ETag from `pending_changes` itself. There is no manual ETag handling to do.

## Process monitoring (inventory_processes_rules)

After creating a process rule:
1. Activate it (activate_changes)
2. **Run a service discovery** — the new "Process X" service appears only then
3. Without the discovery the rule exists but the service stays invisible

`vibemk_set_process_thresholds` does both the activation and the discovery.

## Common mistakes

| Symptom | Cause | Fix |
|---------|-------|-----|
| HTTP 400 | value_raw contains lists instead of tuples | pass value_raw as a Python literal string |
| The rule has no effect | a root rule beats the subfolder rule | create the rule in the host's folder |
| The rule has no effect (2) | wrong order | vibemk_move_rule + top_of_folder |
| The memory check shows stale values | the check has not run yet | wait one to two minutes after activating |
| The interface WARN persists | wrong ruleset | checkgroup_parameters:interfaces, not :if |
"####;
