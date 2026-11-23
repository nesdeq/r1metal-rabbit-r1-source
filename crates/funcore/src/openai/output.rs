// The Responses API's output: an answer's text without the marks it cites by, and its web searches.

use crate::base::json::items;
use crate::base::json::text_at;
use crate::base::json::unescape;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const COMPLETED: &str = "completed";
pub(super) const MESSAGE: &str = "message";
pub(super) const SEARCH_CALL: &str = "web_search_call";
const SEARCH_ACTION: &str = "search";
// What a web search of the model's costs, in millionths of a dollar, beside the tokens of its answer.
pub(super) const SEARCH_MICROS: u64 = 10_000;
const OUTPUT_TEXT: &str = "output_text";
const CITATION_OPEN: char = '\u{E200}';
const CITATION_CLOSE: char = '\u{E201}';
const CITATION_SEPARATOR: char = '\u{E202}';

pub(super) fn output_text<'a>(output: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<String> = output
        .filter(|item| text_at(item, &["type"]) == Some(MESSAGE))
        .flat_map(|item| items(item, &["content"]))
        .filter(|part| text_at(part, &["type"]) == Some(OUTPUT_TEXT))
        .filter_map(|part| text_at(part, &["text"]).map(unescape))
        .collect();
    strip_citations(&parts.join("\n\n")).trim().replace("\r\n", "\n")
}

// The web searches among an answer's items, each of which is paid for; a page the model opened is none.
pub(super) fn search_count<'a>(output: impl Iterator<Item = &'a str>) -> u64 {
    let is_search = |item: &&str| {
        text_at(item, &["type"]) == Some(SEARCH_CALL) && text_at(item, &["action", "type"]) == Some(SEARCH_ACTION)
    };
    output.filter(is_search).count() as u64
}

pub(crate) fn strip_citations(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let (head, mark) = rest.split_once(CITATION_OPEN).map_or((rest, None), |(head, mark)| (head, Some(mark)));
        output.extend(head.chars().filter(|symbol| *symbol != CITATION_CLOSE && *symbol != CITATION_SEPARATOR));
        let Some(mark) = mark else {
            return output;
        };
        output.truncate(output.trim_end_matches(' ').len());
        let Some((_, tail)) = mark.split_once(CITATION_CLOSE) else {
            return output;
        };
        rest = tail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_text() {
        let output = [
            r#"{"type": "reasoning", "summary": [{"type": "summary_text", "text": "thinking"}]}"#,
            r#"{"type": "message", "content": [{"type": "output_text", "text": "One.\r\nTwo."}, {"type": "refusal", "refusal": "No."}]}"#,
            r#"{"type": "message", "content": [{"type": "output_text", "text": "Three. "}]}"#,
        ];
        assert_eq!(output_text(output.into_iter()), "One.\nTwo.\n\nThree.");
        assert_eq!(output_text(core::iter::empty()), "", "no message, no text");
        let cited = [
            "{\"type\": \"message\", \"content\": [{\"type\": \"output_text\", \"text\": \"No. The train leaves at nine. \\ue200cite\\ue202turn0search4\"}]}",
            "{\"type\": \"message\", \"content\": [{\"type\": \"output_text\", \"text\": \"\\ue202turn0search7\\ue201\"}]}",
        ];
        assert_eq!(
            output_text(cited.into_iter()),
            "No. The train leaves at nine.",
            "a mark the model cites by is no text, even where it runs from one message into the next"
        );
    }

    #[test]
    fn citation_marks() {
        assert_eq!(
            strip_citations(
                "Yes \u{E200}cite\u{E202}turn0search4\u{E202}turn0news2\u{E201}, the second one was his brother."
            ),
            "Yes, the second one was his brother."
        );
        assert_eq!(strip_citations("Yes.  \u{E200}cite\u{E202}turn0"), "Yes.", "a mark still coming");
        assert_eq!(strip_citations("Yes.\n\u{E200}cite\u{E201}\nNo."), "Yes.\n\nNo.", "a line's end stays");
        assert_eq!(strip_citations("a\u{E201}b\u{E202}c"), "abc", "a closing or a parting without its opening");
        assert_eq!(
            (strip_citations(""), strip_citations("Good. (example.com)")),
            (String::new(), String::from("Good. (example.com)"))
        );
        let reply =
            "Yes. It was \u{E200}cite\u{E202}turn0search1\u{E201} his brother.\nNothing \u{E200}cite\u{E201}more.";
        let expected = strip_citations(reply);
        for cut in (0..=reply.len()).filter(|cut| reply.is_char_boundary(*cut)) {
            let partial = strip_citations(&reply[..cut]);
            assert!(
                expected.starts_with(partial.trim_end_matches(' ')),
                "what has come of a reply shows as the beginning of the whole: {partial:?}"
            );
        }
    }
}
