//! What the compiler tells of the workspace as cargo hands it on in JSON: each message's level, code and words, and the places it and its notes name.

use r1_funcore::base::json;
use std::collections::HashSet;

// A place a message names: its file as the workspace's root names it, the byte it begins at, and whether the message is about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Span {
    pub(super) file: String,
    pub(super) start: usize,
    pub(super) primary: bool,
}

// One of the compiler's messages, with its notes and helps: each one's words and the places it names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Message {
    pub(super) level: String,
    pub(super) code: Option<String>,
    pub(super) text: String,
    pub(super) rendered: String,
    pub(super) spans: Vec<Span>,
    pub(super) children: Vec<(String, Vec<Span>)>,
}

// The compiler's messages among the lines cargo writes, each once, however many builds tell it.
pub(super) fn messages(output: &str, seen: &mut HashSet<String>) -> Vec<Message> {
    output
        .lines()
        .filter(|line| json::text_at(line, &["reason"]) == Some("compiler-message"))
        .filter_map(|line| json::value_at(line, &["message"]).and_then(message))
        .filter(|message| seen.insert(message.rendered.clone()))
        .collect()
}

// A message as its object in the JSON holds it.
fn message(object: &str) -> Option<Message> {
    let text = |path: &[&str]| json::text_at(object, path).map(json::unescape);
    let children = json::items(object, &["children"])
        .map(|child| (json::text_at(child, &["message"]).map(json::unescape).unwrap_or_default(), spans(child)));
    Some(Message {
        level: text(&["level"])?,
        code: text(&["code", "code"]),
        text: text(&["message"])?,
        rendered: text(&["rendered"]).unwrap_or_default(),
        spans: spans(object),
        children: children.collect(),
    })
}

// The places an object of a message names.
fn spans(object: &str) -> Vec<Span> {
    json::items(object, &["spans"])
        .filter_map(|span| {
            Some(Span {
                file: json::unescape(json::text_at(span, &["file_name"])?),
                start: json::number_at(span, &["byte_start"])?,
                primary: json::value_at(span, &["is_primary"]) == Some("true"),
            })
        })
        .collect()
}

// The names a message's words quote between backticks.
pub(super) fn quoted(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_message_read_once() {
        let line = r#"{"reason":"compiler-message","package_id":"x","message":{"rendered":"error[E0603]: function `made` is private\n","children":[{"children":[],"code":null,"level":"note","message":"the function `made` is defined here","rendered":null,"spans":[{"byte_end":40,"byte_start":30,"file_name":"crates/funcore/src/a.rs","is_primary":true}]}],"code":{"code":"E0603","explanation":"x"},"level":"error","message":"function `made` is private","spans":[{"byte_end":9,"byte_start":5,"file_name":"crates/impshell/src/b.rs","is_primary":true},{"byte_end":2,"byte_start":1,"file_name":"crates/impshell/src/b.rs","is_primary":false}]}}"#;
        let artifact = r#"{"reason":"compiler-artifact","package_id":"x"}"#;
        let output = format!("{artifact}\n{line}\n{line}\n");
        let mut seen = HashSet::new();
        let read = messages(&output, &mut seen);
        assert_eq!(read.len(), 1, "an artifact is no message, and the same message twice is one");
        let message = &read[0];
        assert_eq!(
            (message.level.as_str(), message.code.as_deref(), quoted(&message.text)),
            ("error", Some("E0603"), vec!["made"])
        );
        assert_eq!(
            message.spans,
            [
                Span { file: "crates/impshell/src/b.rs".into(), start: 5, primary: true },
                Span { file: "crates/impshell/src/b.rs".into(), start: 1, primary: false }
            ]
        );
        assert_eq!(
            message.children,
            [(
                "the function `made` is defined here".to_string(),
                vec![Span { file: "crates/funcore/src/a.rs".into(), start: 30, primary: true }]
            )]
        );
        assert!(messages(&output, &mut seen).is_empty(), "nor is one another build told before");
    }
}
