//! Port of src/vibemk/server/dispatch.py: tool call -> MCP CallToolResult.
//! Every outcome, an unknown tool included, is a result with `isError`, never a JSON-RPC error.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jsonschema::error::{TypeKind, ValidationErrorKind};
use jsonschema::Validator;
use serde_json::{json, Value};

use crate::client::CheckMkClient;
use crate::handlers::base::{py_repr, Base, ERROR_BLOCK, STRUCTURED_BLOCK};
use crate::registry::ToolRegistry;

pub const CONFIGURATION_HELP: &str = "Please set the required environment variables:\n- CHECKMK_SERVER_URL\n- CHECKMK_SITE\n- CHECKMK_USERNAME\n- CHECKMK_PASSWORD";

/// dispatch.py `is_error` (l.34): only an `_error` block counts, never the wording.
pub fn is_error(content: &[Value]) -> bool {
    content
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some(ERROR_BLOCK))
}

/// dispatch.py `to_content_blocks` (l.45): the text blocks, markers and data removed.
pub fn to_content_blocks(content: &[Value]) -> Vec<Value> {
    content
        .iter()
        .filter(|b| {
            !matches!(
                b.get("type").and_then(Value::as_str),
                Some(STRUCTURED_BLOCK | ERROR_BLOCK)
            )
        })
        .map(|b| {
            let text = match b.get("text") {
                None => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => py_repr(other),
            };
            json!({"type": "text", "text": text})
        })
        .collect()
}

/// dispatch.py `structured_of` (l.54): the first `_structured` block's data, if not null.
pub fn structured_of(content: &[Value]) -> Option<Value> {
    content
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some(STRUCTURED_BLOCK))
        .and_then(|b| b.get("data"))
        .filter(|d| !d.is_null())
        .cloned()
}

/// Names of the tools tools.json marks `readOnlyHint: true` (the set of annotations.py READ_ONLY).
pub fn read_only_tools() -> HashSet<String> {
    crate::tools::tools()
        .iter()
        .filter(|t| t.pointer("/annotations/readOnlyHint") == Some(&Value::Bool(true)))
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
        .collect()
}

pub struct Dispatcher {
    pub registry: Arc<ToolRegistry>,
    /// None allows every tool; read-only mode passes the read tools only.
    allowed_tools: Option<HashSet<String>>,
    /// Built once: compiling a validator per call would cost more than the check.
    validators: HashMap<String, Validator>,
    /// Tools whose schema offers `activate_changes`.
    activating: HashSet<String>,
    /// Client used to activate pending changes (the handlers sit behind a trait with no client).
    activator: Option<Arc<CheckMkClient>>,
}

impl Dispatcher {
    /// Every tool allowed, arguments validated against rust/tools.json.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        let schemas = crate::tools::tools()
            .iter()
            .filter_map(|t| {
                Some((
                    t.get("name")?.as_str()?.to_string(),
                    t.get("inputSchema")?.clone(),
                ))
            })
            .collect();
        Self {
            registry,
            allowed_tools: None,
            validators: HashMap::new(),
            activating: HashSet::new(),
            activator: None,
        }
        .with_input_schemas(schemas)
    }

    /// Registry over `client`, which also serves `activate_changes`.
    pub fn from_client(client: Arc<CheckMkClient>, read_only: bool) -> Self {
        let d = Self::new(Arc::new(ToolRegistry::from_client(client.clone())))
            .with_activation_client(client);
        if read_only {
            d.read_only()
        } else {
            d
        }
    }

    /// Refuse every tool not annotated readOnlyHint (VIBEMK_READ_ONLY).
    pub fn read_only(self) -> Self {
        self.with_allowed_tools(read_only_tools())
    }

    /// Whether `name` may be listed and called (everything, unless read-only).
    pub fn allows(&self, name: &str) -> bool {
        self.allowed_tools.as_ref().is_none_or(|a| a.contains(name))
    }

    pub fn with_allowed_tools(mut self, tools: HashSet<String>) -> Self {
        self.allowed_tools = Some(tools);
        self
    }

    pub fn with_activation_client(mut self, client: Arc<CheckMkClient>) -> Self {
        self.activator = Some(client);
        self
    }

    /// Replace the schemas arguments are checked against (default: tools.json).
    pub fn with_input_schemas(mut self, schemas: HashMap<String, Value>) -> Self {
        self.activating = schemas
            .iter()
            .filter(|(_, s)| s.pointer("/properties/activate_changes").is_some())
            .map(|(n, _)| n.clone())
            .collect();
        self.validators = schemas
            .iter()
            .filter_map(|(n, s)| Some((n.clone(), jsonschema::draft202012::new(s).ok()?)))
            .collect();
        self
    }

    /// dispatch.py `call_tool` (l.85): the CallToolResult JSON ({content, isError, structuredContent?}).
    pub async fn call_tool(&self, name: &str, arguments: Option<&Value>) -> Value {
        tracing::info!("Tool call: {name}");
        let empty = json!({});
        // Python `arguments or {}`: null and {} both mean no arguments.
        let args = arguments.filter(|a| !a.is_null()).unwrap_or(&empty);

        // Before the registry: a refused write must not even open the CheckMK connection.
        if self
            .allowed_tools
            .as_ref()
            .is_some_and(|a| !a.contains(name))
        {
            tracing::warn!("Refused {name}: the server runs read-only");
            return failure(&format!(
                "❌ **{name} is not available**\n\nThis vibeMK server runs read-only: it only offers tools that read from CheckMK. Ask the operator to restart it without --read-only (VIBEMK_READ_ONLY) to make changes."
            ));
        }

        let problems = self.argument_problems(name, args);
        if !problems.is_empty() {
            let list: Vec<String> = problems.iter().map(|p| format!("- {p}")).collect();
            return failure(&format!(
                "❌ **Invalid arguments for {name}**\n\n{}",
                list.join("\n")
            ));
        }

        let Some(handler) = self.registry.handler_for(name) else {
            return failure(&format!("❌ **Unknown tool: {name}**"));
        };

        // A panic is a tool that ran and failed: a result the model can act on, not a dead server.
        let (tool, owned) = (name.to_string(), args.clone());
        let mut content =
            match tokio::spawn(async move { handler.handle(&tool, &owned).await }).await {
                Ok(c) => c,
                Err(error) => {
                    tracing::error!("Error in tool call {name}: {error}");
                    return failure(&format!("❌ **{name} failed**\n\n{error}"));
                }
            };

        let failed = is_error(&content);
        if !failed
            && self.activating.contains(name)
            && args.get("activate_changes") == Some(&Value::Bool(true))
        {
            content = self.with_activation(content).await;
        }

        let mut result = json!({"content": to_content_blocks(&content), "isError": failed});
        if let Some(s) = structured_of(&content) {
            result["structuredContent"] = s;
        }
        result
    }

    /// dispatch.py `_with_activation` (l.153): the one-line outcome joins the last text block.
    async fn with_activation(&self, mut content: Vec<Value>) -> Vec<Value> {
        let line = match &self.activator {
            Some(client) => Base::new(client.clone()).run_activation().await,
            None => "⚠️ Activation failed: no CheckMK client".to_string(),
        };
        match content
            .iter()
            .rposition(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        {
            None => content.push(json!({"type": "text", "text": line})),
            Some(i) => {
                let old = content[i]
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                content[i]["text"] = Value::String(format!("{old}\n{line}"));
            }
        }
        content
    }

    /// dispatch.py `_argument_problems` (l.167): one line per problem, Python jsonschema wording.
    fn argument_problems(&self, name: &str, args: &Value) -> Vec<String> {
        let Some(validator) = self.validators.get(name) else {
            return Vec::new();
        };
        let mut found: Vec<(Vec<String>, String)> = validator
            .iter_errors(args)
            .map(|e| {
                let path: Vec<String> = e
                    .instance_path
                    .as_str()
                    .split('/')
                    .skip(1)
                    .map(str::to_string)
                    .collect();
                (path, python_message(&e.kind, &e.instance, &e.to_string()))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
            .into_iter()
            .map(|(path, m)| {
                if path.is_empty() {
                    m
                } else {
                    format!("{}: {m}", path.join("."))
                }
            })
            .collect()
    }
}

/// The message Python's jsonschema gives the same error; Rust's own wording otherwise.
fn python_message(kind: &ValidationErrorKind, instance: &Value, fallback: &str) -> String {
    let inst = py_repr(instance);
    let quoted = |s: &String| py_repr(&Value::String(s.clone()));
    match kind {
        ValidationErrorKind::Required { property } => {
            format!("{} is a required property", py_repr(property))
        }
        ValidationErrorKind::Type {
            kind: TypeKind::Single(t),
        } => format!("{inst} is not of type '{t}'"),
        ValidationErrorKind::Type {
            kind: TypeKind::Multiple(_),
        } => match fallback.split_once(" is not of types ") {
            Some((_, types)) => format!("{inst} is not of type {}", types.replace('"', "'")),
            None => fallback.to_string(),
        },
        ValidationErrorKind::Enum { options } => {
            format!("{inst} is not one of {}", py_repr(options))
        }
        ValidationErrorKind::Minimum { limit } => {
            format!("{inst} is less than the minimum of {}", py_repr(limit))
        }
        ValidationErrorKind::Maximum { limit } => {
            format!("{inst} is greater than the maximum of {}", py_repr(limit))
        }
        ValidationErrorKind::ExclusiveMinimum { limit } => format!(
            "{inst} is less than or equal to the minimum of {}",
            py_repr(limit)
        ),
        ValidationErrorKind::ExclusiveMaximum { limit } => format!(
            "{inst} is greater than or equal to the maximum of {}",
            py_repr(limit)
        ),
        ValidationErrorKind::MinLength { limit: 1 }
        | ValidationErrorKind::MinItems { limit: 1 } => format!("{inst} should be non-empty"),
        ValidationErrorKind::MinLength { .. } | ValidationErrorKind::MinItems { .. } => {
            format!("{inst} is too short")
        }
        ValidationErrorKind::MaxLength { .. } | ValidationErrorKind::MaxItems { .. } => {
            format!("{inst} is too long")
        }
        ValidationErrorKind::Pattern { pattern } => {
            format!("{inst} does not match {}", quoted(pattern))
        }
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            let names: Vec<String> = unexpected.iter().map(quoted).collect();
            let verb = if names.len() == 1 { "was" } else { "were" };
            format!(
                "Additional properties are not allowed ({} {verb} unexpected)",
                names.join(", ")
            )
        }
        _ => fallback.to_string(),
    }
}

/// dispatch.py `_failure` (l.179).
fn failure(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}
