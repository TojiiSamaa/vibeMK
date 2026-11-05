//! Port of tests/test_config.py and the URL-scheme tests of tests/test_client_transport.py.

use std::collections::HashMap;

use vibemk::config::Config;

fn load(pairs: &[(&str, &str)]) -> Result<Config, String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(&|name| map.get(name).cloned()).map_err(|e| e.to_string())
}

const BASE: [(&str, &str); 4] = [
    ("CHECKMK_SERVER_URL", "http://localhost:8080"),
    ("CHECKMK_SITE", "cmk"),
    ("CHECKMK_USERNAME", "automation"),
    ("CHECKMK_PASSWORD", "password"),
];

/// BASE with `extra` added or overriding.
fn with(extra: &[(&'static str, &'static str)]) -> Result<Config, String> {
    let mut pairs = BASE.to_vec();
    for (k, v) in extra {
        pairs.retain(|(name, _)| name != k);
        pairs.push((k, v));
    }
    load(&pairs)
}

fn direct(url: &str) -> Config {
    Config {
        server_url: url.into(),
        site: "cmk".into(),
        username: "automation".into(),
        password: "secret".into(),
        verify_ssl: true,
        timeout: 30,
        max_retries: 3,
        debug: false,
        never_activate_changes: false,
    }
    .validated()
    .expect("valid")
}

#[test]
fn from_env_complete() {
    let c = load(&[
        ("CHECKMK_SERVER_URL", "https://checkmk.example.com"),
        ("CHECKMK_SITE", "production"),
        ("CHECKMK_USERNAME", "automation"),
        ("CHECKMK_PASSWORD", "secret123"),
        ("CHECKMK_VERIFY_SSL", "true"),
        ("CHECKMK_TIMEOUT", "45"),
        ("CHECKMK_MAX_RETRIES", "5"),
    ])
    .unwrap();
    assert_eq!(c.server_url, "https://checkmk.example.com");
    assert_eq!(
        (c.site.as_str(), c.username.as_str(), c.password.as_str()),
        ("production", "automation", "secret123")
    );
    assert!(c.verify_ssl);
    assert_eq!((c.timeout, c.max_retries), (45, 5));
}

#[test]
fn from_env_minimal_uses_defaults() {
    let c = load(&BASE).unwrap();
    assert_eq!(c.server_url, "http://localhost:8080");
    assert!(c.verify_ssl);
    assert_eq!((c.timeout, c.max_retries), (30, 3));
    assert!(!c.debug && !c.never_activate_changes);
}

#[test]
fn missing_required_fields_name_the_variable() {
    assert_eq!(load(&[]).unwrap_err(), "CHECKMK_SERVER_URL is required");
    for var in ["CHECKMK_SITE", "CHECKMK_USERNAME", "CHECKMK_PASSWORD"] {
        let msg = format!("{var} is required");
        let pairs: Vec<_> = BASE.iter().filter(|(k, _)| *k != var).copied().collect();
        assert_eq!(load(&pairs).unwrap_err(), msg);
        // an empty value is as missing as an absent one
        let mut empty = pairs.clone();
        empty.push((var, ""));
        assert_eq!(load(&empty).unwrap_err(), msg);
    }
}

#[test]
fn invalid_boolean_falls_back_to_default() {
    assert!(with(&[("CHECKMK_VERIFY_SSL", "maybe")]).unwrap().verify_ssl);
    assert!(!with(&[("CHECKMK_DEBUG", "perhaps")]).unwrap().debug);
}

#[test]
fn boolean_spellings() {
    for (text, expected) in [
        ("TRUE", true),
        ("1", true),
        ("yes", true),
        ("On", true),
        ("false", false),
        ("0", false),
        ("NO", false),
        ("off", false),
    ] {
        assert_eq!(
            with(&[("CHECKMK_VERIFY_SSL", text)]).unwrap().verify_ssl,
            expected,
            "{text}"
        );
    }
    assert!(
        with(&[("CHECKMK_VERIFY_SSL", "")]).unwrap().verify_ssl,
        "empty keeps the default (true)"
    );
}

#[test]
fn invalid_integer_falls_back_to_default() {
    assert_eq!(
        with(&[("CHECKMK_TIMEOUT", "not_a_number")])
            .unwrap()
            .timeout,
        30
    );
    assert_eq!(
        with(&[("CHECKMK_MAX_RETRIES", "1.5")]).unwrap().max_retries,
        3
    );
}

#[test]
fn debug_and_never_activate_changes_are_read() {
    let c = with(&[("CHECKMK_DEBUG", "1"), ("NEVER_ACTIVATE_CHANGES", "yes")]).unwrap();
    assert!(c.debug && c.never_activate_changes);
}

#[test]
fn url_normalization() {
    for (input, expected) in [
        ("http://checkmk.local", "http://checkmk.local"),
        ("http://checkmk.local/", "http://checkmk.local"),
        ("https://checkmk.local:8080/", "https://checkmk.local:8080"),
        ("checkmk.local", "https://checkmk.local"),
    ] {
        assert_eq!(
            with(&[("CHECKMK_SERVER_URL", input)]).unwrap().server_url,
            expected,
            "{input}"
        );
    }
}

#[test]
fn numeric_validation() {
    assert_eq!(
        with(&[("CHECKMK_TIMEOUT", "-1")]).unwrap_err(),
        "timeout must be positive"
    );
    assert_eq!(
        with(&[("CHECKMK_TIMEOUT", "0")]).unwrap_err(),
        "timeout must be positive"
    );
    assert_eq!(
        with(&[("CHECKMK_MAX_RETRIES", "-1")]).unwrap_err(),
        "max_retries must be non-negative"
    );
    assert_eq!(
        with(&[("CHECKMK_MAX_RETRIES", "0")]).unwrap().max_retries,
        0
    );
    let zero = Config {
        timeout: 0,
        ..direct("http://test")
    };
    assert_eq!(
        zero.validated().unwrap_err().to_string(),
        "timeout must be positive"
    );
}

#[test]
fn validated_checks_required_fields_in_post_init_order() {
    let blank = Config {
        server_url: String::new(),
        username: String::new(),
        ..direct("http://test")
    };
    assert_eq!(
        blank.validated().unwrap_err().to_string(),
        "CHECKMK_SERVER_URL is required"
    );
    let no_user = Config {
        username: String::new(),
        site: String::new(),
        ..direct("http://test")
    };
    assert_eq!(
        no_user.validated().unwrap_err().to_string(),
        "CHECKMK_USERNAME is required"
    );
    let no_site = Config {
        site: String::new(),
        ..direct("http://test")
    };
    assert_eq!(
        no_site.validated().unwrap_err().to_string(),
        "CHECKMK_SITE is required"
    );
}

#[test]
fn password_is_redacted_in_debug_output() {
    let shown = format!("{:?}", direct("http://test"));
    assert!(!shown.contains("secret"));
    assert!(shown.contains("***"));
}

#[test]
fn development_environment() {
    let c = with(&[
        ("CHECKMK_PASSWORD", "cmk"),
        ("CHECKMK_VERIFY_SSL", "false"),
        ("CHECKMK_TIMEOUT", "10"),
    ])
    .unwrap();
    assert_eq!(c.server_url, "http://localhost:8080");
    assert!(!c.verify_ssl);
    assert_eq!(c.timeout, 10);
}

#[test]
fn production_environment() {
    let c = with(&[
        ("CHECKMK_SERVER_URL", "https://checkmk.company.com"),
        ("CHECKMK_SITE", "production"),
        ("CHECKMK_USERNAME", "vibemk_automation"),
        ("CHECKMK_PASSWORD", "complex_secure_password_123"),
        ("CHECKMK_VERIFY_SSL", "true"),
        ("CHECKMK_TIMEOUT", "60"),
        ("CHECKMK_MAX_RETRIES", "5"),
    ])
    .unwrap();
    assert_eq!(c.server_url, "https://checkmk.company.com");
    assert!(c.verify_ssl);
    assert_eq!((c.timeout, c.max_retries), (60, 5));
}

#[test]
fn docker_environment() {
    let c = with(&[
        ("CHECKMK_SERVER_URL", "http://checkmk-container:5000"),
        ("CHECKMK_VERIFY_SSL", "false"),
    ])
    .unwrap();
    assert!(c.server_url.contains("checkmk-container"));
    assert!(!c.verify_ssl);
}

#[test]
fn scheme_default_bare_host_is_https() {
    assert_eq!(
        direct("checkmk.example.com").server_url,
        "https://checkmk.example.com"
    );
    assert_eq!(
        direct("checkmk.example.com:8080").server_url,
        "https://checkmk.example.com:8080"
    );
    assert_eq!(
        direct("checkmk.example.com/").server_url,
        "https://checkmk.example.com"
    );
}

#[test]
fn scheme_default_explicit_scheme_is_respected() {
    assert_eq!(
        direct("http://localhost:8080").server_url,
        "http://localhost:8080"
    );
    assert_eq!(
        direct("https://checkmk.example.com").server_url,
        "https://checkmk.example.com"
    );
}

/// The one test that reads the real process environment (all the others use a map).
#[test]
fn from_env_reads_the_process_environment() {
    std::env::remove_var("CHECKMK_SERVER_URL");
    assert_eq!(
        Config::from_env().unwrap_err().to_string(),
        "CHECKMK_SERVER_URL is required"
    );
}
