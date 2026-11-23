//! A message as the model gets it.

use super::InputKind;
use crate::base::time::Moment;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

/// The stamp a message begins with: `[Friday 16 October 2026, 14:02, Vienna, spoken]`.
pub fn to_stamp(local: u64, city: Option<&str>, kind: Option<InputKind>) -> String {
    let now = Moment::from_seconds(local);
    let mut stamp = format!("[{}, {}", now.to_date_words(true, true), now.to_time_text());
    if let Some(city) = city.map(str::trim).filter(|city| !city.is_empty()) {
        let _ = write!(stamp, ", {city}");
    }
    stamp.push_str(match kind {
        Some(InputKind::Voice) => ", spoken]",
        Some(InputKind::Text) => ", typed]",
        None => "]",
    });
    stamp
}

pub(super) fn to_heard_part(passages: &[String], heard_count: usize) -> Option<String> {
    (heard_count < passages.len()).then(|| passages[..heard_count].join(" "))
}

fn last_sentence(text: &str) -> &str {
    let parts = text.rsplit(['.', '!', '?']).map(str::trim);
    parts.into_iter().find(|part| !part.is_empty()).unwrap_or(text)
}

/// A message for the model: its stamp, the heard part of the last reply, and its words.
pub fn to_message(stamp: &str, heard: Option<&str>, words: &str) -> String {
    let words = words.trim();
    match heard.map(str::trim) {
        None => format!("{stamp} {words}"),
        Some("") => format!("{stamp} [The user cut your last reply short before a word of it was heard.] {words}"),
        Some(heard) => format!(
            "{stamp} [The user cut your last reply short: the last they heard of it was \"{}\".] {words}",
            last_sentence(heard)
        ),
    }
}

/// The message of the robot's first wake.
pub fn to_wake_message(stamp: &str) -> String {
    format!("{stamp} [You have just been switched on for the first time. Nobody has said anything yet: speak first.]")
}

/// A message with the photo that goes with it: as the request sends it, and as the thread keeps it, the photo no longer with it.
pub fn to_shown_messages(message: &str) -> (String, String) {
    let sent = format!("{message} [With this message, a photo the user took with the device's camera.]");
    let kept = format!("{sent} [You saw it in that turn and no longer see it.]");
    (sent, kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_159_320;

    #[test]
    fn stamp_and_message() {
        let spoken = to_stamp(NOW, Some(" Vienna "), Some(InputKind::Voice));
        assert_eq!(spoken, "[Friday 16 October 2026, 14:02, Vienna, spoken]");
        assert_eq!(to_stamp(NOW + 59, None, Some(InputKind::Text)), "[Friday 16 October 2026, 14:02, typed]");
        assert_eq!(
            to_message(&spoken, None, " Und morgen? "),
            "[Friday 16 October 2026, 14:02, Vienna, spoken] Und morgen?"
        );
        assert_eq!(
            to_message(&spoken, Some("Regen ab Mittag. Nimm den Schirm, sonst"), "Stopp."),
            "[Friday 16 October 2026, 14:02, Vienna, spoken] [The user cut your last reply short: the last they heard of it was \"Nimm den Schirm, sonst\".] Stopp."
        );
        assert!(to_message(&spoken, Some(""), "Stopp.").contains("before a word of it was heard.] Stopp."));
        assert!(
            to_wake_message(&to_stamp(NOW, None, None))
                .starts_with("[Friday 16 October 2026, 14:02] [You have just been switched on")
        );
    }

    #[test]
    fn heard_part_ends_at_passage() {
        let passages = [String::from("Ja."), String::from("Regen ab Mittag.")];
        assert_eq!(to_heard_part(&[], 0), None);
        assert_eq!(to_heard_part(&passages, 0).as_deref(), Some(""));
        assert_eq!(to_heard_part(&passages, 1).as_deref(), Some("Ja."));
        assert_eq!((to_heard_part(&passages, 2), to_heard_part(&passages, 5)), (None, None));
    }

    #[test]
    fn shown_photo_told() {
        let (sent, kept) = to_shown_messages("[Friday] Was ist das?");
        assert_eq!(sent, "[Friday] Was ist das? [With this message, a photo the user took with the device's camera.]");
        assert_eq!(kept, format!("{sent} [You saw it in that turn and no longer see it.]"));
    }
}
