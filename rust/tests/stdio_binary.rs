//! Launches the real binary over stdio and talks MCP to it.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread")]
async fn the_binary_speaks_mcp_over_stdio() {
    let fake = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(json!({"site":"cmk","versions":{"checkmk":"2.3.0"}})),
        )
        .mount(&fake)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_vibemk"))
        .env("CHECKMK_SERVER_URL", fake.uri())
        .env("CHECKMK_SITE", "cmk")
        .env("CHECKMK_USERNAME", "automation")
        .env("CHECKMK_PASSWORD", "test-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("binary starts");

    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"vibemk_get_checkmk_version","arguments":{}}}),
    ];
    let mut stdin = child.stdin.take().unwrap();
    for message in &input {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin); // EOF ends the server

    let answers: Vec<Value> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).expect("stdout carries only JSON-RPC"))
        .collect();
    assert!(child.wait().unwrap().success());

    // 5 messages, 1 notification: 4 answers, in order.
    let ids: Vec<Value> = answers.iter().map(|a| a["id"].clone()).collect();
    assert_eq!(ids, [json!(1), json!(2), json!(3), json!(4)]);
    assert_eq!(answers[0]["result"]["serverInfo"]["name"], "vibemk");

    assert_eq!(answers[1]["result"]["tools"].as_array().unwrap().len(), 154);
    let expected: Value = serde_json::from_str(include_str!("../tools.json")).unwrap();
    assert_eq!(answers[1]["result"]["tools"], expected);

    assert_eq!(answers[2]["result"], json!({}));
    assert!(answers[3]["result"]["content"].is_array());
}
