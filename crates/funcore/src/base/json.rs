//! JSON values read out of a document by path.

use alloc::string::String;
use core::fmt::Write;
use core::iter;
use core::str::{Chars, FromStr};

const HIGH_SURROGATES: core::ops::RangeInclusive<u32> = 0xD800..=0xDBFF;
const LOW_SURROGATES: core::ops::RangeInclusive<u32> = 0xDC00..=0xDFFF;
const SURROGATE_PAYLOAD_BITS: u32 = 10;
const FIRST_SUPPLEMENTARY: u32 = 0x1_0000;
const ESCAPE_DIGITS: usize = 4;
const ESCAPE_RADIX: u32 = 16;

fn string_length(bytes: &[u8]) -> Option<usize> {
    let mut at = 1;
    loop {
        match *bytes.get(at)? {
            b'\\' => at += 2,
            b'"' => return Some(at + 1),
            _ => at += 1,
        }
    }
}

fn nested_length(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = 0;
    loop {
        match *bytes.get(at)? {
            b'"' => at += string_length(&bytes[at..])?,
            b'{' | b'[' => {
                depth += 1;
                at += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                at += 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => at += 1,
        }
    }
}

fn bare_length(bytes: &[u8]) -> Option<usize> {
    let is_end = |byte: &u8| byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']');
    bytes.iter().position(is_end).filter(|length| *length > 0)
}

fn value_length(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    match bytes.first()? {
        b'"' => string_length(bytes),
        b'{' | b'[' => nested_length(bytes),
        _ => bare_length(bytes),
    }
}

fn split_value(text: &str) -> Option<(&str, &str)> {
    let (value, rest) = text.split_at(value_length(text)?);
    let rest = rest.trim_start();
    Some((value, rest.strip_prefix(',').unwrap_or(rest).trim_start()))
}

fn member<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    let value = value.strip_prefix('[').unwrap_or(value).trim_start();
    let mut rest = value.strip_prefix('{')?.trim_start();
    while rest.starts_with('"') {
        let (key, tail) = rest.split_at(value_length(rest)?);
        let body = tail.trim_start().strip_prefix(':')?.trim_start();
        if key[1..key.len() - 1] == *name {
            return Some(body);
        }
        rest = split_value(body)?.1;
    }
    None
}

fn walk<'a>(document: &'a str, path: &[&str]) -> Option<&'a str> {
    let mut value = document.trim_start();
    for name in path {
        value = member(value, name)?;
    }
    Some(value)
}

/// The text at a path, escapes kept; none for any other value.
pub fn text_at<'a>(document: &'a str, path: &[&str]) -> Option<&'a str> {
    let value = value_at(document, path)?;
    value.strip_prefix('"')?.strip_suffix('"')
}

/// The source text of the value at a path.
pub fn value_at<'a>(document: &'a str, path: &[&str]) -> Option<&'a str> {
    split_value(walk(document, path)?).map(|(value, _)| value)
}

/// The number at a path.
pub fn number_at<T: FromStr>(document: &str, path: &[&str]) -> Option<T> {
    value_at(document, path)?.parse().ok()
}

/// The source text of each value of the array at a path.
pub fn items<'a>(document: &'a str, path: &[&str]) -> impl Iterator<Item = &'a str> {
    let body = walk(document, path).and_then(|value| value.strip_prefix('['));
    let mut rest = body.unwrap_or_default().trim_start();
    iter::from_fn(move || {
        let (item, tail) = split_value(rest)?;
        rest = tail;
        Some(item)
    })
}

// The name, escapes kept, and the source text of each member of the object at a path.
pub(crate) fn members<'a>(document: &'a str, path: &[&str]) -> impl Iterator<Item = (&'a str, &'a str)> {
    let body = walk(document, path).and_then(|value| value.strip_prefix('{'));
    let mut rest = body.unwrap_or_default().trim_start();
    iter::from_fn(move || {
        if !rest.starts_with('"') {
            return None;
        }
        let (name, tail) = rest.split_at(value_length(rest)?);
        let (value, next) = split_value(tail.trim_start().strip_prefix(':')?.trim_start())?;
        rest = next;
        Some((&name[1..name.len() - 1], value))
    })
}

fn read_escape_number(digits: &mut Chars<'_>) -> Option<u32> {
    let mut number = 0;
    for _ in 0..ESCAPE_DIGITS {
        number = number * ESCAPE_RADIX + digits.next()?.to_digit(ESCAPE_RADIX)?;
    }
    Some(number)
}

fn take_low_surrogate(rest: &mut Chars<'_>) -> Option<u32> {
    let mut probe = rest.clone();
    let is_escape = probe.next() == Some('\\') && probe.next() == Some('u');
    let low = read_escape_number(&mut probe).filter(|low| is_escape && LOW_SURROGATES.contains(low))?;
    *rest = probe;
    Some(low)
}

fn read_number_escape(rest: &mut Chars<'_>) -> Option<char> {
    let number = read_escape_number(rest)?;
    if !HIGH_SURROGATES.contains(&number) {
        return char::from_u32(number);
    }
    let low = take_low_surrogate(rest)?;
    let high_bits = (number - HIGH_SURROGATES.start()) << SURROGATE_PAYLOAD_BITS;
    char::from_u32(FIRST_SUPPLEMENTARY + high_bits + (low - LOW_SURROGATES.start()))
}

fn read_escape(rest: &mut Chars<'_>) -> Option<char> {
    match rest.next()? {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'b' => Some('\u{8}'),
        'f' => Some('\u{C}'),
        'u' => read_number_escape(rest),
        same @ ('"' | '\\' | '/') => Some(same),
        _ => None,
    }
}

/// Text of `text_at` with its escapes resolved; an invalid escape becomes the replacement character.
pub fn unescape(escaped: &str) -> String {
    let mut text = String::with_capacity(escaped.len());
    let mut rest = escaped.chars();
    while let Some(character) = rest.next() {
        let resolved = if character == '\\' { read_escape(&mut rest) } else { Some(character) };
        text.push(resolved.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    text
}

pub(crate) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ if character.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(character));
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn reads_by_path() {
        let document = r#" { "a" : { "n" : 1, "s" : "x\"}y" , "deep": [ {"n": 7} , {"n": 8} ] }, "b": {"n": 2, "list": [1, [2, 3], {"n": 9}]}, "n": 3 } "#;
        let number = |path: &[&str]| number_at::<u64>(document, path);
        assert_eq!((number(&["n"]), number(&["a", "n"]), number(&["b", "n"])), (Some(3), Some(1), Some(2)));
        assert_eq!((text_at(document, &["a", "s"]), number(&["a", "deep", "n"])), (Some(r#"x\"}y"#), Some(7)));
        assert_eq!(
            (number(&["a", "s"]), text_at(document, &["n"]), number(&["c"]), number(&["b", "list", "n"])),
            (None, None, None, None)
        );
        assert_eq!(
            (value_at(document, &["b", "list"]), value_at(document, &["n"]), value_at(document, &["c"])),
            (Some(r#"[1, [2, 3], {"n": 9}]"#), Some("3"), None)
        );
        assert_eq!(
            (number_at::<i64>("{\"n\": -12600}", &["n"]), number_at::<u64>("{\"n\": -1}", &["n"])),
            (Some(-12_600), None)
        );
        assert_eq!(
            (
                value_length("12, "),
                value_length("\"a\\\\\" "),
                value_length("[1, {\"a\": \"]\"}] x"),
                value_length("{\"a\": [1, 2"),
                value_length("\"open"),
                value_length("12"),
                value_length("")
            ),
            (Some(2), Some(5), Some(15), None, None, None, None)
        );
        assert_eq!(
            (value_length("}"), value_length("]"), value_length(", 1"), value_at("{\"n\": }", &["n"])),
            (None, None, None, None)
        );
    }

    #[test]
    fn array_items() {
        let document = r#"{"output": [ {"type": "reasoning", "summary": []}, 7 , "a]b", [1, 2], {"type": "message", "content": [{"text": "x"}]} ], "none": [], "n": 3}"#;
        let listed: Vec<&str> = items(document, &["output"]).collect();
        assert_eq!(
            listed,
            [
                r#"{"type": "reasoning", "summary": []}"#,
                "7",
                r#""a]b""#,
                "[1, 2]",
                r#"{"type": "message", "content": [{"text": "x"}]}"#
            ]
        );
        let message = items(document, &["output"]).find(|item| text_at(item, &["type"]) == Some("message"));
        assert_eq!(message.and_then(|message| text_at(message, &["content", "text"])), Some("x"));
        assert_eq!(
            (items(document, &["none"]).count(), items(document, &["n"]).count(), items(document, &["gone"]).count()),
            (0, 0, 0)
        );
        assert_eq!(items("[1, 2", &[]).collect::<Vec<_>>(), ["1"]);
        assert_eq!(
            (
                items("[1, 2}", &[]).collect::<Vec<_>>(),
                items("[{\"a\": 1}}]", &[]).collect::<Vec<_>>(),
                items("[1,, 2]", &[]).collect::<Vec<_>>()
            ),
            (vec!["1", "2"], vec!["{\"a\": 1}"], vec!["1"])
        );
    }

    #[test]
    fn object_members() {
        let document = r#"{"headers": { "a" : "x,y" , "b\"": [1, {"c": 2}], "d": 3 }, "empty": {}, "n": 4}"#;
        let listed: Vec<(&str, &str)> = members(document, &["headers"]).collect();
        assert_eq!(listed, [("a", r#""x,y""#), (r#"b\""#, r#"[1, {"c": 2}]"#), ("d", "3")]);
        assert_eq!(
            (
                members(document, &["empty"]).count(),
                members(document, &["n"]).count(),
                members(document, &["x"]).count()
            ),
            (0, 0, 0)
        );
        assert_eq!(members(r#"{"a": 1, "b""#, &[]).collect::<Vec<_>>(), [("a", "1")], "up to what is whole");
        assert_eq!(
            (
                value_at("\n {\"a\": 1} ", &[]),
                number_at::<u64>(" 7 ", &[]),
                items(" [1, 2]", &[]).collect::<Vec<_>>(),
                members(" {\"a\": 1}", &[]).collect::<Vec<_>>()
            ),
            (Some("{\"a\": 1}"), Some(7), vec!["1", "2"], vec![("a", "1")]),
            "a document read whole may begin with blanks, as one read by a path may"
        );
    }

    #[test]
    fn escape_round_trip() {
        let text = "She said \"no\\yes\"\n\tK\u{E4}se \u{1F407} \u{1}\r";
        let escaped = escape(text);
        assert_eq!(escaped, "She said \\\"no\\\\yes\\\"\\n\\tK\u{E4}se \u{1F407} \\u0001\\r");
        assert_eq!(unescape(&escaped), text);
        let numbered = ["a\\/b \\", "u00e4 \\", "uD83D\\", "uDC07 \\b\\f"].concat();
        assert_eq!(unescape(&numbered), "a/b \u{E4} \u{1F407} \u{8}\u{C}");
        assert_eq!(unescape(r"\uD83D x \uDC07 \u12 \q \"), "\u{FFFD} x \u{FFFD} \u{FFFD}\u{FFFD} \u{FFFD}");
        let document = format!("{{\"text\": \"{escaped}\"}}");
        assert_eq!(text_at(&document, &["text"]).map(unescape).as_deref(), Some(text));
    }
}
