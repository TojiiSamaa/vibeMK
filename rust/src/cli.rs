//! Port of src/vibemk/server/cli.py: arguments, then stdio or HTTP.

use std::ffi::OsString;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use clap::{Parser, ValueEnum};

use crate::client::CheckMkClient;
use crate::config::Config;
use crate::dispatch::Dispatcher;

const DEFAULT_HTTP_PORT: u16 = 8765;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Transport {
    Stdio,
    Http,
}

/// Fields mirror cli.py `parse_arguments` (l.38): same flags, same environment variables.
#[derive(Debug, Parser)]
#[command(name = "vibemk", about = "CheckMK monitoring over MCP")]
pub struct Args {
    /// stdio (default) or http for Streamable HTTP
    #[arg(long, value_enum, env = "VIBEMK_TRANSPORT", default_value = "stdio")]
    pub transport: Transport,
    /// address to bind in http mode
    #[arg(long, env = "VIBEMK_HTTP_HOST", default_value = "127.0.0.1")]
    pub host: String,
    /// port to bind in http mode
    #[arg(long, env = "VIBEMK_HTTP_PORT", default_value_t = DEFAULT_HTTP_PORT)]
    pub port: u16,
    /// URL path to serve in http mode
    #[arg(long, env = "VIBEMK_HTTP_PATH", default_value = "/mcp")]
    pub path: String,
    /// offer only the tools that read (env: VIBEMK_READ_ONLY=1)
    #[arg(long)]
    pub read_only: bool,
}

fn env_truthy(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn placeholder_config() -> Config {
    Config {
        server_url: String::new(),
        site: String::new(),
        username: String::new(),
        password: String::new(),
        verify_ssl: true,
        timeout: 30,
        max_retries: 0,
        debug: false,
        never_activate_changes: false,
    }
}

/// Parse `argv` and run the server (cli.py `main`, l.76).
pub async fn run<I: IntoIterator<Item = OsString>>(argv: I) -> anyhow::Result<()> {
    let args = Args::parse_from(argv);
    let read_only = args.read_only || env_truthy("VIBEMK_READ_ONLY");
    // Validate the token before anything starts: a weak one is a configuration mistake, not a crash.
    let token = if args.transport == Transport::Http {
        match crate::http::read_token() {
            Ok(token) => Some(token),
            Err(message) => {
                eprintln!("vibemk: {message}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    // tools/list must answer before CheckMK is configured: a bad config is reported on stderr.
    let config = Config::from_env().unwrap_or_else(|error| {
        eprintln!("vibemk: {error}");
        placeholder_config()
    });
    let dispatcher = Dispatcher::from_client(Arc::new(CheckMkClient::new(config)), read_only);

    match token {
        Some(token) => {
            if !matches!(args.host.as_str(), "127.0.0.1" | "localhost" | "::1") {
                eprintln!("vibemk: warning: listening on {}, reachable beyond this machine; put TLS in front of it", args.host);
            }
            let addr: SocketAddr = (args.host.as_str(), args.port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| anyhow::anyhow!("cannot resolve {}", args.host))?;
            crate::http::serve(addr, &args.path, Arc::new(dispatcher), token).await
        }
        None => crate::mcp::serve_stdio(&dispatcher).await,
    }
}
