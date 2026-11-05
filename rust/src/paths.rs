//! Port of src/vibemk/api/paths.py: safe construction of REST API paths.
//!
//! Values from tool arguments must fill exactly one path segment: raw, a value
//! such as "x/../../objects/user_config/admin" or "x?y" would address another
//! endpoint than the tool means to call.

use std::fmt::Write;

/// Segments a server resolves as path navigation rather than as a name.
const NAVIGATION_SEGMENTS: [&str; 2] = [".", ".."];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnsafePathError(pub String);

/// Python repr of a str, enough for the error messages ('x' quoting).
fn py_repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// Encode a value so it fills exactly one segment (urllib.parse.quote(safe="")).
/// "~" survives, keeping CheckMK folder paths such as "~linux~web" intact;
/// "." and ".." are refused because servers normalise encoded dots too.
pub fn path_segment(value: &str) -> Result<String, UnsafePathError> {
    if value.is_empty() {
        return Err(UnsafePathError(
            "An API path segment must not be empty".into(),
        ));
    }
    if NAVIGATION_SEGMENTS.contains(&value) {
        return Err(UnsafePathError(format!(
            "Not a valid name for an API object: {}",
            py_repr(value)
        )));
    }
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    Ok(out)
}

/// urllib.parse.unquote: decode %XX, leave malformed escapes as they are.
fn unquote(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Refuse an endpoint that would navigate away from where it points, or that
/// smuggles in a query string (query parameters go in the client's params).
pub fn check_endpoint(endpoint: &str) -> Result<(), UnsafePathError> {
    if endpoint.contains('?') || endpoint.contains('#') {
        return Err(UnsafePathError(format!(
            "Query or fragment in an API path: {}",
            py_repr(endpoint)
        )));
    }
    for segment in endpoint.split('/') {
        if NAVIGATION_SEGMENTS.contains(&unquote(segment).as_str()) {
            return Err(UnsafePathError(format!(
                "Path navigation in an API path: {}",
                py_repr(endpoint)
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_encodes_everything_but_unreserved() {
        assert_eq!(path_segment("~linux~web").unwrap(), "~linux~web");
        assert_eq!(path_segment("a/b?c d").unwrap(), "a%2Fb%3Fc%20d");
        assert_eq!(path_segment("é").unwrap(), "%C3%A9");
        assert!(path_segment("").is_err());
        assert_eq!(
            path_segment("..").unwrap_err().0,
            "Not a valid name for an API object: '..'"
        );
    }

    #[test]
    fn endpoint_refuses_navigation_and_query() {
        assert!(check_endpoint("objects/host_config/web1").is_ok());
        assert!(check_endpoint("objects/x/%2E%2E/y").is_err());
        assert!(check_endpoint("objects/x/../y").is_err());
        assert!(check_endpoint("objects/x?y").is_err());
        assert!(check_endpoint("objects/x#y").is_err());
        assert!(check_endpoint("objects/%2").is_ok());
    }
}
