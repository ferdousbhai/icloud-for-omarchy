//! JSON as the command line prints it: the layout the Qt app's
//! QJsonDocument writes (four spaces, an object's keys sorted, an empty
//! array or object over two lines), so both binaries print the same bytes.

use icloud_session::output::json_safe;
use serde_json::Value;

/// `value` pretty-printed, as the value of something `depth` levels in
/// (its later lines indented that far), without a trailing newline, and
/// safe on a terminal (U+007F..U+009F escaped).
pub fn text(value: &Value, depth: usize) -> String {
    let mut out = String::new();
    write(&mut out, value, depth);
    json_safe(&out)
}

fn indent(out: &mut String, depth: usize) {
    out.extend(std::iter::repeat_n("    ", depth));
}

fn write(out: &mut String, value: &Value, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n)),
        Value::String(s) => string(out, s),
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write(out, item, depth + 1);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            // By UTF-16 units, as QJsonObject keeps its keys.
            entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push_str("{\n");
            for (i, (key, item)) in entries.iter().enumerate() {
                indent(out, depth + 1);
                string(out, key);
                out.push_str(": ");
                write(out, item, depth + 1);
                out.push_str(if i + 1 < entries.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A whole number as it is; any other as its shortest digits, in
/// exponent form below 1e-4 and from 1e17 (`%.17g` without the padding).
fn number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let Some(f) = n.as_f64().filter(|f| f.is_finite()) else {
        return "null".into();
    };
    if f == 0.0 {
        return "0".into();
    }
    // "d.ddde-7": the shortest digits that read back as f, and its exponent.
    let scientific = format!("{:e}", f.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} prints an exponent");
    let digits = mantissa.replace('.', "");
    let exponent: i32 = exponent.parse().expect("a whole exponent");
    let sign = if f < 0.0 { "-" } else { "" };
    if (-4..17).contains(&exponent) {
        // Rust's own decimal form is those digits with the point in place.
        return format!("{f}");
    }
    let fraction = if digits.len() > 1 {
        format!(".{}", &digits[1..])
    } else {
        String::new()
    };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    format!("{sign}{}{fraction}e{exponent_sign}{:02}", &digits[..1], exponent.abs())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_layout_is_qjsondocuments() {
        let value = json!({"b": [1, "two", null, true], "a": {}, "c": [], "d": {"z": 1, "y": [[]]}});
        assert_eq!(
            text(&value, 0),
            "{\n    \"a\": {\n    },\n    \"b\": [\n        1,\n        \"two\",\n        null,\n        true\n    ],\n    \
             \"c\": [\n    ],\n    \"d\": {\n        \"y\": [\n            [\n            ]\n        ],\n        \"z\": 1\n    }\n}"
        );
        assert_eq!(text(&json!([1]), 1), "[\n        1\n    ]");
        assert_eq!(text(&json!("plain"), 3), "\"plain\"");
        assert_eq!(text(&Value::Null, 0), "null");
    }

    #[test]
    fn strings_are_escaped_as_qt_escapes_them() {
        assert_eq!(
            text(&json!("a\"b\\c\n\r\t\u{8}\u{c}\u{1}\u{1f}é😀/"), 0),
            "\"a\\\"b\\\\c\\n\\r\\t\\b\\f\\u0001\\u001fé😀/\""
        );
        // Safe on a terminal: DEL and the C1 controls are escaped too.
        assert_eq!(text(&json!("\u{7f}\u{9b}"), 0), "\"\\u007f\\u009b\"");
    }

    #[test]
    fn numbers_print_as_qt_prints_them() {
        let print = |source: &str| text(&serde_json::from_str::<Value>(source).unwrap(), 0);
        for (source, printed) in [
            ("0", "0"),
            ("-7", "-7"),
            ("1790000000000", "1790000000000"),
            ("1.0", "1"),
            ("1.5", "1.5"),
            ("-0.25", "-0.25"),
            ("0.0001", "0.0001"),
            ("0.00001", "1e-05"),
            ("1e16", "10000000000000000"),
            ("1e17", "1e+17"),
            ("1.5e300", "1.5e+300"),
            ("-2.5e-10", "-2.5e-10"),
            ("0.0", "0"),
        ] {
            assert_eq!(print(source), printed, "{source}");
        }
    }
}
