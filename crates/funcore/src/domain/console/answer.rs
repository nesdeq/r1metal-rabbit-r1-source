//! The device's answers to a ping, a key and a recording.

use crate::base::time::NANOS_PER_MICRO;
use alloc::format;
use alloc::string::{String, ToString};

const PONG: &str = "pong";
/// The answer to `key` where an OpenAI key is stored.
pub const KEY_STORED: &str = "kept";
const NO_KEY: &str = "none";

/// The answer to `key`, by the keeper's last word on a stored key; none before it.
pub fn key_answer(has_key: Option<bool>) -> &'static str {
    if has_key == Some(true) { KEY_STORED } else { NO_KEY }
}

pub(super) fn keep_refusal(reason: &str) -> String {
    format!("not kept: {reason}")
}

/// The answer to a ping: the word it carries, or `PONG`.
pub fn pong_answer(word: Option<&str>) -> &str {
    word.unwrap_or(PONG)
}

// The start tells the host its recording from earlier leftovers.
fn record_answer(at_ns: u64) -> String {
    (at_ns / NANOS_PER_MICRO).to_string()
}

/// The answer to `record`; `record off` has none.
pub fn to_record_answer(is_on: bool, at_ns: u64) -> Option<String> {
    is_on.then(|| record_answer(at_ns))
}

/// A recording's start in microseconds from the answer to `record on`.
pub fn parse_record_answer(answer: &str) -> Option<u64> {
    answer.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_key_and_record_answers() {
        assert_eq!(
            (pong_answer(None), pong_answer(Some("4711"))),
            (PONG, "4711"),
            "the word a ping carries comes back"
        );
        assert_eq!(keep_refusal("no room"), "not kept: no room");
        assert_eq!(record_answer(1_792_000_000_123_456_789), "1792000000123456", "in microseconds");
        assert_eq!(
            (parse_record_answer(&record_answer(7_000)), parse_record_answer("soon")),
            (Some(7), None),
            "and read back by the host"
        );
        assert_eq!([key_answer(Some(true)), key_answer(Some(false)), key_answer(None)], [KEY_STORED, NO_KEY, NO_KEY]);
    }

    #[test]
    fn record_off_unanswered() {
        assert_eq!((to_record_answer(true, 7_000).as_deref(), to_record_answer(false, 7_000)), (Some("7"), None));
    }
}
