//! Deterministic JSON argument repair for malformed tool-call inputs.
//!
//! DeepSeek streams `tool_calls.function.arguments` as deltas. Two failure
//! shapes are common: (a) SSE chunk boundary cuts inside a JSON string and
//! reassembly leaves a trailing comma or unclosed brace; (b) some local
//! backends emit literal control characters inside JSON string values.
//!
//! The repair ladder runs five stages before falling back to an empty object:
//!
//!  1. Strict parse — done if it parses.
//!  2. Strip literal control chars inside string values.
//!  3. Strip trailing commas before `}` or `]`.
//!  4. Balance braces/brackets (append closers).
//!  5. Strip excess closers if delta is negative.
//!  6. Fallback: empty object `{}`.

use regex::Regex;
use serde_json::{Map, Value};

/// Maximum raw argument length we'll attempt to repair (1 MiB).
const MAX_ARG_LEN: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ArgRepairError {
    #[error("argument exceeded {0} chars; refusing to repair")]
    TooLarge(usize),
}

/// Repair a raw JSON argument string into a valid `serde_json::Value`.
///
/// Runs the deterministic ladder; on success returns the parsed value.
/// The final fallback is an empty object `{}` so dispatch always proceeds.
pub fn repair(raw: &str) -> Result<Value, ArgRepairError> {
    if raw.len() > MAX_ARG_LEN {
        return Err(ArgRepairError::TooLarge(raw.len()));
    }
    // Stage 1: strict parse
    if let Ok(v) = serde_json::from_str(raw) {
        return Ok(v);
    }
    // Stage 2: strip control chars inside strings
    let mut s = strip_control_chars_in_strings(raw);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 3: strip trailing commas
    s = strip_trailing_commas(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 4: balance braces
    s = balance_braces(&s, 50);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Stage 5: strip excess closers
    s = strip_excess_closers(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(v);
    }
    // Fallback: empty object
    Ok(Value::Object(Map::new()))
}

/// Post-parse sanitisation of a tool input `Value`.
///
/// Applies semantic repairs that operate on the parsed JSON tree rather than
/// the raw text:
///
///  1. **Null stripping** — remove keys whose value is `null` from objects.
///     DeepSeek often sends `"fuzz": null` instead of omitting the field;
///     downstream `required_str()` / `optional_*` helpers treat absence and
///     `null` identically, so stripping is safe and prevents spurious
///     `MissingField` errors when `as_str()` returns `None` on a `null`.
///
///  2. **Markdown autolink unwrapping** — DeepSeek's chat post-training
///     distribution leaks through the tool boundary, sometimes emitting
///     file paths as `[notes.md](http://notes.md)`. We unwrap the
///     degenerate case where the link text equals the URL-without-protocol;
///     real markdown links like `[click](https://example.com)` pass through.
pub fn sanitize_parsed_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            for v in map.values_mut() {
                sanitize_parsed_value(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_parsed_value(v);
            }
        }
        Value::String(s) => {
            if let Some(unwrapped) = unwrap_markdown_autolink(s) {
                *s = unwrapped;
            }
        }
        _ => {}
    }
}

/// Unwrap degenerate markdown autolinks where link text matches URL-without-protocol.
///
/// `[notes.md](http://notes.md)` → `notes.md`
/// `[src/lib.rs](https://src/lib.rs)` → `src/lib.rs`
///
/// Real markdown links like `[click](https://example.com)` are left untouched.
fn unwrap_markdown_autolink(s: &str) -> Option<String> {
    let re = Regex::new(r"^\[([^\]]+)\]\((https?://)([^)]+)\)$").ok()?;
    let caps = re.captures(s)?;
    let text = caps.get(1)?.as_str();
    let url_path = caps.get(3)?.as_str();
    // Only unwrap when link text matches the URL path (the degenerate case).
    // Normalise spaces that some models inject (e.g. "notes. md" → "notes.md").
    let normalised_url: String = url_path.chars().filter(|c| *c != ' ').collect();
    if text == normalised_url || text == url_path {
        Some(text.to_string())
    } else {
        None
    }
}

/// Try to unwrap a stringified JSON value.
///
/// When a model emits `"[\"a\",\"b\"]"` (a JSON string containing a serialised
/// array) instead of `["a","b"]` (an actual array), this function parses the
/// inner string and returns the unwrapped value.
///
/// **Only call this for fields where the schema expects array or object** — for
/// free-form string fields (like `write_file` content) this would silently
/// corrupt legitimate JSON text.
///
/// Returns `None` if the value is not a string, or if the string does not parse
/// as a JSON array or object.
#[allow(dead_code)]
pub fn try_unwrap_stringified_json(value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let trimmed = s.trim();
    if !(trimmed.starts_with('[') || trimmed.starts_with('{')) {
        return None;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v @ Value::Array(_)) | Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

/// Strip ASCII control characters (0x00–0x1F except \t, \n, \r) that appear
/// inside JSON string values. We walk character-by-character tracking whether
/// we're inside a string (between unescaped double-quotes).
fn strip_control_chars_in_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            out.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if in_string && (ch as u32) < 0x20 && ch != '\t' && ch != '\n' && ch != '\r' {
            // Drop control characters inside strings
            continue;
        }
        out.push(ch);
    }
    out
}

/// Strip trailing commas before `}` or `]`.
fn strip_trailing_commas(s: &str) -> String {
    // Repeatedly replace ",}" and ",]" until stable (handles nested cases).
    let mut out = s.to_string();
    loop {
        let prev = out.clone();
        out = out.replace(",}", "}").replace(",]", "]");
        // Handle trailing comma at end of string
        out = out.trim_end_matches(',').to_string();
        if out == prev {
            break;
        }
    }
    out
}

/// Balance braces and brackets: count `{`/`}` and `[`/`]`, append closers if
/// positive delta (more opens than closes). Caps iterations so a
/// catastrophically broken input doesn't loop forever.
fn balance_braces(s: &str, max_iter: usize) -> String {
    let mut out = s.to_string();
    for _ in 0..max_iter {
        let brace_delta: i32 = out
            .chars()
            .map(|ch| match ch {
                '{' => 1,
                '}' => -1,
                _ => 0,
            })
            .sum();
        let bracket_delta: i32 = out
            .chars()
            .map(|ch| match ch {
                '[' => 1,
                ']' => -1,
                _ => 0,
            })
            .sum();
        if brace_delta <= 0 && bracket_delta <= 0 {
            break;
        }
        // Append needed closers in reverse order (brackets before braces
        // for correct nesting when both are unbalanced).
        for _ in 0..bracket_delta.max(0) {
            out.push(']');
        }
        for _ in 0..brace_delta.max(0) {
            out.push('}');
        }
    }
    out
}

/// Strip excess closers when the delta is negative (more closes than opens).
fn strip_excess_closers(s: &str) -> String {
    let mut brace_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                    out.push(ch);
                }
                // else drop excess closer
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    out.push(ch);
                }
            }
            '{' => {
                brace_depth += 1;
                out.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_parse_passes_through() {
        let v = repair(r#"{"path": "hello.txt"}"#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_trailing_comma() {
        let v = repair(r#"{"path": "hello.txt",}"#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_trailing_comma_in_array() {
        let v = repair(r#"["a", "b",]"#).unwrap();
        assert_eq!(v, json!(["a", "b"]));
    }

    #[test]
    fn repairs_missing_close_brace() {
        let v = repair(r#"{"path": "hello.txt""#).unwrap();
        assert_eq!(v, json!({"path": "hello.txt"}));
    }

    #[test]
    fn repairs_missing_close_bracket() {
        let v = repair(r#"["a", "b""#).unwrap();
        assert_eq!(v, json!(["a", "b"]));
    }

    #[test]
    fn strips_embedded_control_chars() {
        // Raw \x0B (vertical tab) inside a string value
        let raw = "{\"key\": \"val\x0Bue\"}";
        let v = repair(raw).unwrap();
        assert_eq!(v, json!({"key": "value"}));
    }

    #[test]
    fn handles_empty_string() {
        let v = repair("").unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn handles_gibberish() {
        let v = repair("not json at all").unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn balances_nested_braces() {
        let v = repair(r#"{"outer": {"inner": "val""#).unwrap();
        assert_eq!(v, json!({"outer": {"inner": "val"}}));
    }

    #[test]
    fn strips_excess_closers() {
        let v = repair(r#"{"key": "val"}}"#).unwrap();
        assert_eq!(v, json!({"key": "val"}));
    }

    #[test]
    fn handles_double_encoded_json() {
        // This is a valid JSON string containing a JSON object literal.
        // repair parses it as a string; the engine's existing fallback
        // (parse_tool_input) will unwrap the string and re-parse.
        let v = repair(r#""{\"path\": \"hello.txt\"}""#).unwrap();
        assert_eq!(v, Value::String(r#"{"path": "hello.txt"}"#.to_string()));
    }

    #[test]
    fn oversize_input_rejected() {
        let big = "x".repeat(MAX_ARG_LEN + 1);
        assert!(repair(&big).is_err());
    }

    #[test]
    fn repairs_brace_balance_with_trailing_comma() {
        let v = repair(r#"{"a": 1,"#).unwrap();
        assert_eq!(v, json!({"a": 1}));
    }

    // --- sanitize_parsed_value tests ---

    #[test]
    fn sanitize_strips_null_values() {
        let mut v = json!({"path": "/foo", "fuzz": null, "search": "x"});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"path": "/foo", "search": "x"}));
    }

    #[test]
    fn sanitize_strips_nested_null_values() {
        let mut v = json!({"outer": {"inner": null, "keep": 1}});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"outer": {"keep": 1}}));
    }

    #[test]
    fn sanitize_unwraps_markdown_autolink() {
        let mut v = json!({"path": "[notes.md](http://notes.md)"});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"path": "notes.md"}));
    }

    #[test]
    fn sanitize_unwraps_https_autolink() {
        let mut v = json!({"path": "[src/lib.rs](https://src/lib.rs)"});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"path": "src/lib.rs"}));
    }

    #[test]
    fn sanitize_preserves_real_markdown_links() {
        let mut v = json!({"url": "[click here](https://example.com)"});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"url": "[click here](https://example.com)"}));
    }

    #[test]
    fn sanitize_handles_spaced_url() {
        let mut v = json!({"path": "[notes.md](http://notes. md)"});
        sanitize_parsed_value(&mut v);
        assert_eq!(v, json!({"path": "notes.md"}));
    }

    #[test]
    fn sanitize_no_op_on_clean_input() {
        let mut v = json!({"path": "/foo", "search": "x", "replace": "y"});
        let original = v.clone();
        sanitize_parsed_value(&mut v);
        assert_eq!(v, original);
    }

    // --- try_unwrap_stringified_json tests ---

    #[test]
    fn unwraps_stringified_array() {
        let v = json!(r#"["a","b","c"]"#);
        let unwrapped = try_unwrap_stringified_json(&v).unwrap();
        assert_eq!(unwrapped, json!(["a", "b", "c"]));
    }

    #[test]
    fn unwraps_stringified_object() {
        let v = json!(r#"{"key": "val"}"#);
        let unwrapped = try_unwrap_stringified_json(&v).unwrap();
        assert_eq!(unwrapped, json!({"key": "val"}));
    }

    #[test]
    fn does_not_unwrap_plain_string() {
        let v = json!("hello world");
        assert!(try_unwrap_stringified_json(&v).is_none());
    }

    #[test]
    fn does_not_unwrap_non_string() {
        assert!(try_unwrap_stringified_json(&json!(42)).is_none());
        assert!(try_unwrap_stringified_json(&json!(true)).is_none());
        assert!(try_unwrap_stringified_json(&json!(null)).is_none());
    }

    #[test]
    fn does_not_unwrap_string_number() {
        let v = json!("42");
        assert!(try_unwrap_stringified_json(&v).is_none());
    }
}
