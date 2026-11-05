//! The real binary: --read-only hides and refuses write tools; activate_changes reaches CheckMK.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Run the binary on `args`/`env`, send `input`, return the answers and what CheckMK received.
async fn run(read_only: bool, input: &[Value]) -> (Vec<Value>, Vec<String>) {
    let fake = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&fake)
        .await;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibemk"));
    cmd.env("CHECKMK_SERVER_URL", fake.uri())
        .env("CHECKMK_SITE", "cmk")
        .env("CHECKMK_USERNAME", "automation")
        .env("CHECKMK_PASSWORD", "test-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if read_only {
        cmd.arg("--read-only");
    }
    let mut child = cmd.spawn().expect("binary starts");
    let mut stdin = child.stdin.take().unwrap();
    for message in input {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let answers = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert!(child.wait().unwrap().success());
    let paths = fake
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    (answers, paths)
}

fn catalogue() -> Vec<Value> {
    serde_json::from_str(include_str!("../tools.json")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_lists_only_read_tools_and_refuses_the_others() {
    let (answers, paths) = run(
        true,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"vibemk_delete_host","arguments":{"host_name":"web01"}}}),
        ],
    )
    .await;
    let read_only: Vec<Value> = catalogue()
        .into_iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == json!(true))
        .collect();
    assert_eq!(answers[0]["result"]["tools"], json!(read_only));
    assert!(read_only.len() < 154);
    assert_eq!(answers[1]["result"]["isError"], json!(true));
    assert!(answers[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("runs read-only"));
    assert!(
        paths.is_empty(),
        "a refused write must not reach CheckMK: {paths:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn activate_changes_reaches_the_activation_endpoint() {
    let (_, paths) = run(
        false,
        &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"vibemk_delete_host","arguments":{"host_name":"web01","activate_changes":true}}})],
    )
    .await;
    assert!(
        paths.iter().any(|p| p.contains("activation_run")),
        "no activation request among {paths:?}"
    );
}
