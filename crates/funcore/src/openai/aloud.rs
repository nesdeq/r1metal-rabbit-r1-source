//! A text read aloud by a realtime model over a WebSocket, passage by passage.

use super::models::VOICE;
use super::models::reader::{EFFORT, RATE};
use super::output::COMPLETED;
use crate::base::json::escape;
use crate::base::json::number_at;
use crate::base::json::text_at;
use crate::base::json::unescape;
use alloc::format;
use alloc::string::String;

/// The model's instructions for reading a text.
pub const TASK: &str = include_str!("../../assets/aloud/reader.txt");
const WORDS_HEADER: &str = "Say exactly the following:";
const TEXT_IN: u64 = 60;
const TEXT_CACHED: u64 = 6;
const TEXT_OUT: u64 = 240;
const SOUND_OUT: u64 = 2000;
const RATE_UNIT: u64 = 100;
pub const SESSION_UPDATED: &str = "session.updated";

/// The `session.update` message: the task without reasoning modules, no turn detection, and bare samples in the one voice.
pub fn session_update(task: &str) -> String {
    format!(
        "{{\"type\":\"session.update\",\"session\":{{\"type\":\"realtime\",\"output_modalities\":[\"audio\"],\"reasoning\":{{\"effort\":\"{EFFORT}\"}},\"instructions\":\"{}\",\"audio\":{{\"input\":{{\"turn_detection\":null}},\"output\":{{\"format\":{{\"type\":\"audio/pcm\",\"rate\":{RATE}}},\"voice\":\"{VOICE}\"}}}}}}}}",
        escape(task.trim())
    )
}

/// The `response.create` message of a passage: outside the conversation, so that no passage is paid for again with the next, its words in the instructions, so that the model has nothing to answer.
pub fn response_create(passage: usize, task: &str, words: &str) -> String {
    let instructions = format!("{}\n\n{WORDS_HEADER}\n{}", task.trim(), words.trim());
    format!(
        "{{\"type\":\"response.create\",\"response\":{{\"conversation\":\"none\",\"metadata\":{{\"passage\":\"{passage}\"}},\"input\":[],\"instructions\":\"{}\"}}}}",
        escape(&instructions)
    )
}

/// The `response.cancel` message of a response; its output until then is paid for all the same.
pub fn response_cancel(response: &str) -> String {
    format!("{{\"type\":\"response.cancel\",\"response_id\":\"{}\"}}", escape(response))
}

#[derive(Clone, Copy, Default)]
struct Usage {
    text_in: u64,
    text_cached: u64,
    text_out: u64,
    sound_out: u64,
}

impl Usage {
    const fn micros(&self) -> u64 {
        let units = self
            .text_in
            .saturating_mul(TEXT_IN)
            .saturating_add(self.text_cached.saturating_mul(TEXT_CACHED))
            .saturating_add(self.text_out.saturating_mul(TEXT_OUT))
            .saturating_add(self.sound_out.saturating_mul(SOUND_OUT));
        units.saturating_add(RATE_UNIT / 2) / RATE_UNIT
    }

    fn to_log_line(self, end: Option<&str>) -> String {
        format!(
            "{} tokens of text in, {} of them seen before, {} out, {} tokens of sound, {} millionths of a dollar{}",
            self.text_in.saturating_add(self.text_cached),
            self.text_cached,
            self.text_out,
            self.sound_out,
            self.micros(),
            end.map_or_else(String::new, |end| format!(", ended {end}"))
        )
    }
}

pub(crate) fn parse_done(message: &str) -> (u64, String) {
    let count = |path: &[&str]| number_at::<u64>(message, &[&["response", "usage"][..], path].concat()).unwrap_or(0);
    // The service counts the cached tokens within the text tokens.
    let cached = count(&["input_token_details", "cached_tokens_details", "text_tokens"]);
    let usage = Usage {
        text_in: count(&["input_token_details", "text_tokens"]).saturating_sub(cached),
        text_cached: cached,
        text_out: count(&["output_token_details", "text_tokens"]),
        sound_out: count(&["output_token_details", "audio_tokens"]),
    };
    let status = text_at(message, &["response", "status"]).unwrap_or_default();
    let reason = text_at(message, &["response", "status_details", "reason"]).map(unescape);
    let end =
        (status != COMPLETED).then(|| reason.map_or_else(|| status.into(), |reason| format!("{status}, {reason}")));
    (usage.micros(), usage.to_log_line(end.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANCELLED: &str = r#"{"type":"response.done","response":{"id":"resp_9","status":"cancelled","status_details":{"type":"cancelled","reason":"client_cancelled"},"usage":{"input_token_details":{"text_tokens":1270,"cached_tokens_details":{"text_tokens":1152}},"output_token_details":{"text_tokens":20,"audio_tokens":310}}}}"#;
    const DONE: &str = r#"{"type":"response.done","event_id":"e9","response":{"object":"realtime.response","id":"resp_9","status":"completed","status_details":null,"output":[],"conversation_id":null,"usage":{"total_tokens":2031,"input_tokens":1270,"output_tokens":761,"input_token_details":{"text_tokens":1270,"audio_tokens":0,"image_tokens":0,"cached_tokens":1152,"cached_tokens_details":{"text_tokens":1152,"audio_tokens":0,"image_tokens":0}},"output_token_details":{"text_tokens":41,"audio_tokens":720}}}}"#;

    #[test]
    fn done_usage_and_cost() {
        assert_eq!(
            parse_done(DONE),
            (
                14_638,
                "1270 tokens of text in, 1152 of them seen before, 41 out, 720 tokens of sound, 14638 millionths of a dollar"
                    .into()
            ),
            "a cent and a half for 36 s of voice, nearly all of it for the sound"
        );
        assert_eq!(
            parse_done(CANCELLED),
            (
                6_388,
                "1270 tokens of text in, 1152 of them seen before, 20 out, 310 tokens of sound, 6388 millionths of a dollar, ended cancelled, client_cancelled"
                    .into()
            ),
            "a passage the service was asked to stop is paid for as far as it went"
        );
        let incomplete = r#"{"type":"response.done","response":{"status":"incomplete","status_details":{"type":"incomplete","reason":"max_output_tokens"},"usage":{"output_token_details":{"audio_tokens":4096}}}}"#;
        assert_eq!(
            parse_done(incomplete).1,
            "0 tokens of text in, 0 of them seen before, 0 out, 4096 tokens of sound, 81920 millionths of a dollar, ended incomplete, max_output_tokens"
        );
        assert_eq!(
            parse_done(r#"{"type":"response.done","response":{"status":"failed"}}"#),
            (0, "0 tokens of text in, 0 of them seen before, 0 out, 0 tokens of sound, 0 millionths of a dollar, ended failed".into())
        );
        assert_eq!(
            Usage { sound_out: 15, ..Usage::default() }.to_log_line(Some("cancelled")),
            "0 tokens of text in, 0 of them seen before, 0 out, 15 tokens of sound, 300 millionths of a dollar, ended cancelled"
        );
        assert_eq!(
            Usage { sound_out: 1200, ..Usage::default() }.micros(),
            24_000,
            "2.4 cents for twelve hundred tokens of sound"
        );
        assert_eq!(
            Usage { text_in: u64::MAX, sound_out: u64::MAX, ..Usage::default() }.micros(),
            u64::MAX / RATE_UNIT,
            "and the most there is for a count no answer could hold"
        );
    }

    #[test]
    fn session_and_response_messages() {
        let update = session_update(TASK);
        assert!(
            update.starts_with(
                "{\"type\":\"session.update\",\"session\":{\"type\":\"realtime\",\"output_modalities\":[\"audio\"],\"reasoning\":{\"effort\":\"minimal\"},\"instructions\":\"You read texts aloud"
            ),
            "{update}"
        );
        assert_eq!(text_at(&update, &["session", "reasoning", "effort"]), Some("minimal"), "it thinks the least");
        assert!(
            update.ends_with(
                "\",\"audio\":{\"input\":{\"turn_detection\":null},\"output\":{\"format\":{\"type\":\"audio/pcm\",\"rate\":24000},\"voice\":\"marin\"}}}}"
            ),
            "nothing listens for a voice, and the one voice of every model that speaks reads: {update}"
        );
        assert_eq!(
            text_at(&update, &["session", "instructions"]).map(unescape).as_deref(),
            Some(TASK.trim()),
            "the task alone, without the reasoning modules"
        );
        assert!(
            TASK.contains("exactly as it is written, word for word") && TASK.contains("Add nothing of your own"),
            "the model reads and says nothing of its own"
        );
        assert!(TASK.contains("Do not modify its content"), "its delivery is its own, the words are not");
        assert!(
            TASK.contains("in the language it is written in") && TASK.contains("never translate it"),
            "a passage in another language than the others is read as it is written, in its own language"
        );
        assert!(!update.contains('\n'));
        let create = response_create(7, " Read it.\n", "He said \"no\".\nThen: 5 %.");
        assert_eq!(
            create,
            "{\"type\":\"response.create\",\"response\":{\"conversation\":\"none\",\"metadata\":{\"passage\":\"7\"},\"input\":[],\"instructions\":\"Read it.\\n\\nSay exactly the following:\\nHe said \\\"no\\\".\\nThen: 5 %.\"}}"
        );
        assert_eq!(
            text_at(&create, &["response", "instructions"]).map(unescape).as_deref(),
            Some("Read it.\n\nSay exactly the following:\nHe said \"no\".\nThen: 5 %."),
            "the words stand in the instructions, and no message is there to be answered"
        );
    }

    #[test]
    fn cancel_message() {
        assert_eq!(response_cancel("resp_made_up"), "{\"type\":\"response.cancel\",\"response_id\":\"resp_made_up\"}");
        assert_eq!(
            text_at(&response_cancel("a\"b"), &["response_id"]).map(unescape).as_deref(),
            Some("a\"b"),
            "a name with a quote in it breaks out of nothing"
        );
    }
}
