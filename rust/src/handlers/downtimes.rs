//! Port of src/vibemk/handlers/downtimes.py (DowntimeHandler).
//!
//! Failures of the client are `Err` here (Python raised `CheckMKError`), so the Python
//! "success is false" branches cannot happen and are not ported. The time parsers take the
//! zone as a parameter (`Local` in production) so tests can fix it without a global.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike,
    Utc,
};
use serde_json::{json, Map, Value};

use crate::client::{ApiError, CheckMkClient};
use crate::handlers::base::{py_repr, Base, Handler};

/// Failure of one tool call: a CheckMK error or anything else ("Unexpected Error").
enum Fail {
    Api(ApiError),
    Other(String),
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Fail::Api(e)
    }
}

type R<T> = Result<T, Fail>;

fn other<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail::Other(msg.into()))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `str()`.
fn py_str(v: &Value) -> String {
    v.as_str().map_or_else(|| py_repr(v), str::to_string)
}

/// `str(d.get(key, default))`.
fn get_str(v: &Value, key: &str, default: &str) -> String {
    v.get(key).map_or_else(|| default.to_string(), py_str)
}

/// `d.get(key)` that is truthy, else None.
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| truthy(v))
}

/// A time argument: None when missing or falsy, an error when truthy but not a string.
fn time_arg<'a>(args: &'a Value, key: &str) -> R<Option<&'a str>> {
    match arg(args, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => other(format!("'{key}' must be a string")),
    }
}

fn py_int(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

fn int_literal_error(s: &str) -> String {
    format!(
        "invalid literal for int() with base 10: {}",
        py_repr(&Value::String(s.to_string()))
    )
}

// ---------------------------------------------------------------- time helpers

enum Iso {
    Aware(DateTime<FixedOffset>),
    Naive(NaiveDateTime),
}

/// `datetime.fromisoformat` for the shapes CheckMK and users send.
fn from_iso(s: &str) -> Option<Iso> {
    for f in [
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M%z",
        "%Y-%m-%d %H:%M%z",
    ] {
        if let Ok(d) = DateTime::parse_from_str(s, f) {
            return Some(Iso::Aware(d));
        }
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(Iso::Naive(d));
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(Iso::Naive)
}

/// A naive wall-clock time on the zone's clock (`astimezone()`); a DST gap falls back to UTC.
fn aware<Tz: TimeZone>(tz: &Tz, n: NaiveDateTime) -> DateTime<FixedOffset> {
    tz.from_local_datetime(&n)
        .earliest()
        .map_or_else(|| n.and_utc().fixed_offset(), |d| d.fixed_offset())
}

fn ts_f64<T: TimeZone>(d: &DateTime<T>) -> f64 {
    d.timestamp() as f64 + f64::from(d.timestamp_subsec_nanos()) / 1e9
}

/// A downtime's ISO 8601 time (or a number) as a Unix timestamp; 0.0 when unreadable.
pub fn unix_time(v: &Value) -> f64 {
    match v {
        Value::String(s) => match from_iso(&s.replace('Z', "+00:00")) {
            Some(Iso::Aware(d)) => ts_f64(&d),
            Some(Iso::Naive(n)) => ts_f64(&aware(&Local, n)),
            None => 0.0,
        },
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Whether a downtime is in effect right now (CheckMK 2.5 has no is_pending field).
fn is_active(extensions: &Value, now: f64) -> bool {
    let at = |k: &str| unix_time(extensions.get(k).unwrap_or(&Value::Null));
    at("start_time") <= now && now <= at("end_time")
}

fn now_ts() -> f64 {
    ts_f64(&Utc::now())
}

fn is_service_flag(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(s)) => s == "1" || s == "yes",
        _ => false,
    }
}

/// One entry of the DOWNTIME_LIST output schema.
fn downtime_record(downtime: &Value, now: f64) -> Value {
    let ext = downtime
        .get("extensions")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let is_service = is_service_flag(ext.get("is_service"));
    json!({
        "downtime_id": get_str(downtime, "id", ""),
        "host_name": get_str(&ext, "host_name", ""),
        "service_description": if is_service { ext.get("service_description").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "is_service": is_service,
        "start_time": get_str(&ext, "start_time", ""),
        "end_time": get_str(&ext, "end_time", ""),
        "active": is_active(&ext, now),
        "comment": get_str(&ext, "comment", ""),
        "author": get_str(&ext, "author", ""),
        "recurring": ext.get("recurring").is_some_and(truthy),
    })
}

/// `_is_downtime_active`: like `is_active` but a numeric string is a Unix time and an
/// unreadable one means not active.
pub fn is_downtime_active(downtime: &Value, current: f64) -> bool {
    let ext = downtime.get("extensions").unwrap_or(&Value::Null);
    let read = |k: &str| -> Option<f64> {
        match ext.get(k) {
            None => Some(0.0),
            Some(Value::String(s)) => match from_iso(&s.replace('Z', "+00:00")) {
                Some(Iso::Aware(d)) => Some(ts_f64(&d)),
                Some(Iso::Naive(n)) => Some(ts_f64(&aware(&Local, n))),
                None => s.trim().parse::<f64>().ok(),
            },
            Some(Value::Number(n)) => n.as_f64(),
            Some(_) => None,
        }
    };
    match (read("start_time"), read("end_time")) {
        (Some(s), Some(e)) => s <= current && current <= e,
        _ => false,
    }
}

/// `_format_timestamp`: "YYYY-MM-DD HH:MM" in the value's own offset, local for numbers.
pub fn format_timestamp(v: &Value) -> String {
    stamp(v, "%Y-%m-%d %H:%M").unwrap_or_else(|| "Unknown".to_string())
}

fn stamp(v: &Value, fmt: &str) -> Option<String> {
    if !truthy(v) {
        return None;
    }
    match v {
        Value::String(s) => match from_iso(&s.replace('Z', "+00:00"))? {
            Iso::Aware(d) => Some(d.format(fmt).to_string()),
            Iso::Naive(n) => Some(n.format(fmt).to_string()),
        },
        Value::Number(n) => {
            let t = n.as_f64()?;
            Local
                .timestamp_opt(t.floor() as i64, 0)
                .single()
                .map(|d| d.format(fmt).to_string())
        }
        _ => None,
    }
}

/// `_get_time_only`: "HH:MM" (an unreadable value comes back as its first 5 characters).
fn time_only(v: &Value) -> String {
    let fallback = |s: String| s.chars().take(5).collect::<String>();
    match v {
        Value::String(s) => match from_iso(&s.replace('Z', "+00:00")) {
            Some(Iso::Aware(d)) => d.format("%H:%M").to_string(),
            Some(Iso::Naive(n)) => n.format("%H:%M").to_string(),
            None => fallback(s.clone()),
        },
        Value::Number(n) => n
            .as_f64()
            .and_then(|t| Local.timestamp_opt(t.floor() as i64, 0).single())
            .map_or_else(|| fallback(py_str(v)), |d| d.format("%H:%M").to_string()),
        _ => {
            let s = format_timestamp(v);
            if s.contains(' ') {
                s.rsplit(' ').next().unwrap_or("").to_string()
            } else {
                fallback(s)
            }
        }
    }
}

/// `_parse_time_delta`: "+1h", "30m", "1h30m", "2d4h", bare minutes; 60 when unreadable.
pub fn parse_time_delta(delta: &str) -> Result<i64, String> {
    let s = delta.trim_matches('+');
    let lower = s.to_lowercase();
    let c: Vec<char> = lower.chars().collect();
    let (mut total, mut found, mut i) = (0i64, false, 0usize);
    while i < c.len() {
        let mut j = i;
        while j < c.len() && c[j].is_ascii_digit() {
            j += 1;
        }
        if j > i && j < c.len() && matches!(c[j], 'd' | 'h' | 'm') {
            let n: i64 = c[i..j]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| "Python int too large".to_string())?;
            let mult = match c[j] {
                'd' => 1440,
                'h' => 60,
                _ => 1,
            };
            total = n
                .checked_mul(mult)
                .and_then(|v| total.checked_add(v))
                .ok_or("Python int too large")?;
            found = true;
            i = j + 1;
        } else {
            i += 1;
        }
    }
    if found {
        return Ok(total);
    }
    for (suffix, mult) in [('m', 1), ('h', 60), ('d', 1440)] {
        if let Some(head) = s.strip_suffix(suffix) {
            return py_int(head)
                .and_then(|n| n.checked_mul(mult))
                .ok_or_else(|| int_literal_error(head));
        }
    }
    Ok(py_int(s).unwrap_or(60))
}

// Hand-rolled equivalents of the Python regexes (the regex crate is not a dependency).

fn ws_plus(c: &[char], mut i: usize) -> Option<usize> {
    let start = i;
    while i < c.len() && c[i].is_whitespace() {
        i += 1;
    }
    (i > start).then_some(i)
}

fn lit(c: &[char], i: usize, text: &str) -> Option<usize> {
    let t: Vec<char> = text.chars().collect();
    (c.len() >= i + t.len() && c[i..i + t.len()] == t[..]).then_some(i + t.len())
}

fn digits(c: &[char], i: usize, min: usize, max: usize) -> Vec<usize> {
    // Possible end positions of `\d{min,max}`, greediest first.
    let mut n = 0;
    while n < max && i + n < c.len() && c[i + n].is_ascii_digit() {
        n += 1;
    }
    (min..=n).rev().map(|k| i + k).collect()
}

fn num(c: &[char], a: usize, b: usize) -> u32 {
    c[a..b]
        .iter()
        .collect::<String>()
        .parse()
        .unwrap_or(u32::MAX)
}

/// `(\d{1,2}):(\d{2})` at i -> (hour, minute, end).
fn hhmm(c: &[char], i: usize) -> Option<(u32, u32, usize)> {
    digits(c, i, 1, 2).into_iter().find_map(|e| {
        let k = lit(c, e, ":")?;
        let m = digits(c, k, 2, 2).first().copied()?;
        Some((num(c, i, e), num(c, k, m), m))
    })
}

/// Leftmost `(?:<prefix>\s+at\s+|at\s+)?(\d{1,2}):(\d{2})` for the given prefix word.
fn find_hhmm(c: &[char]) -> Option<(u32, u32)> {
    (0..c.len()).find_map(|i| hhmm(c, i).map(|(h, m, _)| (h, m)))
}

/// `at` the clock time `h:m:00` on `dt`'s date and offset.
fn at_time(dt: &DateTime<FixedOffset>, h: u32, m: u32) -> DateTime<FixedOffset> {
    let n = dt
        .date_naive()
        .and_hms_opt(h, m, 0)
        .unwrap_or_else(|| dt.naive_local());
    dt.offset().from_local_datetime(&n).single().unwrap_or(*dt)
}

fn add(dt: DateTime<FixedOffset>, d: Duration) -> Result<DateTime<FixedOffset>, String> {
    dt.checked_add_signed(d)
        .ok_or_else(|| "date value out of range".to_string())
}

fn minutes(n: i64) -> Result<Duration, String> {
    Duration::try_minutes(n).ok_or_else(|| "Python int too large to convert to C int".to_string())
}

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// `_parse_natural_time`: "22:00", "tomorrow at 14:00", "2026-03-01 at 22:00", "in 2 hours",
/// "next monday at 09:00". `now` is read on `tz`'s clock; the result keeps that offset.
pub fn parse_natural_time<Tz: TimeZone>(
    tz: &Tz,
    now_utc: DateTime<Utc>,
    text: &str,
) -> Result<Option<DateTime<FixedOffset>>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    let lower = text.trim().to_lowercase();
    let c: Vec<char> = lower.chars().collect();
    let now = now_utc.with_timezone(tz).fixed_offset();
    let one_day = Duration::days(1);

    // Pattern 1: "HH:MM", "HH:MM today", "today at HH:MM"
    if let Some((h, m)) = find_hhmm(&c) {
        if (lower.contains("today")
            || !(lower.contains("tomorrow") || WEEKDAYS.iter().any(|d| lower.contains(d))))
            && h <= 23
            && m <= 59
        {
            let mut t = at_time(&now, h, m);
            if t <= now {
                t = add(t, one_day)?;
            }
            return Ok(Some(t));
        }
    }

    // Pattern 2: "HH:MM tomorrow", "tomorrow at HH:MM", "tomorrow"
    if lower.contains("tomorrow") {
        let found = (0..=c.len()).find_map(|i| {
            let with_prefix = [
                lit(&c, i, "tomorrow")
                    .and_then(|e| ws_plus(&c, e))
                    .and_then(|e| lit(&c, e, "at"))
                    .and_then(|e| ws_plus(&c, e)),
                lit(&c, i, "at").and_then(|e| ws_plus(&c, e)),
                Some(i),
            ];
            if let Some(g) = with_prefix.into_iter().flatten().find_map(|p| hhmm(&c, p)) {
                return Some(Some((g.0, g.1)));
            }
            let e = lit(&c, i, "tomorrow")?;
            Some(
                ws_plus(&c, e)
                    .and_then(|e| lit(&c, e, "at"))
                    .and_then(|e| ws_plus(&c, e))
                    .and_then(|e| hhmm(&c, e))
                    .map(|g| (g.0, g.1)),
            )
        });
        if let Some(g) = found {
            let (h, m) = g.unwrap_or((now.hour(), now.minute()));
            if h <= 23 && m <= 59 {
                return Ok(Some(at_time(&add(now, one_day)?, h, m)));
            }
        }
    }

    // Pattern 3: "YYYY-MM-DD" with optional " at HH:MM"
    for i in 0..c.len() {
        let Some(y_end) = digits(&c, i, 4, 4).first().copied() else {
            continue;
        };
        let Some(k) = lit(&c, y_end, "-") else {
            continue;
        };
        for m_end in digits(&c, k, 1, 2) {
            let Some(k2) = lit(&c, m_end, "-") else {
                continue;
            };
            let Some(d_end) = digits(&c, k2, 1, 2).first().copied() else {
                continue;
            };
            let time = ws_plus(&c, d_end)
                .and_then(|e| lit(&c, e, "at"))
                .and_then(|e| ws_plus(&c, e))
                .and_then(|e| hhmm(&c, e));
            let (h, mi) = time.map_or((now.hour(), now.minute()), |t| (t.0, t.1));
            let (y, mo, d) = (num(&c, i, y_end), num(&c, k, m_end), num(&c, k2, d_end));
            let naive = i32::try_from(y)
                .ok()
                .filter(|y| *y >= 1)
                .and_then(|y| NaiveDate::from_ymd_opt(y, mo, d))
                .and_then(|dt| dt.and_hms_opt(h, mi, 0));
            if let Some(n) = naive {
                return Ok(Some(aware(tz, n)));
            }
            // Invalid date: fall through to the next patterns.
            return parse_after_date(&c, &lower, now);
        }
    }
    parse_after_date(&c, &lower, now)
}

/// Patterns 4 (relative) and 5 (weekday names).
fn parse_after_date(
    c: &[char],
    lower: &str,
    now: DateTime<FixedOffset>,
) -> Result<Option<DateTime<FixedOffset>>, String> {
    // Pattern 4: "in X hours/minutes"
    for i in 0..c.len() {
        let Some(e) = lit(c, i, "in").and_then(|e| ws_plus(c, e)) else {
            continue;
        };
        let n_end = digits(c, e, 1, usize::MAX)
            .first()
            .copied()
            .filter(|n| *n > e);
        let Some(n_end) = n_end else { continue };
        let Some(u) = ws_plus(c, n_end) else { continue };
        let amount: i64 = c[e..n_end]
            .iter()
            .collect::<String>()
            .parse()
            .map_err(|_| "Python int too large to convert to C int".to_string())?;
        for unit in ["hour", "hours", "minute", "minutes", "min"] {
            if lit(c, u, unit).is_some() {
                let d = if unit.contains("hour") {
                    Duration::try_hours(amount)
                } else {
                    Duration::try_minutes(amount)
                };
                return add(now, d.ok_or("date value out of range")?).map(Some);
            }
        }
    }

    // Pattern 5: "monday at 09:00", "next tuesday at 14:30"
    for (i, day) in WEEKDAYS.iter().enumerate() {
        let name: Vec<char> = day.chars().collect();
        let Some(pos) = (0..c.len()).find(|p| c[*p..].starts_with(&name)) else {
            continue;
        };
        let mut days_ahead = i as i64 - i64::from(now.weekday().num_days_from_monday());
        if days_ahead <= 0 || lower.contains("next") {
            days_ahead += 7;
        }
        let target = add(now, Duration::days(days_ahead))?;
        let time = ws_plus(c, pos + name.len())
            .and_then(|e| lit(c, e, "at"))
            .and_then(|e| ws_plus(c, e))
            .and_then(|e| hhmm(c, e));
        return Ok(Some(match time {
            Some((h, m, _)) if h <= 23 && m <= 59 => at_time(&target, h, m),
            _ => target,
        }));
    }
    Ok(None)
}

/// An ISO string (the fallback after the natural-language parser), naive read on `tz`'s clock.
fn parse_iso_fallback<Tz: TimeZone>(tz: &Tz, text: &str) -> Option<DateTime<FixedOffset>> {
    let s = if text.ends_with('Z') {
        text.replace('Z', "+00:00")
    } else {
        text.to_string()
    };
    from_iso(&s).map(|i| match i {
        Iso::Aware(d) => d,
        Iso::Naive(n) => aware(tz, n),
    })
}

/// `_parse_downtime_times`: start and end as `%Y-%m-%dT%H:%M:%SZ`, everything converted to UTC.
pub fn parse_downtime_times<Tz: TimeZone>(
    tz: &Tz,
    now_utc: DateTime<Utc>,
    start: Option<&str>,
    end: Option<&str>,
    duration_minutes: i64,
) -> Result<(String, String), String> {
    let span = minutes(if duration_minutes == 0 {
        30
    } else {
        duration_minutes
    })?;
    let default_start = now_utc.fixed_offset();

    let start_dt = match start.filter(|s| !s.is_empty()) {
        None | Some("now") => default_start,
        Some(s) if s.starts_with('+') => add(default_start, minutes(parse_time_delta(s)?)?)?,
        Some(s) => match parse_natural_time(tz, now_utc, s)? {
            Some(d) => d,
            None => parse_iso_fallback(tz, s).unwrap_or_else(|| {
                tracing::warn!("Could not parse start_time '{s}', using default");
                default_start
            }),
        },
    };

    let mut end_dt = match end.filter(|s| !s.is_empty()) {
        None => add(start_dt, span)?,
        Some(s) if s.starts_with('+') => add(start_dt, minutes(parse_time_delta(s)?)?)?,
        Some(s) => match parse_natural_time(tz, now_utc, s)? {
            Some(d) => d,
            None => parse_iso_fallback(tz, s).unwrap_or_else(|| {
                tracing::warn!("Could not parse end_time '{s}', using start + duration");
                add(start_dt, span).unwrap_or(start_dt)
            }),
        },
    };
    if end_dt <= start_dt {
        tracing::warn!("End time is before start time, adjusting");
        end_dt = add(start_dt, span)?;
    }
    let fmt = |d: DateTime<FixedOffset>| {
        d.with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    };
    Ok((fmt(start_dt), fmt(end_dt)))
}

/// `json.dumps` (", " and ": " separators, non-ASCII escaped): the query text Python sent.
fn py_json_dumps(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let mut out = String::from("\"");
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{8}' => out.push_str("\\b"),
                    '\u{c}' => out.push_str("\\f"),
                    c if (c as u32) < 0x20 || (c as u32) > 0x7e && (c as u32) != 0x7f => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{u:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(py_json_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!(
                    "{}: {}",
                    py_json_dumps(&Value::String(k.clone())),
                    py_json_dumps(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- handler

pub struct DowntimeHandler {
    base: Base,
    /// Pause between two verification attempts (5 s in Python); shortened by tests.
    verify_delay: StdDuration,
}

impl DowntimeHandler {
    pub fn new(client: Arc<CheckMkClient>) -> Self {
        Self {
            base: Base::new(client),
            verify_delay: StdDuration::from_secs(5),
        }
    }

    pub fn with_verify_delay(mut self, delay: StdDuration) -> Self {
        self.verify_delay = delay;
        self
    }

    fn text(text: String) -> Vec<Value> {
        vec![json!({"type": "text", "text": text})]
    }

    async fn dispatch(&self, tool: &str, args: &Value) -> R<Vec<Value>> {
        match tool {
            "vibemk_schedule_downtime" => self.schedule_downtime(args).await,
            "vibemk_schedule_host_downtime" => self.schedule_host_downtime(args).await,
            "vibemk_schedule_service_downtime" => self.schedule_service_downtime(args).await,
            "vibemk_list_downtimes" => self.list_downtimes(args).await,
            "vibemk_delete_downtime" => self.delete_downtime(args).await,
            "vibemk_get_active_downtimes" => self.get_active_downtimes(args).await,
            "vibemk_check_host_downtime_status" => self.check_host_downtime_status(args).await,
            "vibemk_modify_downtime" => self.modify_downtime(args).await,
            _ => Ok(self.base.error_response(
                "Unknown tool",
                Some(&format!("Tool '{tool}' is not supported")),
            )),
        }
    }

    /// The generic tool used to post to a GET-only path; it now dispatches to host or service.
    async fn schedule_downtime(&self, args: &Value) -> R<Vec<Value>> {
        match args.get("downtime_type").and_then(Value::as_str) {
            Some("host") => self.schedule_host_downtime(args).await,
            Some("service") => self.schedule_service_downtime(args).await,
            _ => {
                let shown = args
                    .get("downtime_type")
                    .map_or_else(|| "None".to_string(), py_str);
                Ok(self.base.error_response(
                    "Unsupported downtime_type",
                    Some(&format!(
                        "'{shown}' cannot be scheduled here. Use 'host' or 'service'."
                    )),
                ))
            }
        }
    }

    /// Minutes from `duration`: a string goes through the grammar, a number is truncated.
    fn duration_minutes(args: &Value) -> R<i64> {
        match args.get("duration") {
            None => Ok(60),
            Some(Value::String(s)) => parse_time_delta(s).or_else(other),
            Some(v) if !truthy(v) => Ok(60),
            Some(Value::Bool(_)) => Ok(1),
            Some(Value::Number(n)) => Ok(n
                .as_i64()
                .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64)),
            Some(v) => other(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                if v.is_array() { "list" } else { "dict" }
            )),
        }
    }

    fn times(args: &Value, duration_minutes: i64) -> R<(String, String)> {
        let start = time_arg(args, "start_time")?;
        let end = time_arg(args, "end_time")?;
        parse_downtime_times(&Local, Utc::now(), start, end, duration_minutes).or_else(other)
    }

    async fn schedule_host_downtime(&self, args: &Value) -> R<Vec<Value>> {
        let Some(host) = arg(args, "host_name").map(py_str) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_name is required")));
        };
        let comment = args
            .get("comment")
            .map_or_else(|| "Scheduled maintenance".to_string(), py_str);
        let duration = Self::duration_minutes(args)?;
        let (start, end) = Self::times(args, duration)?;

        let existing = self.current_downtimes(&host, &[], &comment).await;
        if !existing.is_empty() && !args.get("force").is_some_and(truthy) {
            tracing::info!("Host {host} already has downtime with comment '{comment}'");
            return Ok(Self::text(format!(
                "⚠️ **Downtime Already Exists**\n\n**Host:** {host}\n**Existing Comment:** {comment}\n**Status:** Not creating duplicate downtime\n\n💡 **Tip:** Use `force=true` parameter to create anyway, or use different comment"
            )));
        }

        let mut body = json!({"downtime_type": "host", "host_name": host, "start_time": start, "end_time": end, "comment": comment});
        if let Some(recur) = arg(args, "recur") {
            body["recur"] = recur.clone();
        }
        let result = self
            .base
            .client
            .post("domain-types/downtime/collections/host", Some(&body), &[])
            .await?;
        let id = result
            .data
            .get("id")
            .map_or_else(|| "Unknown".to_string(), py_str);
        let verified = self.verify_downtime_creation(&host, &comment, 5, &[]).await;

        let mut r = format!("✅ **Host Downtime Scheduled Successfully**\n\n**Host:** {host}\n");
        if id != "Unknown" {
            r.push_str(&format!("**Downtime ID:** {id}\n"));
        }
        r.push_str(&format!("**Start:** {start}\n**End:** {end}\n**Duration:** {duration} minutes\n**Comment:** {comment}\n"));
        r.push_str(&format!(
            "**Verification:** {}\n",
            if verified {
                "✅ Confirmed"
            } else {
                "⚠️ Pending (may take a moment)"
            }
        ));
        r.push_str("\n💡 **Tip:** Use `vibemk_list_downtimes` to view all active downtimes");
        Ok(Self::text(r))
    }

    async fn schedule_service_downtime(&self, args: &Value) -> R<Vec<Value>> {
        let host = arg(args, "host_name").map(py_str);
        let mut services: Vec<String> = match args.get("service_descriptions") {
            Some(Value::Array(a)) => a.iter().map(py_str).collect(),
            Some(v) if truthy(v) => vec![py_str(v)],
            _ => vec![],
        };
        if services.is_empty() {
            if let Some(s) = arg(args, "service_description") {
                services = vec![py_str(s)];
            }
        }
        let Some(host) = host else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_name is required")));
        };
        if services.is_empty() {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("service_description or service_descriptions is required"),
            ));
        }
        let comment = args
            .get("comment")
            .map_or_else(|| "Scheduled service maintenance".to_string(), py_str);
        let duration = Self::duration_minutes(args)?;
        let (start, end) = Self::times(args, duration)?;

        let existing = self.current_downtimes(&host, &services, &comment).await;
        let to_schedule: Vec<String> = if args.get("force").is_some_and(truthy) {
            services.clone()
        } else {
            let fresh: Vec<String> = services
                .iter()
                .filter(|s| !existing.contains(s))
                .cloned()
                .collect();
            if fresh.is_empty() {
                return Ok(Self::text(format!(
                    "⚠️ **All Services Already Have Downtime**\n\n**Host:** {host}\n**Services:** {}\n**Existing Comment:** {comment}\n**Status:** Not creating duplicate downtimes\n\n💡 **Tip:** Use `force=true` parameter to create anyway, or use different comment",
                    services.join(", ")
                )));
            }
            fresh
        };

        let mut body = json!({"downtime_type": "service", "host_name": host, "service_descriptions": to_schedule, "start_time": start, "end_time": end, "comment": comment});
        if let Some(recur) = arg(args, "recur") {
            body["recur"] = recur.clone();
        }
        let result = self
            .base
            .client
            .post(
                "domain-types/downtime/collections/service",
                Some(&body),
                &[],
            )
            .await?;
        let id = result
            .data
            .get("id")
            .map_or_else(|| "Unknown".to_string(), py_str);
        let verified = self
            .verify_downtime_creation(&host, &comment, 5, &to_schedule)
            .await;

        let mut r = format!("✅ **Service Downtime Scheduled Successfully**\n\n**Host:** {host}\n**Services:** {}\n", to_schedule.join(", "));
        if id != "Unknown" {
            r.push_str(&format!("**Downtime ID:** {id}\n"));
        }
        r.push_str(&format!("**Start:** {start}\n**End:** {end}\n**Duration:** {duration} minutes\n**Comment:** {comment}\n"));
        r.push_str(&format!(
            "**Verification:** {}\n",
            if verified {
                "✅ Confirmed"
            } else {
                "⚠️ Pending (may take a moment)"
            }
        ));
        if to_schedule.len() != services.len() {
            let skipped: Vec<&String> = services
                .iter()
                .filter(|s| !to_schedule.contains(s))
                .collect();
            r.push_str(&format!(
                "**Skipped (existing):** {}\n",
                skipped
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        r.push_str("\n💡 **Tip:** Use `vibemk_list_downtimes` to view all active downtimes");
        Ok(Self::text(r))
    }

    async fn all_downtimes(&self, params: Option<&Value>) -> Result<Vec<Value>, ApiError> {
        let r = self
            .base
            .client
            .get("domain-types/downtime/collections/all", params, &[])
            .await?;
        Ok(r.data
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_downtimes(&self, args: &Value) -> R<Vec<Value>> {
        let host = arg(args, "host_name");
        let service = arg(args, "service_description");
        let only_active = args.get("active_only").is_none_or(truthy);
        let all = self.all_downtimes(None).await?;
        let now = now_ts();
        let null = Value::Null;
        let filtered: Vec<Value> = all
            .into_iter()
            .filter(|d| {
                let ext = d.get("extensions").unwrap_or(&null);
                host.is_none_or(|h| ext.get("host_name") == Some(h))
                    && service.is_none_or(|s| ext.get("service_description") == Some(s))
                    && (!only_active || is_active(ext, now))
            })
            .collect();
        let records: Vec<Value> = filtered.iter().map(|d| downtime_record(d, now)).collect();
        let text = Self::format_downtimes_list(&filtered, host.map(py_str).as_deref());
        Ok(self.base.structured_response(
            &text,
            json!({"total": filtered.len(), "downtimes": records}),
        ))
    }

    async fn delete_downtime(&self, args: &Value) -> R<Vec<Value>> {
        let Some(id) = arg(args, "downtime_id") else {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("downtime_id is required (get it from list_downtimes)"),
            ));
        };
        let body = json!({"delete_type": "by_id", "downtime_id": py_str(id), "site_id": self.base.client.config.site});
        self.base
            .client
            .post(
                "domain-types/downtime/actions/delete/invoke",
                Some(&body),
                &[],
            )
            .await?;
        Ok(Self::text(format!(
            "✅ **Downtime Deleted**\n\n**Downtime ID:** {}\n**Status:** Removed from monitoring schedule\n\n💡 **Tip:** Use `vibemk_list_downtimes` to view remaining active downtimes",
            py_str(id)
        )))
    }

    async fn get_active_downtimes(&self, args: &Value) -> R<Vec<Value>> {
        let host = arg(args, "host_name");
        let all = self.all_downtimes(None).await?;
        let now = now_ts();
        let null = Value::Null;
        let active: Vec<Value> = all
            .into_iter()
            .filter(|d| {
                let ext = d.get("extensions").unwrap_or(&null);
                is_active(ext, now) && host.is_none_or(|h| ext.get("host_name") == Some(h))
            })
            .collect();
        let records: Vec<Value> = active.iter().map(|d| downtime_record(d, now)).collect();
        let text = Self::format_active_downtimes(&active, host.map(py_str).as_deref());
        Ok(self
            .base
            .structured_response(&text, json!({"total": active.len(), "downtimes": records})))
    }

    fn format_downtimes_list(downtimes: &[Value], host_filter: Option<&str>) -> String {
        let filter = host_filter.map_or(String::new(), |h| format!(" for host '{h}'"));
        if downtimes.is_empty() {
            return format!("📋 **No Downtimes Found**\n\nNo active downtimes{filter}");
        }
        let mut r = format!("📋 **Downtimes List{filter}**\n\n");
        let (mut hosts, mut services) = (Vec::new(), Vec::new());
        for d in downtimes.iter().take(30) {
            let is_service = d
                .get("extensions")
                .is_some_and(|e| is_service_flag(e.get("is_service")));
            if is_service {
                services.push(d)
            } else {
                hosts.push(d)
            }
        }
        let plural = |n: usize| if n != 1 { "s" } else { "" };
        r.push_str(&format!(
            "Found {} downtimes: **{} Host-Level** + **{} Service-Level**\n\n",
            hosts.len() + services.len(),
            hosts.len(),
            services.len()
        ));
        let null = Value::Null;
        let line = |d: &Value, indent: &str| -> String {
            let ext = d.get("extensions").unwrap_or(&null);
            let t = |k: &str| time_only(&ext.get(k).cloned().unwrap_or_else(|| json!(0)));
            format!(
                "{indent}• Downtime #{}: {} - {}\n",
                get_str(d, "id", "Unknown"),
                t("start_time"),
                t("end_time")
            )
        };
        let comment = |d: &Value| {
            get_str(
                d.get("extensions").unwrap_or(&null),
                "comment",
                "No comment",
            )
        };

        if !hosts.is_empty() {
            r.push_str("🏠 **HOST DOWNTIMES** (suppress ALL alerts for these hosts)\n");
            r.push_str(&format!("{}\n\n", "=".repeat(60)));
            let mut grouped: std::collections::BTreeMap<String, Vec<&Value>> = Default::default();
            for d in &hosts {
                grouped
                    .entry(get_str(
                        d.get("extensions").unwrap_or(&null),
                        "host_name",
                        "Unknown",
                    ))
                    .or_default()
                    .push(d);
            }
            for (host, list) in grouped {
                r.push_str(&format!(
                    "**{host}** ({} host downtime{})\n",
                    list.len(),
                    plural(list.len())
                ));
                for d in list {
                    r.push_str(&line(d, "  "));
                    r.push_str(&format!("    Comment: \"{}\"\n", comment(d)));
                    r.push_str(
                        "    **Effect**: Host DOWN/UNREACHABLE + ALL service alerts suppressed\n\n",
                    );
                }
            }
        }
        if !services.is_empty() {
            r.push_str("🔧 **SERVICE DOWNTIMES** (suppress only specific service alerts)\n");
            r.push_str(&format!("{}\n\n", "=".repeat(60)));
            let mut grouped: std::collections::BTreeMap<
                String,
                std::collections::BTreeMap<String, Vec<&Value>>,
            > = Default::default();
            for d in &services {
                let ext = d.get("extensions").unwrap_or(&null);
                grouped
                    .entry(get_str(ext, "host_name", "Unknown"))
                    .or_default()
                    .entry(get_str(ext, "service_description", "Unknown Service"))
                    .or_default()
                    .push(d);
            }
            for (host, by_service) in grouped {
                r.push_str(&format!(
                    "**{host}** ({} service{} with downtimes)\n",
                    by_service.len(),
                    plural(by_service.len())
                ));
                for (service, list) in by_service {
                    r.push_str(&format!(
                        "  📋 **{service}** ({} downtime{})\n",
                        list.len(),
                        plural(list.len())
                    ));
                    for d in list {
                        r.push_str(&line(d, "    "));
                        r.push_str(&format!("      Comment: \"{}\"\n", comment(d)));
                        r.push_str(&format!("      **Effect**: Only '{service}' alerts suppressed, host alerts STILL FIRE\n\n"));
                    }
                }
                r.push('\n');
            }
        }
        if downtimes.len() > 30 {
            r.push_str(&format!(
                "... and {} more downtimes (showing first 30)\n\n",
                downtimes.len() - 30
            ));
        }
        r.push_str("💡 **Key Distinction:**\n");
        r.push_str("   • **Host Downtimes**: Suppress both host alerts (DOWN/UNREACHABLE) AND all service alerts\n");
        r.push_str("   • **Service Downtimes**: Suppress only the specific service alerts, host alerts continue to fire\n");
        r.push_str(
            "💡 **Tip:** Use `vibemk_delete_downtime` with downtime ID to cancel a downtime",
        );
        r
    }

    fn format_active_downtimes(active: &[Value], host_filter: Option<&str>) -> String {
        let filter = host_filter.map_or(String::new(), |h| format!(" on host '{h}'"));
        if active.is_empty() {
            return format!(
                "🟢 **No Active Downtimes**\n\nNo downtimes are currently active{filter}"
            );
        }
        let mut r = format!(
            "🔴 **Currently Active Downtimes{filter}**\n\nFound {} active downtimes:\n\n",
            active.len()
        );
        let now = now_ts();
        let null = Value::Null;
        for (i, d) in active.iter().enumerate() {
            let ext = d.get("extensions").unwrap_or(&null);
            // Only the number 1 (or true) counts here, unlike the list view.
            let is_service = matches!(ext.get("is_service"), Some(Value::Bool(true)))
                || ext.get("is_service").and_then(Value::as_f64) == Some(1.0);
            let end = match ext.get("end_time") {
                Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                Some(Value::String(s)) => match from_iso(&s.replace('Z', "+00:00")) {
                    Some(Iso::Aware(d)) => ts_f64(&d),
                    Some(Iso::Naive(n)) => ts_f64(&aware(&Local, n)),
                    None => s.trim().parse::<f64>().unwrap_or(now),
                },
                _ => 0.0,
            };
            let remaining = (((end - now) / 60.0) as i64).max(0);
            r.push_str(&format!(
                "**{}. Downtime #{}**\n",
                i + 1,
                get_str(d, "id", "Unknown")
            ));
            r.push_str(&format!(
                "   🏠 Host: {}\n",
                get_str(ext, "host_name", "Unknown")
            ));
            r.push_str(&format!(
                "   🔧 Type: {}\n",
                if is_service { "Service" } else { "Host" }
            ));
            r.push_str(&format!("   ⏰ Remaining: {remaining} minutes\n"));
            r.push_str(&format!(
                "   💬 Comment: {}\n",
                get_str(ext, "comment", "No comment")
            ));
            if let Some(s) = ext
                .get("service_description")
                .filter(|s| is_service && truthy(s))
            {
                r.push_str(&format!("   🔧 Service: {}\n", py_str(s)));
            }
            r.push('\n');
        }
        r.push_str("💡 **Info:** These downtimes are currently suppressing alerts");
        r
    }

    /// Host-level and service-level downtimes of a host that are running now.
    async fn host_status(&self, host: &str) -> Result<Value, String> {
        let query = |is_service: &str| {
            let q = json!({"op": "and", "expr": [
                {"op": "=", "left": "host_name", "right": host},
                {"op": "=", "left": "is_service", "right": is_service},
            ]});
            json!({"query": py_json_dumps(&q)})
        };
        let host_dt = self
            .all_downtimes(Some(&query("0")))
            .await
            .map_err(|e| e.to_string())?;
        let service_dt = self
            .all_downtimes(Some(&query("1")))
            .await
            .map_err(|e| e.to_string())?;
        let now = now_ts();
        let running = |list: &[Value]| {
            list.iter()
                .filter(|d| is_downtime_active(d, now))
                .cloned()
                .collect::<Vec<_>>()
        };
        let (ah, asv) = (running(&host_dt), running(&service_dt));
        Ok(json!({
            "has_host_downtime": !ah.is_empty(), "has_service_downtimes": !asv.is_empty(),
            "host_downtime_count": ah.len(), "service_downtime_count": asv.len(),
            "active_host_downtimes": ah, "active_service_downtimes": asv,
            "total_host_downtimes": host_dt.len(), "total_service_downtimes": service_dt.len(),
        }))
    }

    async fn check_host_downtime_status(&self, args: &Value) -> R<Vec<Value>> {
        let Some(host) = arg(args, "host_name").map(py_str) else {
            return Ok(self
                .base
                .error_response("Missing parameter", Some("host_name is required")));
        };
        let status = match self.host_status(&host).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Error getting host downtime status for {host}: {e}");
                return Ok(self
                    .base
                    .error_response("Error checking downtime status", Some(&e)));
            }
        };
        let flag = |k: &str| status[k].as_bool().unwrap_or(false);
        let mut r = format!("🔍 **Downtime Status for Host: {host}**\n\n");
        if flag("has_host_downtime") {
            r.push_str(&format!("🏠 **Host Object Downtime:** ✅ **YES** - Host is covered by {} active host-level downtime(s)\n", status["host_downtime_count"]));
        } else {
            r.push_str(
                "🏠 **Host Object Downtime:** ❌ **NO** - Host object has no active downtimes\n",
            );
        }
        if flag("has_service_downtimes") {
            r.push_str(&format!("🔧 **Service Downtimes:** ✅ **YES** - {} service(s) on this host have active downtimes\n", status["service_downtime_count"]));
        } else {
            r.push_str("🔧 **Service Downtimes:** ❌ **NO** - No services on this host have active downtimes\n");
        }
        r.push('\n');
        if flag("has_host_downtime") {
            r.push_str("🎯 **CRITICAL DISTINCTION:** This host HAS host-level downtimes.\n");
            r.push_str("   ✅ Host alerts (host DOWN, UNREACHABLE) are SUPPRESSED\n");
            r.push_str("   ✅ All service alerts on this host are also SUPPRESSED\n");
            r.push_str("   📊 Alert Status: HOST and SERVICE alerts both suppressed\n");
        } else if flag("has_service_downtimes") {
            r.push_str("⚠️ **CRITICAL DISTINCTION:** This host has NO host-level downtimes.\n");
            r.push_str("   ❌ Host alerts (host DOWN, UNREACHABLE) will FIRE normally\n");
            r.push_str("   ✅ Only specific service alerts are suppressed by service downtimes\n");
            r.push_str("   📊 Alert Status: HOST alerts ACTIVE, some SERVICE alerts suppressed\n");
            r.push_str("   💡 To suppress host alerts, you need a separate HOST downtime\n");
        } else {
            r.push_str("🔴 **NO DOWNTIMES:** This host has neither host nor service downtimes.\n");
            r.push_str("   ❌ Host alerts (host DOWN, UNREACHABLE) will FIRE normally\n");
            r.push_str("   ❌ All service alerts will FIRE normally\n");
            r.push_str("   📊 Alert Status: ALL alerts are ACTIVE\n");
        }
        r.push('\n');
        let id = |d: &Value| d.get("id").map_or_else(|| "None".to_string(), py_str);
        let ext_of = |d: &Value| d.get("extensions").cloned().unwrap_or(Value::Null);
        let hosts = status["active_host_downtimes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !hosts.is_empty() {
            r.push_str("📋 **Active Host Downtimes:**\n");
            for d in &hosts {
                r.push_str(&format!(
                    "   • Downtime #{}: {}\n",
                    id(d),
                    get_str(&ext_of(d), "comment", "No comment")
                ));
            }
            r.push('\n');
        }
        let services = status["active_service_downtimes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !services.is_empty() {
            r.push_str("📋 **Active Service Downtimes:**\n");
            for d in &services {
                let ext = ext_of(d);
                r.push_str(&format!(
                    "   • Service '{}' (#{}): {}\n",
                    get_str(&ext, "service_description", "Unknown"),
                    id(d),
                    get_str(&ext, "comment", "No comment")
                ));
            }
            r.push('\n');
        }
        r.push_str("💡 **Usage:** This tool helps distinguish between host-level and service-level downtimes\n");
        r.push_str("   for proper alerting analysis and downtime troubleshooting.");
        Ok(Self::text(r))
    }

    /// Whether a host has a running HOST-level downtime (not service downtimes).
    pub async fn has_host_level_downtime(&self, host: &str) -> bool {
        let q = json!({"op": "and", "expr": [
            {"op": "=", "left": "host_name", "right": host},
            {"op": "=", "left": "is_service", "right": "0"},
        ]});
        match self
            .all_downtimes(Some(&json!({"query": py_json_dumps(&q)})))
            .await
        {
            Ok(list) => {
                let now = now_ts();
                list.iter().any(|d| is_downtime_active(d, now))
            }
            Err(e) => {
                tracing::error!("Error checking host-level downtime for {host}: {e}");
                false
            }
        }
    }

    /// Services (or ["HOST"]) that already carry a downtime with this comment; [] when the query fails.
    async fn current_downtimes(
        &self,
        host: &str,
        services: &[String],
        comment: &str,
    ) -> Vec<String> {
        let like = |left: &str, right: &str| {
            format!("{{\"op\": \"~\", \"left\": \"{left}\", \"right\": \"{right}\"}}")
        };
        let mut filters = vec![];
        if services.is_empty() {
            filters.push("{\"op\": \"=\", \"left\": \"is_service\", \"right\": \"0\"}".to_string());
        } else {
            let each: Vec<String> = services
                .iter()
                .map(|s| like("service_description", s))
                .collect();
            filters.push(if each.len() > 1 {
                format!("{{\"op\": \"or\", \"expr\": [{}]}}", each.join(", "))
            } else {
                each[0].clone()
            });
            filters.push("{\"op\": \"=\", \"left\": \"is_service\", \"right\": \"1\"}".to_string());
        }
        filters.push(like("host_name", host));
        if !comment.is_empty() {
            filters.push(like("comment", comment));
        }
        let params =
            json!({"query": format!("{{\"op\": \"and\", \"expr\": [{}]}}", filters.join(", "))});
        match self.all_downtimes(Some(&params)).await {
            Ok(list) if services.is_empty() => {
                if list.is_empty() {
                    vec![]
                } else {
                    vec!["HOST".to_string()]
                }
            }
            Ok(list) => {
                let mut found: Vec<String> = vec![];
                for d in &list {
                    if let Some(s) = d
                        .pointer("/extensions/service_description")
                        .filter(|s| truthy(s))
                        .map(py_str)
                    {
                        if !found.contains(&s) {
                            found.push(s);
                        }
                    }
                }
                found
            }
            Err(e) => {
                tracing::warn!("Error querying existing downtimes: {e}");
                vec![]
            }
        }
    }

    /// Poll until the new downtime shows up (up to `max_retries` times, `verify_delay` apart).
    async fn verify_downtime_creation(
        &self,
        host: &str,
        comment: &str,
        max_retries: u32,
        services: &[String],
    ) -> bool {
        let is_service = !services.is_empty();
        for retry in 0..max_retries {
            let mut filters = vec![format!(
                "{{\"op\": \"=\", \"left\": \"host_name\", \"right\": \"{host}\"}}"
            )];
            if !comment.is_empty() {
                filters.push(format!(
                    "{{\"op\": \"=\", \"left\": \"comment\", \"right\": \"{comment}\"}}"
                ));
            }
            filters.push(format!(
                "{{\"op\": \"=\", \"left\": \"type\", \"right\": \"{}\"}}",
                if is_service { 3 } else { 2 }
            ));
            let mut params = Map::new();
            params.insert("host_name".into(), json!(host));
            params.insert(
                "query".into(),
                json!(format!(
                    "{{\"op\": \"and\", \"expr\": [{}]}}",
                    filters.join(", ")
                )),
            );
            params.insert(
                "downtime_type".into(),
                json!(if is_service { "service" } else { "host" }),
            );
            params.insert("site_id".into(), json!(self.base.client.config.site));
            match self.all_downtimes(Some(&Value::Object(params))).await {
                Ok(list) if !list.is_empty() => return true,
                Ok(_) => {}
                Err(e) => tracing::warn!("Error during verification attempt {}: {e}", retry + 1),
            }
            if retry + 1 < max_retries {
                tokio::time::sleep(self.verify_delay).await;
            }
        }
        tracing::warn!("Downtime verification failed after {max_retries} attempts");
        false
    }

    async fn modify_downtime(&self, args: &Value) -> R<Vec<Value>> {
        let end = args
            .get("end_time")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null);
        let comment = arg(args, "comment");
        let mut data = if let Some(id) = arg(args, "downtime_id") {
            let n = match id {
                Value::String(s) => py_int(s).ok_or_else(|| Fail::Other(int_literal_error(s)))?,
                Value::Number(n) => n
                    .as_i64()
                    .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
                Value::Bool(b) => i64::from(*b),
                _ => {
                    return other(
                        "int() argument must be a string, a bytes-like object or a real number",
                    )
                }
            };
            json!({"modify_type": "params", "downtime_id": n, "end_time": {"modify_type": "set_end_time", "value": end}})
        } else if let Some(host) = arg(args, "host_name") {
            let mut query = json!({"op": "=", "left": "host_name", "right": host});
            if let Some(s) = arg(args, "service_description") {
                query = json!({"op": "and", "expr": [query, {"op": "=", "left": "service_description", "right": s}]});
            }
            json!({"modify_type": "by_query", "query": query, "end_time": {"modify_type": "set_end_time", "value": end}})
        } else {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("downtime_id or host_name required"),
            ));
        };
        if let Some(c) = comment {
            data["comment"] = c.clone();
        }
        if !truthy(&end) {
            return Ok(self.base.error_response(
                "Missing parameter",
                Some("end_time is required (ISO 8601 format)"),
            ));
        }
        self.base
            .client
            .put(
                "domain-types/downtime/actions/modify/invoke",
                Some(&data),
                &[],
            )
            .await?;
        let target = arg(args, "downtime_id").map_or_else(
            || arg(args, "host_name").map_or_else(String::new, py_str),
            |id| format!("downtime #{}", py_str(id)),
        );
        Ok(Self::text(format!(
            "⏰ **Downtime Modified**\n\nTarget: {target}\nNew end: {}",
            py_str(&end)
        )))
    }
}

#[async_trait]
impl Handler for DowntimeHandler {
    async fn handle(&self, tool: &str, args: &Value) -> Vec<Value> {
        match self.dispatch(tool, args).await {
            Ok(blocks) => blocks,
            Err(Fail::Api(e)) => self
                .base
                .error_response("CheckMK API Error", Some(&e.to_string())),
            Err(Fail::Other(m)) => {
                tracing::error!("Error in {tool}: {m}");
                self.base.error_response("Unexpected Error", Some(&m))
            }
        }
    }
}
