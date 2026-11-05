//! Port of tests/test_api_paths.py.

mod common;

use vibemk::paths::{check_endpoint, path_segment};

#[test]
fn value_stays_in_one_segment() {
    for (value, expected) in [
        ("myhost", "myhost"),
        ("web-01.example.com", "web-01.example.com"),
        ("~linux~web", "~linux~web"),
        (
            "checkgroup_parameters:cpu_load",
            "checkgroup_parameters%3Acpu_load",
        ),
        (
            "x/../../objects/user_config/admin",
            "x%2F..%2F..%2Fobjects%2Fuser_config%2Fadmin",
        ),
        (
            "x?effective_attributes=true",
            "x%3Feffective_attributes%3Dtrue",
        ),
        ("x#frag", "x%23frag"),
        ("my host", "my%20host"),
        ("42", "42"),
    ] {
        assert_eq!(path_segment(value).unwrap(), expected, "{value}");
    }
}

#[test]
fn navigation_and_empty_values_are_refused() {
    for value in ["", ".", ".."] {
        assert!(path_segment(value).is_err(), "{value:?}");
    }
}

#[test]
fn ordinary_endpoints_pass() {
    for endpoint in [
        "version",
        "objects/host_config/myhost",
        "objects/folder_config/~linux~web/collections/hosts",
        "objects/host_config/x%2F..%2Fy",
    ] {
        assert!(check_endpoint(endpoint).is_ok(), "{endpoint}");
    }
}

#[test]
fn navigation_query_and_fragment_are_refused() {
    for endpoint in [
        "objects/host_config/../user_config/admin",
        "objects/host_config/%2e%2e/user_config/admin",
        "objects/host_config/./x",
        "objects/host_config/x?y=1",
        "objects/host_config/x#y",
    ] {
        assert!(check_endpoint(endpoint).is_err(), "{endpoint}");
    }
}

#[tokio::test]
async fn client_refuses_before_sending() {
    let (server, client) = common::fake_checkmk().await;
    let err = client
        .get("objects/host_config/../user_config/admin", None, &[])
        .await
        .unwrap_err();
    assert_eq!(err.kind, vibemk::client::ApiErrorKind::Validation);
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing may reach the server"
    );
}

/// The k-th top-level argument after the format string, if any.
fn nth_arg(after_literal: &str, k: usize) -> Option<String> {
    let (mut depth, mut args, mut current) = (0i32, Vec::new(), String::new());
    for c in after_literal.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                args.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    args.push(current);
    // args[0] is what sits between the closing quote and the first comma.
    args.get(k + 1).map(|a| a.trim().to_string())
}

fn sanitised(s: &str) -> bool {
    s.contains("path_segment(") || s.contains("seg(")
}

/// `field` is bound to a path_segment/seg result in `window` (the lines ending at the format!).
fn field_bound(field: &str, window: &[&str]) -> bool {
    window.iter().enumerate().any(|(i, l)| {
        let prev = if i > 0 { window[i - 1] } else { "" };
        // `let f = path_segment(..)`, `let Ok(f) = path_segment(..)`
        ((l.contains(&format!("let {field} =")) || l.contains(&format!("let Ok({field}) =")))
            && sanitised(l))
            // `match path_segment(..) { Ok(f) =>` and `path_segment(..).map(|f| ..)`
            || ((l.contains(&format!("Ok({field}) =>")) || l.contains(&format!(".map(|{field}|")))
                && (sanitised(l) || sanitised(prev)))
    })
}

/// Structural guard: a handler endpoint that interpolates a raw value fails here.
#[test]
fn no_raw_interpolation_in_object_paths() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/handlers");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (number, line) in lines.iter().copied().enumerate() {
            // rustfmt may put the format! arguments on the next lines.
            let after = lines[number..(number + 4).min(lines.len())].join(
                "
",
            );
            let window = &lines[number.saturating_sub(4)..=number];
            let mut offset = 0;
            let mut rest = line;
            while let Some(at) = rest.find("\"objects/") {
                let literal = &rest[at + 1..];
                let end = literal.find('"').unwrap_or(literal.len());
                let after_literal = &after[offset + at + 1 + end + 1..];
                let mut fields = &literal[..end];
                let mut positional = 0;
                while let Some(open) = fields.find('{') {
                    let close = fields[open..].find('}').map_or(fields.len(), |c| open + c);
                    let field = &fields[open + 1..close];
                    let ok = if field.is_empty() {
                        positional += 1;
                        nth_arg(after_literal, positional - 1).is_some_and(|a| sanitised(&a))
                    } else {
                        // `kind` is a hard-coded collection name (groups.rs endpoint), never user input.
                        field_bound(field, window) || field == "kind" || field == "encoded_service"
                    };
                    if !ok {
                        offenders.push(format!(
                            "{}:{}: {{{field}}}",
                            path.file_name().unwrap().to_string_lossy(),
                            number + 1
                        ));
                    }
                    fields = &fields[(close + 1).min(fields.len())..];
                }
                offset += at + 1 + end;
                rest = &literal[end..];
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "Wrap path values in path_segment():
{}",
        offenders.join(
            "
"
        )
    );
}
