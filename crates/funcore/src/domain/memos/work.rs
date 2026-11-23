//! The notes thread's work of each round.

use super::Memo;
use super::Request;
use super::state::{Report, Round};
use crate::domain::cause::Cause;
use crate::domain::scribe::Entry;
use crate::domain::shelf::Shelf;
use crate::domain::tally::Tally;
use crate::openai::ledger::SummaryKind;
use crate::storage::SaveFault;
use alloc::string::String;
use alloc::vec::Vec;

/// An effect of the notes thread.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    // Ends the recording and keeps the note.
    End,
    // Begins a recording, the playing note stopped first.
    Begin,
    Stop,
    // The playing's number and the note's stem.
    Play(u64, String),
    // Deletes the entry, the playing note stopped first where true.
    Delete(SummaryKind, String, bool),
}

fn has_sound(shelf: Option<Shelf>) -> bool {
    shelf == Some(Shelf::Notes)
}

/// The thread's state across rounds and the results of the current round.
#[derive(Default)]
pub struct Work {
    pub(super) shelf: Option<Shelf>,
    pub(super) count_key: Option<(u32, u64)>,
    recording: Option<String>,
    playing: Option<(u64, String)>,
    play_count: u64,
    target: Option<Shelf>,
    home: Option<(u32, u64)>,
    is_stale: bool,
    kept_count: u32,
    cause: Option<Cause>,
    new_entries: Vec<Entry>,
}

impl Work {
    fn is_playing(&self, stem: &str) -> bool {
        self.playing.as_ref().is_some_and(|playing| playing.1 == stem)
    }

    /// Begins a round; returns its first actions and the screens' requests.
    pub fn begin_round(&mut self, round: Round) -> (Vec<Action>, Vec<Request>) {
        self.target = round.target;
        self.home = round.home;
        self.is_stale = round.is_stale;
        self.kept_count = 0;
        self.cause = None;
        let mut actions = Vec::new();
        let has_ended = self.playing.as_ref().is_some_and(|playing| Some(playing.0) == round.play_end);
        if self.playing.is_some() && (has_ended || !has_sound(round.target)) {
            self.playing = None;
            actions.push(Action::Stop);
        }
        if self.recording.is_some() && !has_sound(round.target) {
            self.recording = None;
            self.is_stale = true;
            actions.push(Action::End);
        }
        (actions, round.requests)
    }

    /// The action of a request.
    pub fn take_request(&mut self, request: Request) -> Action {
        let action = match request {
            // Sound begins only where its shelf shows, whose leaving alone ends it.
            Request::Record | Request::Play(_) if !has_sound(self.target) => Action::Stop,
            Request::Record if self.recording.is_some() => Action::End,
            Request::Record => Action::Begin,
            Request::Play(stem) if self.is_playing(&stem) => Action::Stop,
            Request::Play(stem) => Action::Play(self.play_count + 1, stem),
            Request::Delete(kind, stem) => {
                let is_playing = self.is_playing(&stem);
                Action::Delete(kind, stem, is_playing)
            }
        };
        match &action {
            Action::End => {
                self.recording = None;
                self.is_stale = true;
            }
            Action::Begin | Action::Stop => self.playing = None,
            Action::Delete(_, _, is_playing) => {
                self.is_stale = true;
                if *is_playing {
                    self.playing = None;
                }
            }
            Action::Play(number, stem) => {
                self.play_count = *number;
                self.playing = Some((*number, stem.clone()));
            }
        }
        action
    }

    /// Notes a recording's start, or why it failed.
    pub fn note_start(&mut self, outcome: Result<String, SaveFault>) {
        match outcome {
            Ok(stem) => self.recording = Some(stem),
            Err(fault) => self.cause = Some(Cause::from_save_fault(fault)),
        }
    }

    /// Notes a recording's end: the note kept, or why not.
    pub fn note_end(&mut self, outcome: Result<String, SaveFault>) {
        match outcome {
            Ok(stem) => {
                self.kept_count += 1;
                self.new_entries.push((SummaryKind::Note, stem));
            }
            Err(fault) => self.cause = Some(Cause::from_save_fault(fault)),
        }
    }

    /// Notes a note that could not be played.
    pub fn fail_playing(&mut self) {
        self.playing = None;
        self.cause = Some(Cause::NotPlayed);
    }

    /// Notes an entry that could not be deleted.
    pub fn fail_deleting(&mut self) {
        self.cause = Some(Cause::NotDeleted);
    }

    /// The shelf to list again after the round's actions.
    pub fn take_relist(&mut self) -> Option<Shelf> {
        let relist = self.target.filter(|shelf| self.is_stale || self.shelf != Some(*shelf));
        self.shelf = self.target;
        relist
    }

    /// True if home is counted again after the round's actions.
    pub fn take_recount(&mut self) -> bool {
        let count = self.home.filter(|view| self.is_stale || self.count_key != Some(*view));
        self.count_key = count.or(self.count_key);
        count.is_some()
    }

    pub fn take_report(&mut self, listing: Option<(Shelf, Vec<Memo>)>, tally: Option<Tally>) -> Report {
        Report {
            listing,
            tally,
            recording: self.recording.clone(),
            playing: self.playing.as_ref().map(|playing| playing.1.clone()),
            kept_count: self.kept_count,
            cause: self.cause,
            new_entries: core::mem::take(&mut self.new_entries),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round(is_shown: bool, requests: Vec<Request>, play_end: Option<u64>) -> Round {
        Round { target: is_shown.then_some(Shelf::Notes), requests, is_stale: false, play_end, home: None }
    }

    #[test]
    fn clicks_and_deletes() {
        let mut work = Work::default();
        let (actions, requests) = work.begin_round(round(true, vec![Request::Record, Request::Record], None));
        assert!(actions.is_empty() && requests.len() == 2);
        assert_eq!(work.take_request(Request::Record), Action::Begin);
        work.note_start(Ok("n".into()));
        assert_eq!(work.take_request(Request::Record), Action::End);
        work.note_end(Ok("n".into()));
        assert_eq!(work.take_request(Request::Record), Action::Begin);
        work.note_start(Err(SaveFault::NotRecorded));
        assert_eq!(work.take_request(Request::Record), Action::Begin);
        assert_eq!(work.take_relist(), Some(Shelf::Notes));
        let report = work.take_report(None, None);
        assert_eq!((report.recording, report.kept_count, report.cause), (None, 1, Some(Cause::NotRecorded)));
        assert_eq!(report.new_entries, [(SummaryKind::Note, String::from("n"))]);
        work.begin_round(round(true, Vec::new(), None));
        assert_eq!(
            [
                work.take_request(Request::Play("a".into())),
                work.take_request(Request::Play("a".into())),
                work.take_request(Request::Play("b".into()))
            ],
            [Action::Play(1, "a".into()), Action::Stop, Action::Play(2, "b".into())]
        );
        assert_eq!(work.take_relist(), None);
        assert_eq!(
            [
                work.take_request(Request::Delete(SummaryKind::Note, "b".into())),
                work.take_request(Request::Delete(SummaryKind::Talk, "a".into()))
            ],
            [Action::Delete(SummaryKind::Note, "b".into(), true), Action::Delete(SummaryKind::Talk, "a".into(), false)]
        );
        assert_eq!(work.take_relist(), Some(Shelf::Notes));
        work.fail_deleting();
        let report = work.take_report(None, None);
        assert_eq!((report.playing, report.cause, report.kept_count), (None, Some(Cause::NotDeleted), 0));
        work.begin_round(round(true, Vec::new(), None));
        work.take_request(Request::Play("c".into()));
        assert_eq!(work.take_report(None, None).playing.as_deref(), Some("c"));
        work.fail_playing();
        assert_eq!(work.take_report(None, None).cause, Some(Cause::NotPlayed));
        work.take_request(Request::Play("c".into()));
        assert_eq!(work.begin_round(round(true, Vec::new(), Some(3))).0, Vec::new());
        assert_eq!(work.begin_round(round(true, Vec::new(), Some(4))).0, [Action::Stop]);
        assert_eq!(work.take_request(Request::Play("d".into())), Action::Play(5, "d".into()));
        assert_eq!(work.take_request(Request::Record), Action::Begin);
        work.note_start(Ok("n".into()));
        assert_eq!(work.take_report(None, None).playing, None);
        work.take_request(Request::Play("d".into()));
        let (actions, _) = work.begin_round(round(false, Vec::new(), None));
        assert_eq!(actions, [Action::Stop, Action::End]);
        assert!(
            work.take_relist().is_none() && !work.take_recount() && work.take_report(None, None).recording.is_none()
        );
        assert!(work.begin_round(round(false, Vec::new(), None)).0.is_empty());
    }

    #[test]
    fn no_sound_without_shelf() {
        let mut work = Work::default();
        let (actions, requests) =
            work.begin_round(round(false, vec![Request::Record, Request::Play("a".into())], None));
        assert!(actions.is_empty());
        let actions: Vec<Action> = requests.into_iter().map(|request| work.take_request(request)).collect();
        assert_eq!(actions, [Action::Stop, Action::Stop], "nothing records or plays while the notes do not show");
        let report = work.take_report(None, None);
        assert_eq!((report.recording, report.playing), (None, None));
    }
}
