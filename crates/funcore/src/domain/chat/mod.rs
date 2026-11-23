//! The robot's conversation: its days, memory, turns and functions.

pub mod calls;
pub mod instructions;
pub mod memory;
pub mod message;
pub mod onboarding;
mod passage;
pub mod read;
pub mod recall;
pub mod recollect;
pub mod reply;
pub mod state;
pub mod thread;
pub mod tools;
pub mod transcript;

use crate::domain::memos::Memo;
use crate::hw::sound::playback::{MemoryFile, Playback};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use memory::{Memory, split_day};
use state::Lapse;

const SEARCH_SOUND: &[u8] = include_bytes!("../../../assets/chat/searching.m4a");
const RECALL_DAYS: usize = 7;
pub(crate) const MAX_TEXT_CHARS: usize = 500;
/// The answer of the function that ends the first conversation.
pub const ONBOARDING_ANSWER: &str = "The first conversation is over. From now on, talk as you always will.";
pub const CORE_FILE: &str = "core.md";
pub const KNOWLEDGE_FILE: &str = "knowledge.md";
pub const STATE_FILE: &str = "state";
pub const THREAD_FILE: &str = "thread";
/// The log line when no partition holds the conversation.
pub const NO_PARTITION_LINE: &str = "chat: no data partition to keep the conversation on";
/// The log line of a recording the partition has no room for.
pub const NEARLY_FULL_LINE: &str = "chat: no recording: the data partition is nearly full";
/// The log line of a first conversation's start.
pub const WAKE_LINE: &str = "chat: the robot wakes for its first conversation";
/// The log line of a first conversation's end.
pub const MET_LINE: &str = "chat: the first conversation is over";
/// The log line of a thread file rewritten.
pub const THREAD_MENDED_LINE: &str = "chat: the thread's file held what is no item, and is written anew";

/// The input a message came by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    Voice,
    Text,
}

/// The day the screen opens on: today where it has words, else the last of `days`, oldest first.
pub fn start_day<'a>(days: &'a [String], today: &str) -> Option<&'a String> {
    days.iter().find(|day| *day == today).or(days.last())
}

/// The last of `days`, oldest first, before `day`.
pub fn previous_day(days: Vec<String>, day: &str) -> Option<String> {
    days.into_iter().rfind(|known| known.as_str() < day)
}

/// The sound played during a web search.
pub fn search_sound() -> Option<Playback<MemoryFile<'static>>> {
    Playback::new(MemoryFile::parse(SEARCH_SOUND).ok()?)
}

/// The log line of the search sound.
pub const fn to_search_line(has_sound: bool) -> &'static str {
    if has_sound { "sound: the robot's searching sounds" } else { "sound: the robot's searching is silent" }
}

/// The facts as their list shows them: each by the line its file keeps, its text without the day it names.
pub fn to_fact_entries(memory: &Memory) -> Vec<Memo> {
    let to_entry = |stem: String| {
        let text = split_day(stem.clone()).1;
        Memo { stem, seconds: 0, words: None, summary: Some(text), rundowns: Vec::new() }
    };
    memory.facts().into_iter().map(to_entry).collect()
}

/// The days of the conversation, oldest first, in the order their list shows them: the newest first.
pub fn to_listed_days(days: Vec<String>) -> Vec<String> {
    days.into_iter().rev().collect()
}

/// A day of the conversation as its list shows it: its recollection, and its words as read.
pub fn to_day_entry(day: String, recollection: Option<String>, words: String) -> Memo {
    let words = Some(words).filter(|words| !words.trim().is_empty());
    Memo { stem: day, seconds: 0, words, summary: recollection, rundowns: Vec::new() }
}

/// The log line of what was forgotten, which names no fact.
pub fn to_forgotten_line(lapse: &Lapse) -> String {
    match lapse {
        Lapse::Fact(_) => String::from("chat: a fact is forgotten"),
        Lapse::Day(day) => format!("chat: {day} is forgotten, its words and its recollection"),
        Lapse::Thread => String::from("chat: the thread is forgotten"),
    }
}

/// The days before `today` whose recollections go into every turn's instructions.
pub fn recall_days(days: &[String], today: &str) -> Vec<String> {
    let earlier: Vec<String> = days.iter().filter(|day| day.as_str() < today).cloned().collect();
    let skip_count = earlier.len().saturating_sub(RECALL_DAYS);
    earlier.into_iter().skip(skip_count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    #[test]
    fn opens_on_today_or_latest() {
        let days = vec![String::from("20261015"), String::from("20261016")];
        assert_eq!((start_day(&days, "20261016"), start_day(&days, "20261020")), (Some(&days[1]), Some(&days[1])));
        assert_eq!(start_day(&days, "20261015"), Some(&days[0]));
        assert_eq!(start_day(&[], "20261016"), None);
        assert_eq!(previous_day(days.clone(), "20261016"), Some("20261015".into()));
        assert_eq!(previous_day(days.clone(), "20261015"), None);
        assert_eq!(to_listed_days(days), ["20261016", "20261015"], "listed, the newest first");
    }

    #[test]
    fn last_seven_days() {
        let days: Vec<String> = (1..=9).map(|day| format!("2026100{day}")).collect();
        let recalled = recall_days(&days, "20261009");
        assert_eq!(recalled.len(), 7);
        assert_eq!((recalled[0].as_str(), recalled[6].as_str()), ("20261002", "20261008"));
        assert_eq!(recall_days(&days, "20261001"), Vec::<String>::new());
    }

    #[test]
    fn search_sound_parses() {
        assert!(search_sound().is_some());
        assert_eq!(
            [true, false].map(to_search_line),
            ["sound: the robot's searching sounds", "sound: the robot's searching is silent"]
        );
    }

    #[test]
    fn robots_entries() {
        let mut memory = Memory::default();
        memory.save("Lives in Vienna.", true, None, "2026-10-16");
        memory.save("Likes tea.", false, None, "2026-10-16");
        let facts = to_fact_entries(&memory);
        assert_eq!(
            facts.iter().map(|fact| (fact.stem.as_str(), fact.summary.as_deref())).collect::<Vec<_>>(),
            [("Lives in Vienna.", Some("Lives in Vienna.")), ("2026-10-16: Likes tea.", Some("Likes tea."))]
        );
        let day = to_day_entry("20261016".into(), None, " ".into());
        assert_eq!((day.summary, day.words), (None, None), "a day of no words has no conversation to read");
        assert_eq!(to_forgotten_line(&Lapse::Fact("Likes tea.".into())), "chat: a fact is forgotten");
        assert_eq!(
            to_forgotten_line(&Lapse::Day("20261016".into())),
            "chat: 20261016 is forgotten, its words and its recollection"
        );
        assert_eq!(to_forgotten_line(&Lapse::Thread), "chat: the thread is forgotten");
    }
}
