//! The tool catalogue, byte-for-byte rust/tools.json (never edit that file).

use std::sync::OnceLock;

use serde_json::Value;

pub static TOOLS_JSON: &str = include_str!("../tools.json");

/// tools.json parsed once; key order preserved (serde_json preserve_order).
pub fn tools() -> &'static [Value] {
    static PARSED: OnceLock<Vec<Value>> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(TOOLS_JSON).expect("rust/tools.json is valid JSON"))
}

/// Tool definition by name.
pub fn tool(name: &str) -> Option<&'static Value> {
    tools()
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
}

/// Every tool name, in file order.
pub fn tool_names() -> Vec<&'static str> {
    tools()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect()
}
