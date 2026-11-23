//! State shared by the notes thread and the screens.

use super::work::Work;
use super::{Memo, Request};
use crate::domain::cause::Cause;
use crate::domain::scribe::Entry;
use crate::domain::shelf::Shelf;
use crate::domain::tally::{HomeView, Tally};
use alloc::string::String;
use alloc::vec::Vec;

/// The inputs of a round of the notes thread.
pub struct Round {
    pub(super) target: Option<Shelf>,
    pub(super) requests: Vec<Request>,
    pub(super) is_stale: bool,
    pub(super) play_end: Option<u64>,
    pub(super) home: Option<(u32, u64)>,
}

/// The notes thread's report for the screens.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    /// A new listing of a shelf, newest first.
    pub listing: Option<(Shelf, Vec<Memo>)>,
    /// The note being recorded.
    pub recording: Option<String>,
    /// The note playing.
    pub playing: Option<String>,
    pub kept_count: u32,
    pub cause: Option<Cause>,
    /// A new tally for home.
    pub tally: Option<Tally>,
    /// New entries for the scribe.
    pub new_entries: Vec<Entry>,
}

/// State shared by the screens and the notes thread.
#[derive(Default)]
pub struct State {
    target: Option<Shelf>,
    requests: Vec<Request>,
    is_stale: bool,
    file_changes: u32,
    play_end: Option<u64>,
    is_busy: bool,
    home: HomeView,
    pub report: Report,
}

impl State {
    /// Sets the shelf shown, the count of file changes and home's view; true if any changed. The robot's shelves are none of its.
    pub fn set_wanted(&mut self, shelf: Option<Shelf>, file_changes: u32, (is_home, day): (bool, u64)) -> bool {
        let shelf = shelf.filter(|shelf| !shelf.is_robots());
        let has_new_shelf = self.target != shelf;
        self.target = shelf;
        self.is_busy |= has_new_shelf && shelf.is_none();
        let has_new_files = self.file_changes != file_changes;
        self.file_changes = file_changes;
        self.is_stale |= has_new_files;
        let has_new_home = self.home.set(is_home, day);
        has_new_shelf || has_new_files || has_new_home
    }

    /// Adds the screens' requests; true if there are any.
    pub fn add_requests(&mut self, requests: Vec<Request>) -> bool {
        let has_any = !requests.is_empty();
        self.requests.extend(requests);
        self.is_busy |= has_any;
        has_any
    }

    /// Notes the end of the playing of that number.
    pub fn note_play_end(&mut self, number: u64) {
        self.play_end = Some(number);
    }

    /// True if the thread holds no microphone or speaker and has no work left.
    pub const fn is_off(&self) -> bool {
        !self.is_busy
    }

    /// The next round, if the thread has any work.
    pub fn take_round(&mut self, work: &Work) -> Option<Round> {
        let home = self.home.count_key();
        let is_home_due = home.is_some() && home != work.count_key;
        let is_idle = self.target == work.shelf
            && self.requests.is_empty()
            && !self.is_stale
            && self.play_end.is_none()
            && !is_home_due;
        (!is_idle).then(|| Round {
            target: self.target,
            requests: core::mem::take(&mut self.requests),
            is_stale: core::mem::take(&mut self.is_stale),
            play_end: self.play_end.take(),
            home,
        })
    }

    /// Merges the thread's report; returns the listing it replaced, to drop outside the lock.
    pub fn add_report(&mut self, report: Report) -> Option<(Shelf, Vec<Memo>)> {
        let Report { listing, tally, recording, playing, kept_count, cause, new_entries } = report;
        let previous = listing.and_then(|listing| self.report.listing.replace(listing));
        self.report.tally = tally.or(self.report.tally.take());
        self.is_busy = recording.is_some() || playing.is_some() || !self.requests.is_empty();
        self.report.recording = recording;
        self.report.playing = playing;
        self.report.kept_count += kept_count;
        self.report.cause = cause.or(self.report.cause);
        self.report.new_entries.extend(new_entries);
        previous
    }

    /// Takes the report; `recording` and `playing` stay.
    pub fn take_report(&mut self) -> Report {
        let next = Report {
            recording: self.report.recording.clone(),
            playing: self.report.playing.clone(),
            ..Report::default()
        };
        core::mem::replace(&mut self.report, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn round_and_report() {
        let mut state = State::default();
        let mut work = Work::default();
        assert!(state.is_off() && state.take_round(&work).is_none());
        assert!(!state.add_requests(Vec::new()) && state.is_off());
        assert!(
            state.set_wanted(Some(Shelf::Notes), 0, (false, 0)) && !state.set_wanted(Some(Shelf::Notes), 0, (false, 0))
        );
        let round = state.take_round(&work).expect("another kind shows");
        assert_eq!(
            (round.target, round.requests.len(), round.is_stale, round.play_end),
            (Some(Shelf::Notes), 0, false, None)
        );
        work.begin_round(round);
        assert_eq!(work.take_relist(), Some(Shelf::Notes));
        assert!(state.take_round(&work).is_none());
        assert!(
            state.set_wanted(Some(Shelf::Facts), 0, (false, 0)) && state.take_round(&work).unwrap().target.is_none(),
            "the robot's facts are the conversation's to list"
        );
        assert!(state.add_requests(vec![Request::Record]));
        assert!(state.set_wanted(Some(Shelf::Notes), 1, (false, 0)));
        state.note_play_end(3);
        assert!(!state.is_off());
        let round = state.take_round(&work).unwrap();
        assert_eq!((round.requests, round.is_stale, round.play_end), (vec![Request::Record], true, Some(3)));
        assert!(state.take_round(&work).is_none());
        assert!(
            state.set_wanted(Some(Shelf::Notes), 1, (true, 5)) && !state.set_wanted(Some(Shelf::Notes), 1, (true, 5))
        );
        let round = state.take_round(&work).unwrap();
        assert!(round.home.is_some_and(|home| home.1 == 5));
        work.begin_round(round);
        assert!(work.take_recount() && !work.take_recount());
        assert!(state.take_round(&work).is_none());
        state.set_wanted(Some(Shelf::Notes), 2, (true, 5));
        work.begin_round(state.take_round(&work).unwrap());
        assert!(work.take_recount());
        let notes = vec![Memo { stem: "a".into(), seconds: 3, words: None, summary: None, rundowns: Vec::new() }];
        let recording = || Some(String::from("n"));
        let report =
            |listing, cause| Report { listing, recording: recording(), kept_count: 1, cause, ..Report::default() };
        assert_eq!(state.add_report(report(Some((Shelf::Notes, notes.clone())), Some(Cause::NotPlayed))), None);
        let previous = state.add_report(report(Some((Shelf::Notes, Vec::new())), Some(Cause::NotRecorded)));
        assert_eq!(previous, Some((Shelf::Notes, notes)));
        assert!(!state.is_off());
        assert_eq!(
            state.take_report(),
            Report {
                listing: Some((Shelf::Notes, Vec::new())),
                recording: recording(),
                playing: None,
                kept_count: 2,
                cause: Some(Cause::NotRecorded),
                tally: None,
                new_entries: Vec::new()
            }
        );
        assert_eq!(state.take_report(), Report { recording: recording(), ..Report::default() });
        state.add_requests(vec![Request::Record]);
        state.add_report(Report { recording: None, ..report(None, None) });
        assert!(!state.is_off(), "a request handed over during the round still waits");
        work.begin_round(state.take_round(&work).unwrap());
        state.add_report(Report { recording: None, ..report(None, None) });
        assert!(state.is_off());
        assert!(state.set_wanted(None, 2, (true, 5)) && !state.is_off());
    }
}
