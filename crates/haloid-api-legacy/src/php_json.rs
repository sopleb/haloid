//! JSON written the way upstream's PHP writes it.
//!
//! `json_encode` with default flags escapes `/` as `\/` and every non-ASCII
//! character as a `\uXXXX` escape (a surrogate pair beyond the BMP), in
//! lowercase hex. Valid JSON either way, and identical to any parser; but
//! some clients match on the raw text, and the golden tests compare bytes,
//! so the legacy endpoint reproduces it exactly.
//!
//! Key order is the `Value`'s own: `serde_json` is built with
//! `preserve_order`, which the response builder relies on.

use serde_json::Value;
use std::fmt::Write;

pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7E => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slashes_and_unicode_are_escaped_as_php_does() {
        assert_eq!(
            to_string(&json!("https://img.example.com/a/b.jpg")),
            r#""https:\/\/img.example.com\/a\/b.jpg""#
        );
        assert_eq!(to_string(&json!("caf\u{e9}")), "\"caf\\u00e9\"");
        assert_eq!(to_string(&json!("\u{732b}")), "\"\\u732b\"");
        assert_eq!(
            to_string(&json!("\u{1f600}")),
            "\"\\ud83d\\ude00\"",
            "surrogate pair"
        );
        assert_eq!(
            to_string(&json!("a\"b\\c\nd\u{1}")),
            r#""a\"b\\c\nd\u0001""#
        );
        assert_eq!(to_string(&json!("\u{7f}")), r#""\u007f""#);
    }

    #[test]
    fn structure_and_order_are_kept() {
        let value = json!({"z": 1, "a": [true, null, 1.5], "m": {"k": "v"}});
        assert_eq!(
            to_string(&value),
            r#"{"z":1,"a":[true,null,1.5],"m":{"k":"v"}}"#
        );
    }

    #[test]
    fn it_reproduces_a_captured_upstream_body() {
        // The no-key error, byte for byte as the live instance sent it.
        let captured = include_str!("../../../fixtures/golden/upstream/error-no-key-json.body");
        let parsed: Value = serde_json::from_str(captured).unwrap();
        assert_eq!(to_string(&parsed), captured);

        // A full success body, slashes and all.
        let captured = include_str!("../../../fixtures/golden/upstream/success-params.body");
        let parsed: Value = serde_json::from_str(captured).unwrap();
        assert_eq!(to_string(&parsed), captured);
    }
}
