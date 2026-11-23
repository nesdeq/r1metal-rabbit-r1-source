//! A turn's request: its tools, its items and its body.

use crate::base::code::base64_onto;
use crate::base::json::escape;
use crate::openai::models::Thinking;
use crate::openai::models::chat::{CACHE_HOURS, COMPACT_THRESHOLD, DETAIL, SEARCH_BUDGET, SEARCH_CONTEXT};
use crate::openai::models::writer::VERBOSITY;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

const BODY_SLACK: usize = 512;

/// The device's location for a web search.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Location {
    pub(crate) country: String,
    pub(crate) region: String,
    pub city: String,
    pub(crate) zone: String,
}

/// The tools of every turn: the web search with the location where `has_search`, then the robot's functions, in a fixed order so that the service's cache holds.
pub fn build_tools(location: Option<&Location>, has_search: bool, functions: &str) -> String {
    let list = functions.trim();
    let inner = list.strip_prefix('[').and_then(|inner| inner.strip_suffix(']')).map(str::trim).unwrap_or_default();
    if !has_search {
        return format!("[{inner}]");
    }
    let mut search = format!(
        "{{\"type\":\"web_search\",\"search_context_size\":\"{SEARCH_CONTEXT}\",\"return_token_budget\":\"{SEARCH_BUDGET}\",\"external_web_access\":true"
    );
    if let Some(place) = location {
        search.push_str(",\"user_location\":{\"type\":\"approximate\"");
        for (name, value) in
            [("country", &place.country), ("region", &place.region), ("city", &place.city), ("timezone", &place.zone)]
        {
            if !value.trim().is_empty() {
                let _ = write!(search, ",\"{name}\":\"{}\"", escape(value.trim()));
            }
        }
        search.push('}');
    }
    search.push('}');
    if inner.is_empty() { format!("[{search}]") } else { format!("[{search},{inner}]") }
}

pub(super) fn response_create(
    thinking: Thinking,
    instructions: &str,
    items: &[&str],
    tools: &str,
    previous: Option<&str>,
) -> String {
    let size = instructions.len() + tools.len() + items.iter().map(|item| item.len() + 1).sum::<usize>();
    let mut body = String::with_capacity(size + BODY_SLACK);
    let _ = write!(
        body,
        "{{\"type\":\"response.create\",\"model\":\"{}\",\"reasoning\":{{\"effort\":\"{}\"}},\"text\":{{\"verbosity\":\"{VERBOSITY}\"}},\"instructions\":\"{}\",\"input\":[",
        thinking.model().to_word(),
        thinking.effort().to_word(),
        escape(instructions)
    );
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        body.push_str(item);
    }
    let _ = write!(
        body,
        "],\"tools\":{tools},\"include\":[\"reasoning.encrypted_content\",\"web_search_call.action.sources\"],\"context_management\":[{{\"type\":\"compaction\",\"compact_threshold\":{COMPACT_THRESHOLD}}}],\"prompt_cache_options\":{{\"mode\":\"implicit\",\"ttl\":\"{CACHE_HOURS}h\"}},\"store\":false"
    );
    if let Some(previous) = previous {
        let _ = write!(body, ",\"previous_response_id\":\"{}\"", escape(previous));
    }
    body.push('}');
    body
}

/// The item of a user message with the JPEG photos that go with it, each after its words.
pub fn user_message(text: &str, photos: &[&[u8]]) -> String {
    let mut item = format!(
        "{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{}\"}}",
        escape(text)
    );
    for photo in photos {
        item.push_str(",{\"type\":\"input_image\",\"image_url\":\"data:image/jpeg;base64,");
        base64_onto(photo, &mut item);
        let _ = write!(item, "\",\"detail\":\"{DETAIL}\"}}");
    }
    item.push_str("]}");
    item
}

/// The item that answers a function call, by the call's id.
pub fn function_output(call: &str, output: &str) -> String {
    format!("{{\"type\":\"function_call_output\",\"call_id\":\"{}\",\"output\":\"{}\"}}", escape(call), escape(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUNCTIONS: &str = "[{\"type\":\"function\",\"name\":\"save_memory\"}]";

    #[test]
    fn request_body() {
        let place = Location {
            country: "AT".into(),
            region: "Vienna".into(),
            city: "Vienna".into(),
            zone: "Europe/Vienna".into(),
        };
        let tools = build_tools(Some(&place), true, FUNCTIONS);
        assert_eq!(
            tools,
            "[{\"type\":\"web_search\",\"search_context_size\":\"high\",\"return_token_budget\":\"unlimited\",\"external_web_access\":true,\"user_location\":{\"type\":\"approximate\",\"country\":\"AT\",\"region\":\"Vienna\",\"city\":\"Vienna\",\"timezone\":\"Europe/Vienna\"}},{\"type\":\"function\",\"name\":\"save_memory\"}]",
            "the search first, with all it can be told, then the robot's own functions"
        );
        assert_eq!(
            build_tools(None, true, FUNCTIONS),
            "[{\"type\":\"web_search\",\"search_context_size\":\"high\",\"return_token_budget\":\"unlimited\",\"external_web_access\":true},{\"type\":\"function\",\"name\":\"save_memory\"}]",
            "a device that does not know where it is tells the search nothing of it"
        );
        let with_enum = "[{\"type\":\"function\",\"enum\":[\"a\"]}]";
        assert!(
            build_tools(None, true, with_enum).ends_with(",{\"type\":\"function\",\"enum\":[\"a\"]}]")
                && build_tools(None, true, " [ ] ").ends_with("true}]"),
            "the list's own brackets go and no more, and no function is none"
        );
        assert_eq!(
            build_tools(Some(&place), false, FUNCTIONS),
            "[{\"type\":\"function\",\"name\":\"save_memory\"}]",
            "with the search off, the functions alone"
        );
        let stamped = "[Friday 16 October 2026, 14:02, Vienna, spoken] Wie wird das \"Wetter\"?\nMorgen.";
        let message = user_message(stamped, &[]);
        let chat = Thinking::STANDARD;
        let body =
            response_create(chat, "You are the robot.", &[&message, &function_output("call_1", "saved")], &tools, None);
        assert!(body.starts_with(
            "{\"type\":\"response.create\",\"model\":\"gpt-6.1-terra\",\"reasoning\":{\"effort\":\"high\"},\"text\":{\"verbosity\":\"low\"},\"instructions\":\"You are the robot.\",\"input\":[{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"[Friday 16 October 2026, 14:02, Vienna, spoken] Wie wird das \\\"Wetter\\\"?\\nMorgen.\"}]},{\"type\":\"function_call_output\",\"call_id\":\"call_1\",\"output\":\"saved\"}],\"tools\":[{\"type\":\"web_search\""
        ));
        assert!(
            body.ends_with(
                "],\"include\":[\"reasoning.encrypted_content\",\"web_search_call.action.sources\"],\"context_management\":[{\"type\":\"compaction\",\"compact_threshold\":200000}],\"prompt_cache_options\":{\"mode\":\"implicit\",\"ttl\":\"12h\"},\"store\":false}"
            ),
            "{body}"
        );
        assert!(!body.contains('\n'), "nothing in the body breaks out of its line");
        let next =
            response_create(chat, "You are the robot.", &[&function_output("call_1", "saved")], &tools, Some("resp_1"));
        assert!(
            next.contains(
                "\"input\":[{\"type\":\"function_call_output\",\"call_id\":\"call_1\",\"output\":\"saved\"}]"
            ) && next.ends_with(",\"store\":false,\"previous_response_id\":\"resp_1\"}"),
            "a request after an answer carries only what is new to it, and names the answer: {next}"
        );
    }

    #[test]
    fn photos_after_words() {
        assert_eq!(
            user_message("Was ist das?", &[b"foo", b"f"]),
            "{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Was ist das?\"},{\"type\":\"input_image\",\"image_url\":\"data:image/jpeg;base64,Zm9v\",\"detail\":\"high\"},{\"type\":\"input_image\",\"image_url\":\"data:image/jpeg;base64,Zg==\",\"detail\":\"high\"}]}"
        );
    }
}
