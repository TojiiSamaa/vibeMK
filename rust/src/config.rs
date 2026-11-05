//! Port of src/vibemk/config/settings.py (CheckMKConfig). Real impl: C1.

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub server_url: String,
    pub site: String,
    pub username: String,
    pub password: String,
    pub verify_ssl: bool,
    pub timeout: u64,
    pub max_retries: u32,
    pub debug: bool,
    pub never_activate_changes: bool,
}

/// Same wording as the Python ValueError messages ("CHECKMK_SERVER_URL is required").
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// Version reported in the User-Agent (src/vibemk/config/version.py).
pub const MCP_VERSION: &str = "0.6.4";

fn safe_bool(value: Option<String>, default: bool) -> bool {
    match value.as_deref().map(str::to_lowercase).as_deref() {
        Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        _ => default, // unset, empty or invalid: the default, never false
    }
}

fn safe_int(value: Option<String>, default: i64) -> i64 {
    value.and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn required(get: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<String, ConfigError> {
    get(name)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ConfigError(format!("{name} is required")))
}

impl Config {
    /// Load from CHECKMK_* / NEVER_ACTIVATE_CHANGES, as CheckMKConfig.from_env.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    /// `from_env` over any variable lookup (tests pass a map, not the process env).
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let server_url = required(get, "CHECKMK_SERVER_URL")?;
        let site = required(get, "CHECKMK_SITE")?;
        let username = required(get, "CHECKMK_USERNAME")?;
        let password = required(get, "CHECKMK_PASSWORD")?;
        let timeout = safe_int(get("CHECKMK_TIMEOUT"), 30);
        let max_retries = safe_int(get("CHECKMK_MAX_RETRIES"), 3);
        // Negative values are rejected like __post_init__ does; u64/u32 cannot hold them.
        if timeout <= 0 {
            return Err(ConfigError("timeout must be positive".into()));
        }
        if max_retries < 0 {
            return Err(ConfigError("max_retries must be non-negative".into()));
        }
        Self {
            server_url,
            site,
            username,
            password,
            verify_ssl: safe_bool(get("CHECKMK_VERIFY_SSL"), true),
            timeout: timeout as u64,
            max_retries: u32::try_from(max_retries).unwrap_or(u32::MAX),
            debug: safe_bool(get("CHECKMK_DEBUG"), false),
            never_activate_changes: safe_bool(get("NEVER_ACTIVATE_CHANGES"), false),
        }
        .validated()
    }

    /// CheckMKConfig.__post_init__: required fields, positive timeout, normalised URL.
    pub fn validated(mut self) -> Result<Self, ConfigError> {
        for (value, name) in [
            (&self.server_url, "CHECKMK_SERVER_URL"),
            (&self.username, "CHECKMK_USERNAME"),
            (&self.password, "CHECKMK_PASSWORD"),
            (&self.site, "CHECKMK_SITE"),
        ] {
            if value.is_empty() {
                return Err(ConfigError(format!("{name} is required")));
            }
        }
        if self.timeout == 0 {
            return Err(ConfigError("timeout must be positive".into()));
        }
        self.server_url = normalize_url(&self.server_url);
        Ok(self)
    }
}

/// Assume TLS when no scheme is given (credentials travel in a header); an explicit
/// http:// is honoured; trailing slashes are dropped.
fn normalize_url(url: &str) -> String {
    let url = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    if url.len() > 1 {
        url.trim_end_matches('/').to_string()
    } else {
        url
    }
}

// Password never printed, as CheckMKConfig.__repr__.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("server_url", &self.server_url)
            .field("site", &self.site)
            .field("username", &self.username)
            .field("password", &"***")
            .field("verify_ssl", &self.verify_ssl)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("debug", &self.debug)
            .field("never_activate_changes", &self.never_activate_changes)
            .finish()
    }
}
