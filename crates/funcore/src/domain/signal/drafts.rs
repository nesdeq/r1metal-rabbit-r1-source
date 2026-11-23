//! A voice message written down into a draft, or kept as a voice note.

use crate::base::text::has_words;
use crate::domain::cause::Cause;
use crate::domain::voice::{Job, Keep, Phase, Query, Voice};
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::Infallible;

#[derive(Clone, PartialEq, Eq)]
pub struct Recording {
    pub sound: Vec<u8>,
    pub seconds: u64,
}

impl core::fmt::Debug for Recording {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Said").field("seconds", &self.seconds).finish_non_exhaustive()
    }
}

/// The drafts' report for the main loop.
#[derive(Clone, PartialEq, Eq)]
pub struct Report {
    pub phase: Phase,
    pub draft: Option<String>,
    /// The voice note said, where messages said are kept as voice notes.
    pub note: Option<Recording>,
}

impl core::fmt::Debug for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Report")
            .field("phase", &self.phase)
            .field("draft", &self.draft.is_some())
            .field("note", &self.note.as_ref().map(|note| note.seconds))
            .finish()
    }
}

/// The draft and the voice note the main loop has not taken, and whether a message said is kept as a voice note in place of being written down.
#[derive(Default)]
pub struct Draft {
    draft: Option<String>,
    note: Option<Recording>,
    is_voice: bool,
}

impl Draft {
    /// Sets whether a message said is kept as a voice note.
    pub fn set_voice(&mut self, is_voice: bool) {
        self.is_voice = is_voice;
    }
}

impl Job for Draft {
    type Input = Recording;
    type Chore = Infallible;
    type Report = Report;
    const KEEP: Keep = Keep::Newest;

    // A message is said; what is typed goes into the draft on the screen.
    fn take_query(&mut self, query: Query) -> Option<Recording> {
        let Query::Voice { sound, seconds } = query else {
            return None;
        };
        let recording = Recording { sound, seconds };
        if !self.is_voice {
            return Some(recording);
        }
        self.note = Some(recording);
        None
    }

    fn is_written_down(&self) -> bool {
        !self.is_voice
    }

    fn report(&mut self, phase: Phase) -> Report {
        Report { phase, draft: self.draft.take(), note: self.note.take() }
    }
}

type State = Voice<Draft>;

impl State {
    /// Takes a message's transcript; true if that is news.
    pub fn take_words(&mut self, words: String) -> bool {
        if !has_words(&words) {
            return self.set_phase(Phase::Failed(Cause::NotHeard));
        }
        self.job.draft = Some(words);
        self.phase = Phase::Idle;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn draft_report() {
        let mut state = State::new(true);
        assert!(state.set_phase(Phase::Writing) && !state.set_phase(Phase::Writing));
        state.begin_recording(true, 0);
        assert_eq!(state.take_report().phase, Phase::Listening, "a recording that runs comes first");
        state.end_recording();
        assert!(state.set_phase(Phase::Failed(Cause::NoNetwork)));
        assert!(state.take_words("See you at noon".into()));
        assert_eq!(
            state.take_report(),
            Report { phase: Phase::Idle, draft: Some("See you at noon".into()), note: None }
        );
        assert_eq!(state.take_report().draft, None, "once");
    }

    #[test]
    fn blank_words() {
        let mut state = State::new(true);
        assert!(state.take_words(" \n".into()), "news");
        assert_eq!(state.take_report(), Report { phase: Phase::Failed(Cause::NotHeard), draft: None, note: None });
        assert!(!state.take_words(String::new()), "and no news again");
        assert!(state.take_words("Ja".into()));
        assert_eq!(state.take_report(), Report { phase: Phase::Idle, draft: Some("Ja".into()), note: None });
    }

    #[test]
    fn voice_note_kept() {
        let mut state = State::new(true);
        state.job.set_voice(true);
        state.begin_recording(true, 0);
        assert!(!state.should_connect_ahead(), "a voice note goes as it is, and nothing is opened to the speech model");
        state.take_recording(Some((vec![1, 2], 4)));
        assert!(!state.has_inputs() && state.is_off(), "nothing waits to be written down, nor needs the key");
        let report = state.take_report();
        assert_eq!(
            (report.phase, report.draft, report.note),
            (Phase::Idle, None, Some(Recording { sound: vec![1, 2], seconds: 4 })),
            "the voice note as it was said"
        );
        assert_eq!(state.take_report().note, None, "once");
        state.job.set_voice(false);
        state.begin_recording(true, 0);
        assert!(state.should_connect_ahead(), "a message to be written down has its connection made ahead");
        state.take_recording(Some((vec![3], 2)));
        assert!(state.has_inputs() && state.take_report().note.is_none(), "written down as a new device has it");
    }

    #[test]
    fn overheard_press_claimed() {
        let mut state = State::new(true);
        state.begin_recording(false, 0);
        assert!(
            state.take_report().phase == Phase::Idle && !state.should_connect_ahead(),
            "a press that is no hold yet tells nothing and opens nothing"
        );
        state.claim_recording();
        assert!(
            state.take_report().phase == Phase::Listening && state.should_connect_ahead(),
            "held, the recording is the message's from the press on"
        );
        state.take_recording(Some((vec![5], 3)));
        assert!(state.has_inputs(), "and is written down as one a hold began");
    }

    #[test]
    fn debug_hides_words() {
        assert_eq!(alloc::format!("{:?}", Recording { sound: vec![1, 2], seconds: 3 }), "Said { seconds: 3, .. }");
        let report = Report {
            phase: Phase::Idle,
            draft: Some("Lunch?".into()),
            note: Some(Recording { sound: vec![1], seconds: 2 }),
        };
        assert_eq!(alloc::format!("{report:?}"), "Report { phase: Idle, draft: true, note: Some(2) }");
    }
}
