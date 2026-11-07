# vibemk (Rust)

## Python vs Rust: measured comparison

Same machine (Windows 11), same fake CheckMK, both servers over stdio, release build for Rust.
Script: `scripts/bench.py`. Median of 10 starts, 300 sequential `tools/call`.

| | Python (0.6.4) | Rust (`rust/`) | Rust vs Python |
|---|---|---|---|
| Start until `initialize` answered | 1217 ms | 259 ms | 4.7x faster |
| `tools/list` (154 tools) | 8.9 ms | 4.7 ms | 1.9x faster |
| `tools/call`, median | 4.86 ms | 1.05 ms | 4.6x faster |
| `tools/call`, p99 | 75 ms | 44 ms | 1.7x faster |
| Sequential throughput | 53 calls/s | 251 calls/s | 4.7x |
| Peak memory (process tree) | 75 MB | 54 MB | 28 % less |
| Install footprint | 75.5 MB of dependencies (31 packages) + a Python interpreter | one 11.1 MB binary | |
| Source lines (src) | 17,126 | 15,312 | |
| Tests (offline, 7 Nov 2026) | 603 pytest cases (516 passed, 87 live tests skipped), 7,162 lines | 574 tests passed, 13,281 lines | |
| Live sweep, 154 tools on a real CheckMK 2.3.0p52 / 2.5.0p18 | 270/270 on both | 270/270 on both | |
| Tools offered | 154 | 154 (identical `tools.json`) | |

Limits: a local fake server answers in microseconds, so these numbers show the server's own overhead, not
what you will see against a real CheckMK, where the network and CheckMK dominate. One machine, one run.
Both servers are also run against real CheckMK 2.3.0p52 Raw and 2.5.0p18 Community sites (Docker) by
`scripts/live_sweep.py`, and the Rust integration tests (`rust/tests/integration.rs`, 9/9) pass on both.
See `AUDIT.md`.

Rust port of the vibeMK CheckMK MCP server. Same 154 tools, same schemas
(`rust/tools.json` is generated from the Python reference), same environment
variables and flags. It is a single binary with no Python runtime.

## Build

```
cd rust
cargo build --release
```

The binary is `target/release/vibemk` (`vibemk.exe` on Windows).

## Usage

```
vibemk [--transport stdio|http] [--host HOST] [--port PORT] [--path PATH] [--read-only]
```

| Option | Environment | Default |
|---|---|---|
| `--transport <stdio\|http>` | `VIBEMK_TRANSPORT` | `stdio` |
| `--host <HOST>` | `VIBEMK_HTTP_HOST` | `127.0.0.1` |
| `--port <PORT>` | `VIBEMK_HTTP_PORT` | `8765` |
| `--path <PATH>` | `VIBEMK_HTTP_PATH` | `/mcp` |
| `--read-only` | `VIBEMK_READ_ONLY=1` (also `true`, `yes`, `on`) | off; only read tools are listed, and a call to any other tool is refused before it reaches CheckMK |

**stdio** (default): the client starts the binary and speaks MCP on stdin/stdout.

**HTTP**: `vibemk --transport http` serves MCP on `http://HOST:PORT/PATH`
(POST JSON-RPC). Every request needs `Authorization: Bearer <token>`, where the
token is `VIBEMK_HTTP_TOKEN` (required, at least 16 characters). A missing or
wrong token gets a 401. The server has no TLS: when `--host` is not a loopback
address it warns, and TLS should be put in front of it.

## Environment variables

Connection (required unless noted):

| Variable | Meaning | Default |
|---|---|---|
| `CHECKMK_SERVER_URL` | CheckMK server URL | required |
| `CHECKMK_SITE` | site name | required |
| `CHECKMK_USERNAME` | automation user | required |
| `CHECKMK_PASSWORD` | its secret | required |
| `CHECKMK_VERIFY_SSL` | verify TLS certificates | `true` |
| `CHECKMK_TIMEOUT` | request timeout, seconds | `30` |
| `CHECKMK_MAX_RETRIES` | retries after a 5xx | `3` |
| `CHECKMK_DEBUG` | debug output | `false` |
| `NEVER_ACTIVATE_CHANGES` | never activate changes automatically | `false` |
| `VIBEMK_HTTP_TOKEN` | bearer token, HTTP transport only | required for http |

The Python `LOGFILE` variable is not read by the Rust port.

## MCP client configuration

```json
{
  "mcpServers": {
    "vibemk": {
      "command": "/path/to/vibemk",
      "env": {
        "CHECKMK_SERVER_URL": "https://checkmk.example.com",
        "CHECKMK_SITE": "monitoring",
        "CHECKMK_USERNAME": "automation",
        "CHECKMK_PASSWORD": "secret"
      }
    }
  }
}
```

`--read-only` is enforced twice, as in the Python server: write tools are not listed, and a call that names one anyway is refused.

## Parity with the Python server

The tool list, input schemas and annotations come from the Python reference;
the handlers are ports of `src/vibemk/handlers`. `AUDIT.md` lists the audit
findings; the port carries the corrected behaviour, each with a test in
`rust/tests/` (`audit.rs`, `client.rs`, `hosts.rs`, `connection.rs`,
`user_roles.rs`, `configuration.rs`, `paths.rs`). Deliberate differences from
the original Python behaviour:

- S1: `Authorization` is never sent on a redirect to another host.
- S3: a non-ASCII bearer token is a 401, not a 500.
- S4: object paths are built with `path_segment`; `paths.rs` fails on raw interpolation.
- E1: response header lookup ignores case.
- E2, E11, N1: null `data`/`extensions` bodies no longer crash the write report.
- E3: a retry keeps `follow_redirects` off.
- E4: `Retry-After` is capped.
- E6: a 412/428 on `update_host` gives the concurrency advice.
- E8: the `*` ETag fallback is logged as a warning.
- E9a: an activation is reported as started, not finished.
- E10a: `time_ago` of a future date is 0.
- S2: an automatic activation never activates other users' changes, and says so when they block it.
- E9c: the 422 "no changes to activate" is an info; the 401/403 for foreign changes is explained.
- E5: a DELETE retried after a 5xx that gets 404 counts as deleted.
- E10b: downtime times are shown in local time.

No audit finding is left open. Details in `AUDIT.md`.

## Regenerating `tools.json`

After any change to `src/vibemk/server/tools.py`, from the repository root:

```
python scripts/export_tools.py rust/tools.json
```

CI regenerates it and fails if `git diff rust/tools.json` is not empty.

## Tests

```
cd rust
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```
