//! MCP over stdio: one JSON-RPC 2.0 message per line; stdout is reserved for the protocol.
//! Hand-written (style of laya-tojii/mcp-rust): the surface is initialize, ping, tools/list, tools/call.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::dispatch::Dispatcher;

pub const SERVER_NAME: &str = "vibemk";
/// src/vibemk/config/version.py
pub const SERVER_VERSION: &str = "0.6.4";
/// Versions of the Python `mcp` SDK; an unknown request gets the latest (last).
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Answer one JSON-RPC line; None for notifications.
pub async fn handle_line(dispatcher: &Dispatcher, line: &str) -> Option<String> {
    let reply = match serde_json::from_str::<Value>(line) {
        Ok(request) => handle_request(dispatcher, &request).await,
        Err(_) => Some(error(Value::Null, -32700, "Parse error")),
    };
    reply.map(|v| v.to_string())
}

/// Answer one parsed JSON-RPC request; None when it is a notification (no `id`).
pub async fn handle_request(dispatcher: &Dispatcher, request: &Value) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    };
    let id = object.get("id")?.clone(); // no id = notification: never answered
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(error(id, -32600, "Invalid Request"));
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .unwrap_or(&PROTOCOL_VERSIONS[3]);
            json!({
                "protocolVersion": version,
                "capabilities": {"experimental": {}, "tools": {"listChanged": false}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let tools: Vec<&Value> = crate::tools::tools()
                .iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| dispatcher.allows(n))
                })
                .collect();
            json!({ "tools": tools })
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, -32602, "Invalid params: name is required"));
            };
            if crate::tools::tool(name).is_none() {
                return Some(error(id, -32602, &format!("Unknown tool: {name}")));
            }
            let arguments = params.get("arguments").filter(|a| !a.is_null());
            dispatcher.call_tool(name, arguments).await
        }
        _ => return Some(error(id, -32601, &format!("Method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Read stdin line by line until EOF, write answers to stdout.
pub async fn serve_stdio(dispatcher: &Dispatcher) -> anyhow::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = handle_line(dispatcher, &line).await {
            stdout.write_all(answer.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
