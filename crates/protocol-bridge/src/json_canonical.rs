// Lint scope for this module: the algorithm is ported from CC Switch and keeps
// its original structure, which trips style/complexity/perf lints that would be
// noise here. Correctness and suspicious lints stay ENABLED on purpose - those
// are the ones that catch real protocol bugs. Do not widen this to
// `clippy::all`, which would silently disable them again.

//! Stable JSON helpers for cache-sensitive request bodies.

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        Value::Object(map) => {
            let mut entries = map.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));

            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonicalize_value(value));
            }
            Value::Object(sorted)
        }
        other => other,
    }
}

pub fn canonical_json_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        // `String` is an infallible serde_json writer in practice, but this
        // helper is used on request data and must not turn a serialization
        // failure into a process panic. The fallback is valid JSON and keeps
        // the non-Result API backward compatible.
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(values) => {
            let parts = values.iter().map(canonical_json_string).collect::<Vec<_>>();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(left, _)| *left);
            let parts = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned());
                    format!("{key}:{}", canonical_json_string(value))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// Re-serialize a JSON document only when doing so cannot change a number.
///
/// `serde_json::Value` stores a number as `i64`, `u64` or `f64`. An integer
/// literal wider than `u64` — a `uint256` from a chain query, a 26-digit
/// database id, a product of two big primes — is therefore parsed as `f64` and
/// printed back in scientific notation with the low digits replaced by zeros:
/// `12345678901234567890123` becomes `1.2345678901234568e22`. Canonicalization
/// is a cache-key and whitespace normalization, never a licence to rewrite the
/// payload, and silently corrupting a tool argument or a tool result sends the
/// model (and whatever it calls next) a different number than the user's.
///
/// So every numeric literal in the source text is compared against what
/// `serde_json` would print for it. Any mismatch — including harmless
/// reformattings such as `1e2` → `100.0` — makes the caller keep the original
/// text verbatim instead of canonicalizing it. Losing key ordering on those rare
/// documents costs a cache hit; losing digits costs correctness.
pub fn json_text_canonicalizes_losslessly(text: &str) -> bool {
    json_number_literals(text)
        .into_iter()
        .all(|literal| rendered_number(literal).is_some_and(|rendered| rendered == literal))
}

/// What `serde_json` prints for a bare numeric literal, or `None` when the
/// literal is not a JSON number this crate can represent.
fn rendered_number(literal: &str) -> Option<String> {
    match serde_json::from_str::<Value>(literal) {
        Ok(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

/// Raw source text of every numeric literal in a **valid** JSON document.
///
/// Numbers inside strings must not be reported: `"id": "1e999"` is text and
/// canonicalization leaves it alone. The scan tracks string state and honours
/// backslash escapes so a `\"` does not end the string early.
fn json_number_literals(text: &str) -> Vec<&str> {
    const NUMBER_BYTES: &[u8] = b"0123456789.eE+-";
    let bytes = text.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    let mut in_string = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            match byte {
                // Skip the escaped byte as well, so `"\\"` does not look like
                // the string ended.
                b'\\' => index += 1,
                b'"' => in_string = false,
                _ => {}
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        if byte == b'-' || byte.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len() && NUMBER_BYTES.contains(&bytes[index]) {
                index += 1;
            }
            literals.push(&text[start..index]);
            continue;
        }
        index += 1;
    }

    literals
}

pub fn canonicalize_json_string_if_parseable(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }

    match serde_json::from_str::<Value>(trimmed) {
        Ok(parsed) if json_text_canonicalizes_losslessly(trimmed) => canonical_json_string(&parsed),
        // Either not JSON, or JSON whose numbers `serde_json` cannot represent
        // exactly. Both cases must pass the caller's bytes through untouched.
        _ => value.to_string(),
    }
}

/// Normalize a tool-call `arguments` string into a valid JSON payload.
///
/// Identical to [`canonicalize_json_string_if_parseable`] except that an empty
/// (or whitespace-only) value is coerced to `"{}"` instead of being passed
/// through verbatim. A no-argument tool call must serialize as `"{}"`; strict
/// upstreams such as Minimax reject `arguments: ""` with a 400
/// `invalid function arguments json string` error, whereas lenient ones
/// (OpenAI, Kimi) silently treat it as an empty object.
pub fn canonicalize_tool_arguments_str(value: &str) -> String {
    if value.trim().is_empty() {
        return "{}".to_string();
    }
    canonicalize_json_string_if_parseable(value)
}

/// Normalize a tool-call `arguments` field from a Responses/Chat item.
///
/// Mirrors the inline `match` that several transform paths used to duplicate:
/// a string is canonicalized (with empty coerced to `"{}"`), a structured
/// value is serialized canonically, and a missing field defaults to `"{}"`.
pub fn canonicalize_tool_arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => canonicalize_tool_arguments_str(s),
        Some(v) => canonical_json_string(v),
        None => "{}".to_string(),
    }
}

pub fn short_value_hash(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "absent".to_string();
    };
    short_sha256_hex(canonical_json_string(value).as_bytes())
}

pub fn short_sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_string_sorts_nested_object_keys() {
        let left = json!({
            "b": 2,
            "a": {
                "d": true,
                "c": [3, {"z": 1, "y": 2}]
            }
        });
        let right = json!({
            "a": {
                "c": [3, {"y": 2, "z": 1}],
                "d": true
            },
            "b": 2
        });

        assert_eq!(canonical_json_string(&left), canonical_json_string(&right));
        assert_eq!(
            short_value_hash(Some(&left)),
            short_value_hash(Some(&right))
        );
    }

    #[test]
    fn canonicalize_value_sorts_map_storage_order() {
        let value = canonicalize_value(json!({"b": 2, "a": 1}));

        assert_eq!(serde_json::to_string(&value).unwrap(), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn canonicalize_json_string_if_parseable_sorts_keys_and_removes_whitespace() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 2, "a": 1 }"#),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn canonicalize_json_string_if_parseable_preserves_plain_text() {
        assert_eq!(
            canonicalize_json_string_if_parseable("plain text"),
            "plain text"
        );
    }

    #[test]
    fn canonicalize_tool_arguments_str_coerces_empty_to_object() {
        assert_eq!(canonicalize_tool_arguments_str(""), "{}");
        assert_eq!(canonicalize_tool_arguments_str("   "), "{}");
        assert_eq!(canonicalize_tool_arguments_str("\n\t"), "{}");
    }

    #[test]
    fn canonicalize_tool_arguments_str_canonicalizes_valid_json() {
        assert_eq!(
            canonicalize_tool_arguments_str(r#"{ "b": 2, "a": 1 }"#),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn canonicalize_tool_arguments_handles_field_variants() {
        // Missing field -> empty object.
        assert_eq!(canonicalize_tool_arguments(None), "{}");
        // Empty string field -> empty object.
        assert_eq!(canonicalize_tool_arguments(Some(&json!(""))), "{}");
        // String field with JSON -> canonicalized.
        assert_eq!(
            canonicalize_tool_arguments(Some(&json!(r#"{"b":2,"a":1}"#))),
            r#"{"a":1,"b":2}"#
        );
        // Structured (non-string) field -> canonical serialization.
        assert_eq!(
            canonicalize_tool_arguments(Some(&json!({"b": 2, "a": 1}))),
            r#"{"a":1,"b":2}"#
        );
    }

    // ------------------------------------------------------------------
    // numeric precision
    // ------------------------------------------------------------------

    #[test]
    fn canonicalize_json_string_if_parseable_sorts_keys_when_numbers_survive() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 2, "a": [1, -3, 4.5] }"#),
            r#"{"a":[1,-3,4.5],"b":2}"#
        );
    }

    /// A `u64`-wide integer is exactly representable and must still be
    /// canonicalized; only literals `serde_json` would reprint differently are
    /// protected.
    #[test]
    fn canonicalize_keeps_wide_but_exact_integers() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 1, "a": 18446744073709551615 }"#),
            r#"{"a":18446744073709551615,"b":1}"#
        );
    }

    #[test]
    fn canonicalize_refuses_to_corrupt_an_integer_wider_than_u64() {
        let input = r#"{ "b": 1, "a": 12345678901234567890123 }"#;
        assert_eq!(
            canonicalize_json_string_if_parseable(input),
            input,
            "a >u64 integer must be passed through byte-for-byte, not turned into 1.234…e22"
        );
        assert!(
            !json_text_canonicalizes_losslessly(input),
            "the guard must report this document as unsafe to rewrite"
        );
    }

    #[test]
    fn canonicalize_refuses_to_corrupt_a_negative_integer_below_i64() {
        let input = r#"{"delta":-99999999999999999999999999}"#;
        assert_eq!(canonicalize_json_string_if_parseable(input), input);
    }

    /// One unsafe number anywhere in the document disables canonicalization for
    /// the whole document; a partial rewrite would be worse than none.
    #[test]
    fn one_wide_integer_disables_canonicalization_for_the_whole_document() {
        let input = r#"{ "z": 1, "nested": { "y": [ { "x": 12345678901234567890123 } ] } }"#;
        assert_eq!(canonicalize_json_string_if_parseable(input), input);
    }

    /// Numbers inside strings are text. Canonicalization already leaves strings
    /// alone, and the scanner must not mistake them for literals — otherwise
    /// every tool result quoting a big id as a string would stop being
    /// canonicalized and lose its cache key for no reason.
    #[test]
    fn numeric_text_inside_a_string_does_not_disable_canonicalization() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 1, "a": "12345678901234567890123" }"#),
            r#"{"a":"12345678901234567890123","b":1}"#
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_early() {
        // The `\"` must not flip the scanner out of string state, or the digits
        // after it would be read as a bare number literal.
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 1, "a": "say \"hi\" 1e999" }"#),
            r#"{"a":"say \"hi\" 1e999","b":1}"#
        );
    }

    #[test]
    fn a_trailing_backslash_inside_a_string_is_not_an_escape_of_the_quote() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 1, "a": "C:\\path\\" }"#),
            r#"{"a":"C:\\path\\","b":1}"#
        );
    }

    /// Reformattings that change the literal's text but not its value are still
    /// refused: the contract is "the bytes the caller sent are the bytes the
    /// model sees", and a scientific-notation argument is what a strict gateway
    /// may have been given.
    #[test]
    fn scientific_notation_is_passed_through_verbatim() {
        let input = r#"{"a":1e2}"#;
        assert_eq!(canonicalize_json_string_if_parseable(input), input);
    }

    #[test]
    fn tool_arguments_inherit_the_precision_guard() {
        let input = r#"{ "amount": 12345678901234567890123 }"#;
        assert_eq!(canonicalize_tool_arguments_str(input), input);
        // The empty-arguments coercion still applies.
        assert_eq!(canonicalize_tool_arguments_str("  "), "{}");
    }

    #[test]
    fn json_number_literals_skips_strings_and_reports_every_number() {
        assert_eq!(
            json_number_literals(r#"{"a":1,"b":[-2.5,"33",{"c":4e1}],"d":"x"}"#),
            vec!["1", "-2.5", "4e1"]
        );
        assert!(json_number_literals(r#"{"a":"no numbers here"}"#).is_empty());
    }
}
