//! State shared by the translator's thread and the main loop.

use crate::base::handoff::Request;
use crate::domain::cause::Cause;
use crate::domain::dialogue::{Dialogue, Turn};
use crate::domain::voice::Phase;
use crate::openai::fault::Ending;
use alloc::string::String;
use alloc::vec::Vec;

/// The translator's report for its screen.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    /// A session's number and phase.
    pub session: Option<(u32, Phase)>,
    /// Conversations stored.
    pub kept_count: u32,
    /// A session's number and conversation, where it changed.
    pub remarks: Option<(u32, Vec<Turn>)>,
    /// The stems of the conversations just stored, for the scribe.
    pub new_stems: Vec<String>,
}

/// State shared by the translator's thread and the main loop.
#[derive(Default)]
pub struct State {
    pub request: Request<u32>,
    dialogue: Dialogue,
    pub report: Report,
    has_new_remarks: bool,
}

impl State {
    /// The conversation of the session that runs or ran last.
    pub const fn dialogue(&self) -> &Dialogue {
        &self.dialogue
    }

    /// Begins the session of that number, with a new conversation.
    pub fn begin(&mut self, round: u32) {
        self.request.begin(round);
        self.dialogue = Dialogue::default();
        self.has_new_remarks = false;
    }

    /// True while no session runs on the thread or waits to begin.
    pub const fn is_off(&self) -> bool {
        !self.request.is_busy
    }

    /// True while the screen wants the session of that number.
    pub fn is_wanted(&self, round: u32) -> bool {
        self.request.target == Some(round)
    }

    /// The phase of a session that ended so; one the screen let go of is no failure.
    pub fn to_phase(&self, round: u32, ending: &Ending) -> Phase {
        Phase::from_ending(Cause::from_ending(ending), self.is_wanted(round))
    }

    /// Sets a session's phase for the main loop; true if that is news.
    pub fn set_session(&mut self, round: u32, phase: Phase) -> bool {
        let session = Some((round, phase));
        core::mem::replace(&mut self.report.session, session) != session
    }

    pub fn add_source(&mut self, piece: &str, start_ms: Option<u64>) {
        self.dialogue.add_source(piece, start_ms);
        self.has_new_remarks = true;
    }

    pub fn add_translation(&mut self, piece: &str, sent_ms: u64) {
        self.dialogue.add_translation(piece, sent_ms);
        self.has_new_remarks = true;
    }

    /// Notes a conversation stored under `stem`.
    pub fn note_kept(&mut self, stem: String) {
        self.report.kept_count += 1;
        self.report.new_stems.push(stem);
    }

    /// Takes the report, with the conversation where it changed.
    pub fn take_report(&mut self) -> Report {
        let is_new = core::mem::take(&mut self.has_new_remarks);
        let remarks = self.request.current.filter(|_| is_new).map(|round| (round, self.dialogue.turns.clone()));
        Report { remarks, ..core::mem::take(&mut self.report) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_request() {
        let mut state = State::default();
        assert!(state.is_off());
        assert!(state.request.claim(Some(4)) && !state.request.claim(Some(4)));
        assert!(!state.is_off() && state.request.target == Some(4));
        assert_eq!(state.request.next_new(), Some(4));
        state.begin(4);
        assert_eq!((state.request.next_new(), state.is_off()), (None, true));
        assert!(state.request.claim(None) && state.is_off());
        assert!(state.request.claim(Some(4)) && !state.is_off() && state.request.next_new().is_none());
        assert!(state.request.claim(Some(5)) && state.request.next_new() == Some(5));
    }

    #[test]
    fn released_session_no_failure() {
        let mut state = State::default();
        state.request.claim(Some(4));
        assert!(state.is_wanted(4) && !state.is_wanted(5));
        let ending = Ending::Failed(crate::openai::fault::Fault::Temporary("gone".into()));
        assert_ne!(state.to_phase(4, &ending), state.to_phase(5, &ending));
        assert_eq!(state.to_phase(5, &ending), state.to_phase(5, &Ending::Done));
    }

    #[test]
    fn session_report() {
        let mut state = State::default();
        state.begin(7);
        assert!(state.set_session(7, Phase::Listening) && !state.set_session(7, Phase::Listening));
        state.add_source("Wo ist ", Some(10));
        state.add_translation("Where is", 20);
        state.note_kept("t".into());
        assert_eq!(
            state.take_report(),
            Report {
                session: Some((7, Phase::Listening)),
                kept_count: 1,
                remarks: Some((7, state.dialogue.turns.clone())),
                new_stems: alloc::vec![String::from("t")]
            }
        );
        assert_eq!(state.take_report(), Report::default());
        assert!(state.dialogue.has_speech());
        state.begin(8);
        assert!(state.dialogue.turns.is_empty());
    }
}
