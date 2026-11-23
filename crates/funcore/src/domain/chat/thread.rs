//! The conversation's stored seed, switches and thread of items.

use super::onboarding::{Plan, SEED_BYTES};
use crate::base::code::{hex, unhex};
use crate::base::json::value_at;
use crate::openai::chat::answer::is_compaction;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const WORD_ONBOARDED: &str = "onboarded";
const WORD_MUTED: &str = "muted";

/// The conversation's stored switches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RobotState {
    seed: [u8; SEED_BYTES],
    pub is_onboarded: bool,
    pub is_muted: bool,
}

impl RobotState {
    /// A conversation never held, its plan drawn from those random bytes.
    pub const fn new(seed: [u8; SEED_BYTES]) -> RobotState {
        RobotState { seed, is_onboarded: false, is_muted: false }
    }

    /// The state's file text.
    pub fn to_text(&self) -> String {
        let mut text = format!("{}\n", hex(&self.seed));
        for (is_on, word) in [(self.is_onboarded, WORD_ONBOARDED), (self.is_muted, WORD_MUTED)] {
            if is_on {
                text.push_str(word);
                text.push('\n');
            }
        }
        text
    }

    /// Parses the file text; none without a whole seed.
    pub fn parse(text: &str) -> Option<RobotState> {
        let mut lines = text.lines().map(str::trim);
        let seed = unhex(lines.next()?)?.try_into().ok()?;
        let words: Vec<&str> = lines.collect();
        Some(RobotState { seed, is_onboarded: words.contains(&WORD_ONBOARDED), is_muted: words.contains(&WORD_MUTED) })
    }

    /// The first conversation's plan while it goes on.
    pub fn plan(&self) -> Option<Plan> {
        (!self.is_onboarded).then(|| Plan::draw(&self.seed))
    }
}

/// The items sent to the service every turn: from the newest compaction item on.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Thread {
    // The items, oldest first, one line each.
    pub(crate) items: Vec<String>,
}

/// The change of the thread's file after a new item.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileChange {
    // The whole file: the item folded all before it.
    Rewrite(String),
    // The item's line for the file's end.
    Line(String),
}

impl FileChange {
    /// True where the item folded all before it.
    pub fn is_folded(&self) -> bool {
        matches!(self, FileChange::Rewrite(_))
    }
}

/// A message of a turn that goes with a photo: its place in the thread, which keeps it without, and as the turn's requests send it.
pub struct Shown {
    at: usize,
    kept: String,
    sent: String,
}

fn is_item(line: &str) -> bool {
    line.starts_with('{') && value_at(line, &[]).is_some_and(|value| value.len() == line.len())
}

fn to_line(item: &str) -> String {
    format!("{item}\n")
}

impl Thread {
    /// Parses the file, skipping a line a cut write left, so it costs its own item and no later turn.
    pub fn parse(file: &str) -> Thread {
        let mut thread = Thread::default();
        for line in file.lines().map(str::trim).filter(|line| is_item(line)) {
            thread.push(line.into());
        }
        thread
    }

    /// Adds an item and returns the file's change.
    pub fn add(&mut self, item: String) -> FileChange {
        let line = to_line(&item);
        if self.push(item) { FileChange::Rewrite(self.to_text()) } else { FileChange::Line(line) }
    }

    fn push(&mut self, item: String) -> bool {
        let is_fold = is_compaction(&item);
        if is_fold {
            self.items.clear();
        }
        self.items.push(item);
        is_fold
    }

    /// Adds a message as it is `kept`, without its photo; beside the file's change, how the turn's requests send it.
    pub fn add_shown(&mut self, kept: String, sent: String) -> (FileChange, Shown) {
        let change = self.add(kept.clone());
        (change, Shown { at: self.items.len() - 1, kept, sent })
    }

    /// The items as a request of the turn sends them: each of `shown` in the place of the item it is kept as, while that item stands where it was added.
    pub fn to_sent<'a>(&'a self, shown: &'a [Shown]) -> Vec<&'a str> {
        let sent_at = |at: usize, item: &String| shown.iter().find(|shown| shown.at == at && shown.kept == *item);
        let items = self.items.iter().enumerate();
        items.map(|(at, item)| sent_at(at, item).map_or(item.as_str(), |shown| shown.sent.as_str())).collect()
    }

    /// Lets every item go and returns the file's change.
    pub fn clear(&mut self) -> FileChange {
        self.items.clear();
        FileChange::Rewrite(self.to_text())
    }

    /// The text of the thread's file.
    pub fn to_text(&self) -> String {
        self.items.iter().map(|item| to_line(item)).collect()
    }

    /// True where the file it was parsed from holds more than items.
    pub fn needs_rewrite(&self, file: &str) -> bool {
        self.to_text() != file
    }
}

/// True for a conversation never held.
pub fn is_never_held(state: Option<&RobotState>, thread: &Thread) -> bool {
    state.is_some_and(|state| state.plan().is_some()) && thread.items.is_empty()
}

pub(super) fn to_log_line(thread: &Thread, fact_count: usize, state: Option<&RobotState>) -> String {
    let has_met = !thread.items.is_empty() || state.is_some_and(|state| state.is_onboarded);
    let status = if has_met { "met before" } else { "never met" };
    format!("{} items of the thread, {fact_count} facts learned, {status}", thread.items.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn state_round_trip() {
        let mut state = RobotState::new([7; SEED_BYTES]);
        assert_eq!(state.to_text(), "07070707070707070707070707070707\n");
        assert_eq!(RobotState::parse(&state.to_text()), Some(state));
        state.is_onboarded = true;
        state.is_muted = true;
        assert_eq!(state.to_text(), "07070707070707070707070707070707\nonboarded\nmuted\n");
        assert_eq!(RobotState::parse(&state.to_text()), Some(state));
        assert!(state.plan().is_none());
        assert_eq!((RobotState::parse(""), RobotState::parse("0707\n")), (None, None));
    }

    #[test]
    fn compaction_and_cut_line() {
        let mut thread = Thread::default();
        assert_eq!(thread.add(r#"{"type":"message"}"#.into()), FileChange::Line("{\"type\":\"message\"}\n".into()));
        let change = thread.add(r#"{"type":"compaction"}"#.into());
        assert!(change.is_folded());
        assert_eq!(thread.items, vec![r#"{"type":"compaction"}"#]);
        let file = format!("{}{{\"type\":\"mess", thread.to_text());
        let parsed = Thread::parse(&file);
        assert_eq!(parsed.items.len(), 1);
        assert!(parsed.needs_rewrite(&file) && !parsed.needs_rewrite(&thread.to_text()));
    }

    #[test]
    fn shown_by_place() {
        let mut thread = Thread::default();
        thread.add(r#"{"kept":1}"#.into());
        let (change, first) = thread.add_shown(r#"{"kept":1}"#.into(), r#"{"photo":1}"#.into());
        assert_eq!(change, FileChange::Line("{\"kept\":1}\n".into()), "the file keeps it without its photo");
        let (_, second) = thread.add_shown(r#"{"kept":1}"#.into(), r#"{"photo":2}"#.into());
        let shown = [first, second];
        assert_eq!(
            thread.to_sent(&shown),
            [r#"{"kept":1}"#, r#"{"photo":1}"#, r#"{"photo":2}"#],
            "messages of the same words go each with its own photo, an earlier one of those words with none"
        );
        thread.add(r#"{"type":"compaction"}"#.into());
        thread.add(r#"{"type":"message"}"#.into());
        assert_eq!(
            thread.to_sent(&shown),
            [r#"{"type":"compaction"}"#, r#"{"type":"message"}"#],
            "folded away, they are sent no more"
        );
        assert_eq!((thread.clear(), thread.items.len()), (FileChange::Rewrite(String::new()), 0), "an empty file");
    }

    #[test]
    fn never_held_and_log_text() {
        let thread = Thread { items: vec!["{}".into()] };
        let state = RobotState::new([1; SEED_BYTES]);
        assert!(is_never_held(Some(&state), &Thread::default()));
        assert!(!is_never_held(Some(&state), &thread) && !is_never_held(None, &Thread::default()));
        assert_eq!(to_log_line(&thread, 2, None), "1 items of the thread, 2 facts learned, met before");
        assert_eq!(
            to_log_line(&Thread::default(), 0, Some(&state)),
            "0 items of the thread, 0 facts learned, never met"
        );
    }
}
