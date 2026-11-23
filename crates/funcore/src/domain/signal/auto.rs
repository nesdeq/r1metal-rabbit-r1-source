//! Auto mode: the robot answers a conversation for the owner, each reply marked.

use crate::base::text::has_words;
use crate::base::time::{MILLIS_PER_SECOND, SECONDS_PER_HOUR};
use crate::domain::settings::Settings;
use crate::openai::models::{Task, Thinking};
use crate::openai::summary;
use crate::signal::address::ServiceId;
use crate::signal::conversation::Entry;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub(super) const MARK: &str = "\u{1F916} ";
pub(super) const SETTLE_MS: u64 = 5_000;
pub(super) const CONTEXT_MESSAGES: usize = 50;
const HOUR_MS: u64 = SECONDS_PER_HOUR * MILLIS_PER_SECOND;
const TASK: &str = include_str!("../../../assets/signal/auto.txt");

/// Auto mode's limits, and the model that writes its replies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    replies: u32,
    gap_ms: u64,
    per_hour: usize,
    thinking: Thinking,
}

impl From<&Settings> for Limits {
    fn from(settings: &Settings) -> Limits {
        Limits {
            replies: settings.auto_replies.into(),
            gap_ms: u64::from(settings.auto_gap) * MILLIS_PER_SECOND,
            per_hour: settings.auto_hourly.into(),
            thinking: settings.terms.thinking(Task::Auto),
        }
    }
}

impl Default for Limits {
    fn default() -> Limits {
        Limits::from(&Settings::default())
    }
}

// An entry's words without auto mode's mark, which a reply of auto mode's alone carries.
pub(super) fn unmarked(entry: &Entry) -> &str {
    entry.body.strip_prefix(MARK).filter(|_| entry.is_robot).unwrap_or(&entry.body)
}

// The messages a turn is given of a conversation's: its last that were not deleted, the oldest first.
pub(super) fn recent(entries: &[Entry]) -> Vec<&Entry> {
    let kept = entries.iter().rev().filter(|entry| entry.deleter.is_none());
    let mut recent: Vec<&Entry> = kept.take(CONTEXT_MESSAGES).collect();
    recent.reverse();
    recent
}

// The request of a turn given `recent`, the conversation's messages as `recent` picks them.
pub(super) fn to_request(thinking: Thinking, name: Option<&str>, recent: &[Entry]) -> String {
    let contact = name.map_or_else(|| String::from("Contact"), |name| format!("Contact ({name})"));
    let input = recent.iter().fold(String::new(), |mut text, entry| {
        let author = match (entry.is_owner, entry.is_robot) {
            (false, _) => contact.as_str(),
            (true, false) => "Owner",
            (true, true) => "You, for the owner",
        };
        let _ = writeln!(text, "{author}: {}", unmarked(entry));
        text
    });
    summary::body(thinking, TASK.trim(), &input, "")
}

pub(super) fn to_reply(text: &str) -> Option<String> {
    has_words(text).then(|| format!("{MARK}{}", text.trim()))
}

#[derive(Default)]
pub(super) struct Auto {
    pub(super) contact: Option<ServiceId>,
    due_ms: Option<u64>,
    is_asking: bool,
    reply_count: u32,
    sent_ms: VecDeque<u64>,
    is_owed: bool,
    limits: Limits,
}

impl Auto {
    pub(super) const fn thinking(&self) -> Thinking {
        self.limits.thinking
    }

    pub(super) fn set_limits(&mut self, limits: Limits) -> bool {
        let is_new = self.limits != limits;
        self.limits = limits;
        if self.reply_count >= limits.replies {
            self.set(None);
        }
        is_new
    }

    pub(super) fn set(&mut self, contact: Option<ServiceId>) {
        if contact != self.contact {
            self.contact = contact;
            self.due_ms = None;
            self.reply_count = 0;
            self.is_owed = false;
        }
    }

    pub(super) fn note_message(&mut self, sender: ServiceId, now_ms: u64) {
        if self.contact == Some(sender) {
            self.due_ms = Some(now_ms + SETTLE_MS);
        }
    }

    pub(super) fn note_owner_wrote(&mut self, contact: ServiceId) {
        if self.contact == Some(contact) {
            self.set(None);
        }
    }

    pub(super) fn next_turn_ms(&self) -> Option<u64> {
        let due = self.due_ms.filter(|_| !self.is_asking && self.contact.is_some())?;
        let after_gap = self.sent_ms.back().map_or(0, |last| last + self.limits.gap_ms);
        let hourly_index = self.sent_ms.len().checked_sub(self.limits.per_hour);
        let after_hour = hourly_index.map_or(0, |index| self.sent_ms[index] + HOUR_MS);
        Some(due.max(after_gap).max(after_hour))
    }

    pub(super) fn begin_turn(&mut self, now_ms: u64) -> Option<ServiceId> {
        let contact = self.contact.filter(|_| self.next_turn_ms().is_some_and(|at| at <= now_ms))?;
        self.due_ms = None;
        self.is_asking = true;
        Some(contact)
    }

    pub(super) fn note_answered(&mut self, now_ms: u64) {
        self.is_asking = false;
        self.reply_count += 1;
        self.sent_ms.push_back(now_ms);
        self.sent_ms.retain(|at| at + HOUR_MS > now_ms);
        if self.reply_count >= self.limits.replies {
            self.set(None);
        }
    }

    pub(super) fn note_failed(&mut self) {
        self.is_asking = false;
        self.set(None);
    }

    pub(super) fn defer(&mut self) {
        self.is_owed |= self.due_ms.take().is_some();
    }

    pub(super) fn resume(&mut self) {
        if core::mem::take(&mut self.is_owed) && self.contact.is_some() {
            self.due_ms = Some(0);
        }
    }

    pub(super) fn drop_turn(&mut self) {
        self.is_asking = false;
    }

    pub(super) fn is_pending(&self) -> bool {
        self.contact.is_some() && (self.is_asking || self.due_ms.is_some_and(|due| self.next_turn_ms() == Some(due)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::conversation::Deleter;
    use crate::signal::conversation::tests::entry;

    const ANNA: ServiceId = ServiceId::Aci([1; 16]);
    const BORIS: ServiceId = ServiceId::Aci([2; 16]);

    #[test]
    fn burst_gets_one_turn() {
        let mut auto = Auto::default();
        auto.note_message(ANNA, 0);
        assert_eq!(auto.next_turn_ms(), None, "off, nothing is due");
        auto.set(Some(ANNA));
        auto.note_message(BORIS, 0);
        assert!(!auto.is_pending(), "another conversation's message");
        auto.note_message(ANNA, 1_000);
        auto.note_message(ANNA, 3_000);
        assert_eq!(auto.next_turn_ms(), Some(3_000 + SETTLE_MS), "the last of the burst counts");
        assert_eq!(auto.begin_turn(7_999), None);
        assert_eq!(auto.begin_turn(8_000), Some(ANNA));
        assert!(auto.is_pending() && auto.begin_turn(9_000).is_none(), "at work, and nothing more is due");
        auto.note_answered(20_000);
        assert!(!auto.is_pending());
    }

    #[test]
    fn reply_pace_and_limit() {
        let mut auto = Auto::default();
        let Limits { replies: limit, gap_ms: gap, .. } = Limits::default();
        auto.set(Some(ANNA));
        let mut now = 0;
        for reply in 0..limit {
            auto.note_message(ANNA, now);
            let at = auto.next_turn_ms().expect("due");
            assert!(reply == 0 || at >= now.max(auto.sent_ms.back().unwrap() + gap), "a gap after the last");
            assert_eq!(auto.begin_turn(at), Some(ANNA));
            auto.note_answered(at);
            now = at + 1;
        }
        assert_eq!(auto.contact, None, "switched off after {limit} replies");
        auto.set(Some(ANNA));
        auto.note_message(ANNA, now);
        assert_eq!(
            auto.next_turn_ms(),
            Some((now + SETTLE_MS).max(auto.sent_ms.back().unwrap() + gap)),
            "on anew, it answers again"
        );
    }

    #[test]
    fn limits_from_settings() {
        use crate::openai::models::{Effort, Model};
        assert_eq!(
            Limits::from(&Settings::parse(
                "auto_replies 50\nauto_gap 120\nauto_hourly 10\nmodel auto gpt-6.1-terra medium\n"
            )),
            Limits {
                replies: 50,
                gap_ms: 120_000,
                per_hour: 10,
                thinking: Thinking::new(Model::Terra, Effort::Medium)
            },
            "auto mode's limits and model, as Signal's settings set them"
        );
        assert_eq!(
            Limits::from(&Settings::parse("auto_replies 7\nauto_gap 1\nauto_hourly many\n")),
            Limits::default(),
            "a line that names none of their steps leaves them as a new device has them"
        );
    }

    #[test]
    fn new_limits_apply() {
        let mut auto = Auto::default();
        let limits = Limits { replies: 5, gap_ms: 60_000, per_hour: 2, ..Limits::default() };
        assert!(auto.set_limits(limits) && !auto.set_limits(limits), "taken once");
        auto.set(Some(ANNA));
        for at in [0, 100_000] {
            auto.note_message(ANNA, at);
            let due = auto.next_turn_ms().expect("due");
            auto.begin_turn(due);
            auto.note_answered(due);
        }
        auto.note_message(ANNA, 200_000);
        assert_eq!(
            auto.next_turn_ms(),
            Some(5_000 + HOUR_MS),
            "two an hour: the third waits until the first is an hour old"
        );
        assert!(
            auto.set_limits(Limits { replies: 2, ..limits }) && auto.contact.is_none(),
            "two replies given, two the most"
        );
    }

    #[test]
    fn owner_or_failure_ends_auto() {
        let mut auto = Auto::default();
        auto.set(Some(ANNA));
        auto.note_owner_wrote(BORIS);
        assert_eq!(auto.contact, Some(ANNA));
        auto.note_owner_wrote(ANNA);
        assert_eq!(auto.contact, None);
        auto.set(Some(ANNA));
        auto.note_message(ANNA, 0);
        auto.begin_turn(SETTLE_MS);
        auto.note_failed();
        assert!(auto.contact.is_none() && !auto.is_pending());
    }

    #[test]
    fn request_and_reply() {
        let mut entries: Vec<Entry> = (0..60).map(|at| entry(1, at % 2 == 1, &format!("m{at}"))).collect();
        entries.push(Entry { is_robot: true, ..entry(1, true, &format!("{MARK}On my way")) });
        entries.push(entry(1, false, "Lunch \"today\"?"));
        entries.push(Entry { deleter: Some(Deleter::Author), ..entry(1, false, "gone") });
        let recent: Vec<Entry> = recent(&entries).into_iter().cloned().collect();
        assert_eq!(recent.len(), CONTEXT_MESSAGES, "one deleted for everyone is none of them");
        let request = to_request(Limits::default().thinking, Some("Anna"), &recent);
        assert!(request.contains("\"model\":\"gpt-6.1-terra\"") && request.contains("\"effort\":\"high\""));
        assert!(!request.contains("m11\\n") && request.contains("Owner: m13\\n"), "the last {CONTEXT_MESSAGES} alone");
        assert!(request.contains("You, for the owner: On my way\\nContact (Anna): Lunch \\\"today\\\"?\\n"));
        let marked = format!("{MARK}beep");
        assert_eq!(
            unmarked(&entry(1, false, &marked)),
            marked,
            "a contact's own words keep what looks like auto mode's mark"
        );
        assert_eq!(to_reply("  See you!\n"), Some(format!("{MARK}See you!")));
        assert_eq!(to_reply(" \n"), None);
    }
}
