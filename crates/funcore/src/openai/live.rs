//! The translator's session with the live model over a WebSocket.

use super::models::VOICE;
use super::models::interpreter::{MODEL, RATE};
use super::money::cost_by_minute;
use super::request::with_modules;
use crate::base::code::base64_onto;
use crate::base::json::escape;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const MICROS_PER_MINUTE: u64 = 50_000;
const TASK: &str = include_str!("../../assets/live/translator.txt");
const GREETING: &str = include_str!("../../assets/live/greeting.txt");
const NO_DELEGATION: &str =
    "Nothing can be delegated here: there is no backend and no tool. Interpret what was said yourself, now.";
pub const SESSION_STARTED: &str = "session.started";
pub const SESSION_CLOSE: &str = "{\"type\":\"session.close\",\"event_id\":\"close\"}";
const SOUND_HEAD: &str = "{\"type\":\"session.input_audio.append\",\"audio\":\"";
const SOUND_TAIL: &str = "\"}";

/// The `session.start` message: the model, its task, and the sound both ways.
pub fn session_start() -> String {
    format!(
        "{{\"type\":\"session.start\",\"event_id\":\"start\",\"session\":{{\"model\":\"{MODEL}\",\"instructions\":\"{}\",\"audio\":{{\"format\":{{\"type\":\"audio/pcm\",\"rate\":{RATE}}},\"output\":{{\"voice\":\"{VOICE}\"}}}}}}}}",
        escape(&with_modules(TASK))
    )
}

/// The message that has the model introduce itself, sent once the session stands.
pub fn greeting() -> String {
    format!(
        "{{\"type\":\"session.instructions.append\",\"event_id\":\"greet\",\"delegation_id\":null,\"content\":\"{}\"}}",
        escape(GREETING.trim())
    )
}

/// The message that hands delegated work back to the model, by its id, as something for the model to know and not to say.
pub fn hand_back(work: &str) -> String {
    format!(
        "{{\"type\":\"session.thinking.append\",\"event_id\":\"back\",\"delegation_id\":\"{}\",\"content\":\"{}\"}}",
        escape(work),
        escape(NO_DELEGATION)
    )
}

/// The cost of a session of that many seconds in millionths of a dollar; more seconds than any session has cost the most there is.
pub const fn session_micros(seconds: u64) -> u64 {
    cost_by_minute(seconds, MICROS_PER_MINUTE)
}

/// The messages that carry the microphone's sound, each written into what held the one before.
#[derive(Default)]
pub struct Uplink {
    bytes: Vec<u8>,
    message: String,
}

impl Uplink {
    /// The message for the next samples.
    pub fn encode(&mut self, samples: &[i16]) -> &str {
        self.bytes.clear();
        self.bytes.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        self.message.clear();
        self.message.push_str(SOUND_HEAD);
        base64_onto(&self.bytes, &mut self.message);
        self.message.push_str(SOUND_TAIL);
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::json::text_at;
    use crate::base::json::unescape;
    use crate::base::time::SECONDS_PER_MINUTE;

    #[test]
    fn session_messages() {
        let start = session_start();
        assert!(
            start.starts_with(
                "{\"type\":\"session.start\",\"event_id\":\"start\",\"session\":{\"model\":\"gpt-live-1\",\"instructions\":\"INTERPRET ONLY. NEVER DELEGATE, CHECK, ANSWER, SEARCH, OR USE TOOLS."
            ),
            "the task opens with the line the service's own guide gives an interpreter: a session is held with nothing to delegate to"
        );
        assert!(
            start.ends_with(
                "\",\"audio\":{\"format\":{\"type\":\"audio/pcm\",\"rate\":24000},\"output\":{\"voice\":\"marin\"}}}}"
            ),
            "{start}"
        );
        assert_eq!(text_at(&start, &["session", "instructions"]).map(unescape), Some(with_modules(TASK)));
        assert_eq!(
            text_at(&start, &["session", "audio", "output", "voice"]),
            Some(VOICE),
            "it speaks with the one voice of every model that speaks"
        );
        assert!(
            TASK.contains("between two people who do not share a language") && TASK.contains("pocket device"),
            "the model knows where it runs and between whom"
        );
        assert!(
            TASK.contains("introduce themselves, each in their own language")
                && TASK.contains("The language the first of them speaks is one of the two languages"),
            "and takes its two languages from how the two introduce themselves"
        );
        let greet = greeting();
        assert!(greet.starts_with(
            "{\"type\":\"session.instructions.append\",\"event_id\":\"greet\",\"delegation_id\":null,\"content\":\"Speak first, now"
        ));
        assert_eq!(text_at(&greet, &["content"]).map(unescape).as_deref(), Some(GREETING.trim()));
        assert!(
            GREETING.contains("introduce yourselves, one after the other, each in your own language"),
            "its first words ask the two for what tells it their languages"
        );
        assert!(
            GREETING.split_whitespace().count() < 250,
            "what it is told once the session stands stays far under the 500 tokens the service takes there"
        );
        assert!(!start.contains('\n') && !greet.contains('\n'), "nothing breaks out of its line");
        assert_eq!(text_at(SESSION_CLOSE, &["type"]), Some("session.close"));
    }

    #[test]
    fn sound_up() {
        let mut uplink = Uplink::default();
        assert_eq!(
            uplink.encode(&[0, 1, -1, 256]),
            "{\"type\":\"session.input_audio.append\",\"audio\":\"AAABAP//AAE=\"}",
            "16 bits a sample, the low byte first"
        );
        let buffer = uplink.encode(&[0; 4]).as_ptr();
        assert_eq!(uplink.encode(&[7; 4]).as_ptr(), buffer, "the next message is written into what held the last");
    }

    #[test]
    fn hand_back_and_cost() {
        let back = hand_back("item \"1\"");
        assert!(
            back.starts_with(
                "{\"type\":\"session.thinking.append\",\"event_id\":\"back\",\"delegation_id\":\"item \\\"1\\\"\",\"content\":\"Nothing can be delegated here"
            ),
            "{back}"
        );
        assert_eq!(text_at(&back, &["content"]), Some(NO_DELEGATION), "what the model is told needs no escapes");
        assert_eq!(
            (session_micros(60), session_micros(1), session_micros(0), session_micros(3600)),
            (50_000, 833, 0, 3_000_000)
        );
        assert_eq!(
            session_micros(u64::MAX),
            u64::MAX / SECONDS_PER_MINUTE,
            "and the most there is for a count no answer holds"
        );
    }
}
