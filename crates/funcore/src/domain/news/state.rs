//! State shared by the editor's thread and the main loop.

use super::Edition;
use super::editor::EditionResult;
use super::rundowns::Rundowns;
use super::silent::Silent;
use super::topics::Topics;
use crate::base::handoff::Loan;
use crate::domain::cause::Cause;
use crate::gfx::palette::Roles;
use alloc::string::String;

/// The input of a pass of the editor's thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EditionRequest {
    /// The local clock's offset from UTC, in minutes.
    pub offset: i16,
    pub edition: Edition,
    /// True if the edition is made whatever the hour and the files.
    pub is_forced: bool,
    /// True if a screen or the console asked.
    pub is_asked: bool,
}

impl EditionRequest {
    pub(super) const fn should_try(&self, is_due: bool) -> bool {
        self.is_asked || is_due
    }

    /// True for a try nobody asked for, which is made once a day: its day is noted in the tried file before it is made.
    pub const fn is_noted(&self) -> bool {
        !self.is_asked
    }
}

/// The editor's report for the main loop.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    /// True if files were written.
    pub has_changed: bool,
    /// Rundowns stored.
    pub kept_count: u32,
    /// The reason an edition is incomplete.
    pub cause: Option<Cause>,
    /// The days being gathered and written.
    pub days: (Option<String>, Option<String>),
    /// The feeds that gave no story when they were last read, where that is news.
    pub silent: Option<Silent>,
}

/// State shared by the editor's thread and the main loop.
#[derive(Default)]
pub struct State {
    schedule: Option<(i16, Edition)>,
    topics: Option<Topics>,
    silent: Silent,
    is_wanted: bool,
    is_opened: bool,
    is_forced: bool,
    is_working: bool,
    is_ended: bool,
    has_radio: bool,
    /// The day whose feeds are being read.
    pub gathering_day: Option<String>,
    /// The day whose rundowns the model is writing.
    pub writing_day: Option<String>,
    /// The page colours.
    pub roles: Roles,
    /// The pending rundowns, on loan while their file is written.
    pub pending: Loan<Rundowns>,
    pub report: Report,
}

impl State {
    /// Sets the clock's offset and the edition the owner set, once the settings are read; true if they changed while the thread runs.
    pub fn set_schedule(&mut self, schedule: Option<(i16, Edition)>) -> bool {
        schedule.is_some() && core::mem::replace(&mut self.schedule, schedule) != schedule && !self.is_ended
    }

    /// Sets the rundowns the owner keeps; true if they are new while the thread runs: it makes no edition before it has them, and rundowns that change ask as a news screen that opens does, so that one kept or switched on once the day's edition is due is made the same day.
    pub fn set_topics(&mut self, topics: Topics) -> bool {
        let is_new = self.topics.as_ref() != Some(&topics);
        self.is_opened |= is_new && self.topics.is_some();
        self.topics = Some(topics);
        is_new && !self.is_ended
    }

    /// The rundowns the owner keeps.
    pub fn topics(&self) -> Topics {
        self.topics.clone().unwrap_or_default()
    }

    /// Takes the feeds an earlier run found silent, as their file keeps them.
    pub fn set_silent(&mut self, silent: Silent) {
        self.report.silent = Some(silent.clone());
        self.silent = silent;
    }

    /// Takes the silent feeds of each rundown a gathering read; the text of their file where that is news.
    pub fn note_silent<'a>(&mut self, found: impl Iterator<Item = (&'a str, &'a [String])>) -> Option<String> {
        let is_new = found.fold(false, |is_new, (name, lines)| self.silent.note(name, lines) | is_new);
        is_new.then(|| {
            self.report.silent = Some(self.silent.clone());
            self.silent.to_text()
        })
    }

    /// Sets whether a news screen shows; true if one opens while the thread runs.
    pub fn set_wanted(&mut self, is_wanted: bool) -> bool {
        let has_opened = is_wanted && !self.is_wanted;
        self.is_wanted = is_wanted;
        self.is_opened |= has_opened;
        has_opened && !self.is_ended
    }

    /// Forces the day's edition, whatever the hour and the files; true while the thread runs to make it.
    pub fn force(&mut self) -> bool {
        self.is_forced = true;
        !self.is_ended
    }

    /// The pass's request, once the settings and the rundowns are read.
    pub fn take_request(&mut self) -> Option<EditionRequest> {
        let (offset, edition) = self.schedule.filter(|_| self.topics.is_some())?;
        let (is_opened, is_forced) = (core::mem::take(&mut self.is_opened), core::mem::take(&mut self.is_forced));
        Some(EditionRequest { offset, edition, is_forced, is_asked: is_forced || is_opened })
    }

    /// Notes an edition to make; the thread then waits for the radio.
    pub fn begin(&mut self) {
        self.is_working = true;
        self.has_radio = false;
    }

    /// True once the main loop has freed the radio for this edition.
    pub const fn has_radio(&self) -> bool {
        self.has_radio
    }

    /// Frees the radio; true if the thread waits for it.
    pub fn grant_radio(&mut self) -> bool {
        let is_waiting = self.is_working && !self.has_radio;
        self.has_radio |= is_waiting;
        is_waiting
    }

    /// Ends the making, with the day whose rundowns the model still writes.
    pub fn end(&mut self, result: &EditionResult, writing_day: Option<String>) {
        self.is_working = false;
        self.has_radio = false;
        self.gathering_day = None;
        self.writing_day = writing_day;
        self.report.kept_count += result.kept_count;
        self.report.cause = result.cause.or(self.report.cause);
    }

    /// Notes the thread's end, as it can wait no more: it makes no edition from then on, holds nothing off, and nothing shows as gathered or written.
    pub fn end_thread(&mut self) {
        (self.is_ended, self.is_working, self.has_radio) = (true, false, false);
        (self.gathering_day, self.writing_day) = (None, None);
    }

    /// True while the thread has no edition to make, and once it has ended.
    pub const fn is_off(&self) -> bool {
        !self.is_working
    }

    /// Takes the report with the current days.
    pub fn take_report(&mut self) -> Report {
        let days = (self.gathering_day.clone(), self.writing_day.clone());
        Report { days, ..core::mem::take(&mut self.report) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_grant() {
        let mut state = State::default();
        assert!(state.is_off() && !state.grant_radio() && !state.has_radio);
        state.begin();
        assert!(!state.is_off() && !state.has_radio);
        assert!(state.grant_radio() && !state.grant_radio() && state.has_radio);
        state.gathering_day = Some("20261013".into());
        state.report.has_changed = true;
        state.end(&EditionResult { kept_count: 1, ..EditionResult::default() }, None);
        assert_eq!((state.is_off(), state.has_radio, state.gathering_day.clone()), (true, false, None));
        assert_eq!(state.take_report(), Report { has_changed: true, kept_count: 1, ..Report::default() });
        state.begin();
        state.end(&EditionResult::default(), Some("20261014".into()));
        assert_eq!(state.writing_day.as_deref(), Some("20261014"));
    }

    #[test]
    fn latest_cause() {
        let mut state = State::default();
        state.end(&EditionResult { kept_count: 1, cause: Some(Cause::Refused), ..EditionResult::default() }, None);
        state.end(&EditionResult { kept_count: 2, cause: Some(Cause::NoKey), ..EditionResult::default() }, None);
        state.end(&EditionResult::default(), None);
        assert_eq!((state.report.kept_count, state.report.cause), (3, Some(Cause::NoKey)));
    }

    #[test]
    fn request_after_settings() {
        let mut state = State::default();
        assert!(state.set_wanted(true) && !state.set_wanted(true));
        state.force();
        assert_eq!(state.take_request(), None);
        let (edition, evening) = (Edition::default(), Edition { hour: Some(19) });
        assert!(
            state.set_schedule(Some((120, edition)))
                && !state.set_schedule(Some((120, edition)))
                && !state.set_schedule(None)
        );
        assert_eq!(state.take_request(), None, "nor before the rundowns the owner keeps are read");
        let kept = Topics::parse("rundown finance on\n");
        assert!(state.set_topics(kept.clone()) && !state.set_topics(kept.clone()) && state.topics() == kept);
        let request = |is_forced, is_asked| Some(EditionRequest { offset: 120, edition, is_forced, is_asked });
        assert_eq!(state.take_request(), request(true, true));
        assert_eq!(state.take_request(), request(false, false));
        assert!(!state.set_wanted(false) && state.set_wanted(true));
        assert_eq!(state.take_request(), request(false, true));
        assert!(state.set_schedule(Some((120, evening))), "another hour is news");
        state.set_schedule(Some((120, edition)));
        assert!(state.set_topics(Topics::default()), "other rundowns are news");
        assert_eq!(state.take_request(), request(false, true), "and ask for what the day lacks of them");
        let unasked = EditionRequest { offset: 0, edition, is_forced: false, is_asked: false };
        assert!(
            !unasked.should_try(false)
                && unasked.should_try(true)
                && EditionRequest { is_asked: true, ..unasked }.should_try(false)
        );
        assert!(
            unasked.is_noted() && !EditionRequest { is_asked: true, ..unasked }.is_noted(),
            "a try nobody asked for is made once a day, one asked for whenever it is asked"
        );
    }

    #[test]
    fn silent_feeds_told_once() {
        let mut state = State::default();
        assert_eq!(state.topics(), Topics::default(), "before any are read, the two a new device has");
        state.set_silent(Silent::parse("feed news https://a.example/rss\n"));
        assert_eq!(
            state.take_report().silent.map(|silent| silent.to_text()).as_deref(),
            Some("feed news https://a.example/rss\n")
        );
        let (same, none): ([String; 1], [String; 0]) = (["https://a.example/rss".into()], []);
        assert_eq!(state.note_silent([("news", &same[..]), ("finance", &none[..])].into_iter()), None);
        assert_eq!(state.take_report().silent, None, "what a gathering finds as it was is no news");
        assert_eq!(state.note_silent([("news", &none[..])].into_iter()).as_deref(), Some(""), "every feed answered");
        assert_eq!(state.take_report().silent, Some(Silent::default()));
    }

    #[test]
    fn thread_end() {
        let mut state = State::default();
        state.set_schedule(Some((120, Edition::default())));
        state.begin();
        (state.gathering_day, state.writing_day) = (Some("20261013".into()), Some("20261013".into()));
        state.end_thread();
        assert!(state.is_off() && !state.has_radio, "an ended thread holds off no sleep and no rest of the radio");
        assert_eq!(state.take_report().days, (None, None), "and nothing shows as gathered or written");
        assert!(
            !state.set_wanted(true)
                && !state.force()
                && !state.set_schedule(Some((60, Edition::default())))
                && !state.set_topics(Topics::default()),
            "nor is it nudged for an edition"
        );
    }
}
