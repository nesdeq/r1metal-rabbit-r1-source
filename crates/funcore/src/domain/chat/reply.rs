//! A turn's reply across its rounds with the model.

use super::calls::continues_turn;
use super::passage::Passages;
use super::tools::{Act, ToolCall};
use super::transcript::{Entry, EntryKind};
use crate::openai::chat::answer::{Answer, Call, Event};
use crate::openai::output::strip_citations;
use alloc::string::String;
use alloc::vec::Vec;

/// The effect of an event of the model's answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Update {
    // The entry of the round's text so far.
    Text(Entry),
    Search,
    Nothing,
}

/// A turn's text, passages and acts so far.
pub struct Reply {
    has_voice: bool,
    text: String,
    round_text: String,
    time: String,
    passages: Passages,
    acts: Vec<Act>,
    is_first: bool,
    needs_answer: bool,
    is_folded: bool,
}

impl Reply {
    /// An empty reply, read by the voice where `has_voice`.
    pub fn new(has_voice: bool) -> Reply {
        Reply {
            has_voice,
            text: String::new(),
            round_text: String::new(),
            time: String::new(),
            passages: Passages::default(),
            acts: Vec::new(),
            is_first: true,
            needs_answer: false,
            is_folded: false,
        }
    }

    /// Begins a round of the model at the time of day `time`.
    pub fn begin_round(&mut self, time: String) {
        self.round_text.clear();
        self.time = time;
    }

    /// True in the turn's first round.
    pub const fn is_first(&self) -> bool {
        self.is_first
    }

    pub fn take_event(&mut self, event: Event) -> Update {
        match event {
            Event::Text(delta) => {
                self.round_text.push_str(&delta);
                self.text.push_str(&delta);
                let text = strip_citations(&self.round_text);
                Update::Text(Entry { kind: EntryKind::Robot, time: self.time.clone(), text })
            }
            Event::Searching => Update::Search,
            _ => Update::Nothing,
        }
    }

    /// The voice's passages since the last call, through the reply's end where `is_last`.
    pub fn take_passages(&mut self, is_last: bool) -> Vec<String> {
        if !self.has_voice {
            return Vec::new();
        }
        let text = strip_citations(&self.text);
        core::iter::from_fn(|| self.passages.next(&text, is_last)).collect()
    }

    /// Adds an act for the end of the turn.
    pub fn add_act(&mut self, act: Act) {
        self.acts.push(act);
    }

    /// Notes an answered call: the model answers the output of one it waits for, and of one that was none of its functions.
    pub fn note_call(&mut self, call: Option<&ToolCall>) {
        self.needs_answer |= call.is_none_or(ToolCall::needs_answer);
    }

    /// Notes a stored item, which folded the thread where `is_folded`.
    pub fn note_item(&mut self, is_folded: bool) {
        self.is_folded |= is_folded;
    }

    /// True once after a fold.
    pub const fn take_folded(&mut self) -> bool {
        core::mem::replace(&mut self.is_folded, false)
    }

    /// Takes an answer and its handled calls; true where the model is asked again.
    pub fn next_round(&mut self, answer: &Answer, calls: &[Call]) -> bool {
        let is_continued = continues_turn(answer, calls, self.needs_answer);
        (self.is_first, self.needs_answer) = (false, false);
        if is_continued {
            self.text.push('\n');
        }
        is_continued
    }

    /// The acts of the ended turn.
    pub fn into_acts(self) -> Vec<Act> {
        self.acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::tools::MemoryCall;
    use alloc::vec;

    fn answer() -> Answer {
        Answer::default()
    }

    fn call() -> Call {
        Call { name: "x".into(), id: "1".into(), arguments: "{}".into() }
    }

    #[test]
    fn rounds_and_passages() {
        let mut reply = Reply::new(true);
        reply.begin_round("12:00".into());
        let entry = |text: &str| Entry { kind: EntryKind::Robot, time: "12:00".into(), text: text.into() };
        assert_eq!(reply.take_event(Event::Text("Ja. ".into())), Update::Text(entry("Ja. ")));
        assert_eq!(reply.take_event(Event::Text("Gut".into())), Update::Text(entry("Ja. Gut")));
        assert_eq!(reply.take_event(Event::Searching), Update::Search);
        assert_eq!(reply.take_event(Event::Other), Update::Nothing);
        assert_eq!(reply.take_passages(false), ["Ja."]);
        assert!(reply.next_round(&answer(), &[call()]), "calls and no text: the model is asked again");
        reply.begin_round("12:01".into());
        let Update::Text(shown) = reply.take_event(Event::Text("Mehr".into())) else { panic!() };
        assert_eq!(shown.text, "Mehr", "the entry shows its round alone");
        assert_eq!(reply.take_passages(true), ["Gut", "Mehr"], "the voice says the rounds apart");
    }

    #[test]
    fn call_continues_turn() {
        let mut reply = Reply::new(false);
        assert!(reply.is_first());
        assert!(!reply.next_round(&answer(), &[]) && !reply.is_first());
        reply.note_call(Some(&ToolCall::Memory(MemoryCall::Tone(Vec::new()))));
        assert!(!reply.next_round(&answer(), &[]), "a call the model waits for no answer of");
        reply.note_call(Some(&ToolCall::Search(String::new())));
        assert!(reply.next_round(&answer(), &[]));
        assert!(!reply.next_round(&answer(), &[]), "each round counts its own calls");
        reply.note_call(None);
        assert!(reply.next_round(&answer(), &[]), "a call of no function of its own is answered so");
        assert!(reply.take_passages(true).is_empty(), "a reply without its voice");
    }

    #[test]
    fn fold_once_and_acts() {
        let mut reply = Reply::new(false);
        reply.note_item(true);
        reply.note_item(false);
        assert!(reply.take_folded() && !reply.take_folded());
        reply.add_act(Act::Home);
        assert_eq!(reply.into_acts(), vec![Act::Home]);
    }
}
