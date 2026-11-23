//! What the reader's thread and the main loop share.

use super::reading::Reading;
use super::{ReadingSource, Script, Words};
use crate::base::handoff::Request;
use crate::domain::voice::Phase;

/// A reading the reader is wanted for.
pub struct ReadingRequest<'a> {
    pub round: u32,
    pub source: ReadingSource,
    pub is_held: bool,
    /// What it says; none for a reply, which comes by `State::set_reply`.
    pub words: Option<Words<'a>>,
}

type ReadingPhase = (u32, ReadingSource, Phase);

/// What the reader tells the main loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub text: Option<ReadingPhase>,
    pub reply: Option<ReadingPhase>,
    pub signal: Option<ReadingPhase>,
    /// How many passages of the reply of a number were heard to their end.
    pub heard: Option<(u32, usize)>,
}

/// What the reader's thread and the main loop tell each other.
#[derive(Default)]
pub struct State {
    pub request: Request<(u32, ReadingSource)>,
    script: Option<Script>,
    reply: Option<(u32, Script)>,
    heard: Option<(u32, usize)>,
    is_held: bool,
    /// A passage is on its way whose cost the service has not told.
    pub is_owing: bool,
    pub report: Report,
}

impl State {
    /// Wants that reading or none. True where the thread is told.
    pub fn set_wanted(&mut self, request: Option<ReadingRequest>) -> bool {
        let reading = request.as_ref().map(|request| (request.round, request.source));
        let is_held = request.as_ref().is_some_and(|request| request.is_held);
        let is_new = self.request.claim(reading);
        if is_new {
            self.script =
                request.and_then(|request| request.words).map(|words| Script::from_passages(words.to_passages()));
        }
        let has_changed = is_new || self.is_held != is_held;
        self.is_held = is_held;
        has_changed
    }

    /// Takes the reply of that number as far as it is written; true where the thread is told.
    pub fn set_reply(&mut self, round: u32, script: Script) -> bool {
        self.reply = Some((round, script));
        self.request.target == Some((round, ReadingSource::Reply))
    }

    fn script_of(&self, (round, source): (u32, ReadingSource)) -> Option<&Script> {
        match source {
            ReadingSource::Reply => self.reply.as_ref().filter(|reply| reply.0 == round).map(|reply| &reply.1),
            // The script kept is the wanted reading's: one it replaced says nothing more.
            ReadingSource::Text(_) | ReadingSource::Signal | ReadingSource::Web => {
                self.script.as_ref().filter(|_| self.is_wanted((round, source)))
            }
        }
    }

    /// True while that reading is wanted.
    pub fn is_wanted(&self, target: (u32, ReadingSource)) -> bool {
        self.request.target == Some(target)
    }

    /// Hands `reading` what it says so far; whether it is held while it is wanted, none once it is not.
    pub fn follow(&self, target: (u32, ReadingSource), reading: &mut Reading) -> Option<bool> {
        if let Some(script) = self.script_of(target) {
            reading.grow(script);
        }
        self.is_wanted(target).then_some(self.is_held)
    }

    /// True while no reading runs on the thread or waits to begin.
    pub const fn is_off(&self) -> bool {
        !self.request.is_busy
    }

    /// Tells the main loop how a reading stands; true if that is news.
    pub fn set_phase(&mut self, (round, source): (u32, ReadingSource), phase: Phase) -> bool {
        let entry = Some((round, source, phase));
        // Each slot is one taker's: a reading's last word is not written over by the first of another taker's.
        let slot = match source {
            ReadingSource::Text(_) | ReadingSource::Web => &mut self.report.text,
            ReadingSource::Reply => &mut self.report.reply,
            ReadingSource::Signal => &mut self.report.signal,
        };
        core::mem::replace(slot, entry) != entry
    }

    /// Tells the main loop how many passages of a reply were heard to their end; true if that is news.
    pub fn set_heard(&mut self, (round, source): (u32, ReadingSource), count: usize) -> bool {
        let heard = Some((round, count));
        let is_news = source == ReadingSource::Reply && self.heard != heard;
        if is_news {
            (self.heard, self.report.heard) = (heard, heard);
        }
        is_news
    }

    /// Notes whether a cost is owed; true once one that was is paid, since the network may go then.
    pub fn set_owing(&mut self, is_owing: bool) -> bool {
        core::mem::replace(&mut self.is_owing, is_owing) && !is_owing
    }

    /// The reading the thread is to hold next, with what it says.
    pub fn next_reading(&mut self) -> Option<((u32, ReadingSource), Script)> {
        let reading = self.request.next_new()?;
        Some((reading, self.script_of(reading)?.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::shelf::Shelf;
    use alloc::string::String;

    const TEXT: ReadingSource = ReadingSource::Text(Shelf::News);

    fn request(round: u32, is_held: bool, passages: &[String]) -> Option<ReadingRequest<'_>> {
        Some(ReadingRequest { round, source: TEXT, is_held, words: Some(Words::Arrived(passages)) })
    }

    #[test]
    fn wanted_once() {
        let (first, other) = ([String::from("One.")], [String::from("Two.")]);
        let mut state = State::default();
        assert!(state.is_off());
        assert!(state.set_wanted(request(1, false, &first)) && !state.set_wanted(request(1, false, &other)));
        assert_eq!(state.next_reading(), Some(((1, TEXT), Script::from_passages(first.to_vec()))));
        state.request.begin((1, TEXT));
        assert_eq!(state.next_reading(), None);
        assert!(state.set_wanted(request(1, true, &other)) && state.is_held);
        assert!(state.set_wanted(None));
    }

    #[test]
    fn reply_and_heard() {
        const REPLY: (u32, ReadingSource) = (2, ReadingSource::Reply);
        let mut state = State::default();
        assert!(state.set_wanted(Some(ReadingRequest { round: 2, source: REPLY.1, is_held: false, words: None })));
        assert_eq!(state.next_reading(), None, "it has not come");
        let script = Script::for_reply("Say it.".into());
        assert!(!state.set_reply(1, script.clone()) && state.next_reading().is_none(), "another reply");
        assert!(state.set_reply(2, script.clone()));
        assert_eq!(state.next_reading(), Some((REPLY, script)));
        assert!(state.set_heard(REPLY, 1) && !state.set_heard(REPLY, 1));
        assert!(!state.set_heard((2, TEXT), 2), "of a text nobody asks");
        assert_eq!(core::mem::take(&mut state.report).heard, Some((2, 1)));
    }

    #[test]
    fn text_and_reply_phases() {
        let mut state = State::default();
        assert!(state.set_phase((1, TEXT), Phase::Speaking) && !state.set_phase((1, TEXT), Phase::Speaking));
        assert!(state.set_phase((7, ReadingSource::Signal), Phase::Idle));
        assert!(state.set_phase((1, ReadingSource::Reply), Phase::Thinking));
        let report = core::mem::take(&mut state.report);
        assert_eq!(report.text, Some((1, TEXT, Phase::Speaking)));
        assert_eq!(report.reply, Some((1, ReadingSource::Reply, Phase::Thinking)));
        assert_eq!(
            report.signal,
            Some((7, ReadingSource::Signal, Phase::Idle)),
            "the end of Signal's reading is told though a reply's began before the report was taken"
        );
    }

    #[test]
    fn paid_cost_wakes() {
        let mut state = State::default();
        assert!(!state.set_owing(true) && state.is_owing);
        assert!(state.set_owing(false) && !state.set_owing(false));
    }

    #[test]
    fn follow_hold() {
        let (passages, mut reading) = ([String::from("One.")], Reading::default());
        let mut state = State::default();
        state.set_wanted(request(1, true, &passages));
        assert_eq!(state.follow((1, TEXT), &mut reading), Some(true));
        state.set_wanted(None);
        assert_eq!(state.follow((1, TEXT), &mut reading), None);
        let other = [String::from("Two."), String::from("Three.")];
        state.set_wanted(request(1, false, &passages));
        state.set_wanted(request(2, false, &other));
        assert_eq!(state.follow((1, TEXT), &mut reading), None);
        assert!(state.script_of((1, TEXT)).is_none(), "a reading another replaced takes none of the other's passages");
        assert_eq!(state.script_of((2, TEXT)), Some(&Script::from_passages(other.to_vec())));
    }
}
