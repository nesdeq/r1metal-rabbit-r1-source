//! The edition's schedule and the result of making one.

use super::state::EditionRequest;
use crate::domain::cause::Cause;
use crate::domain::shelf::Shelf;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The file of the last day the thread tried unasked, with the rundowns it asked for anew that day.
pub const TRIED_FILE: &str = "tried";

/// True while a news screen shows, so a due and incomplete edition is tried again.
pub fn is_wanted(shelf: Option<Shelf>) -> bool {
    shelf == Some(Shelf::News)
}

/// The next step of the editor's thread, each with the pass's request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Try(EditionRequest),
    // Looks at the pending rundowns.
    Look(EditionRequest),
    // Sets the alarm.
    Arm(EditionRequest),
    Wait,
}

/// When the thread tries an edition unasked: at its start and at its alarm.
pub struct Schedule {
    is_starting: bool,
    due_s: Option<u64>,
    next: Step,
}

impl Default for Schedule {
    fn default() -> Schedule {
        Schedule { is_starting: true, due_s: None, next: Step::Wait }
    }
}

impl Schedule {
    fn is_due(&mut self, utc: u64) -> bool {
        let is_start = core::mem::take(&mut self.is_starting);
        is_start || self.due_s.is_some_and(|due_s| utc >= due_s)
    }

    /// Begins a pass with the thread's request, none where it can do nothing.
    pub fn begin_pass(&mut self, request: Option<EditionRequest>, utc: u64) {
        self.next = match request {
            Some(request) if request.should_try(self.is_due(utc)) => Step::Try(request),
            Some(request) => Step::Look(request),
            None => Step::Wait,
        };
    }

    pub const fn next_step(&mut self) -> Step {
        let step = self.next;
        self.next = match step {
            Step::Try(request) => Step::Look(request),
            Step::Look(request) => Step::Arm(request),
            Step::Arm(_) | Step::Wait => Step::Wait,
        };
        step
    }

    /// Sets the alarm's time; returns it if it changed, for the log.
    pub fn set_due(&mut self, next_s: Option<u64>) -> Option<u64> {
        let is_new = next_s != self.due_s;
        self.due_s = next_s;
        next_s.filter(|_| is_new)
    }
}

/// The result of asking for a rundown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    // The answer came at once and the rundown is stored.
    Kept,
    // The model works on it, and a later look collects it.
    Pending,
}

/// The result of making an edition or of a look at the pending rundowns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EditionResult {
    pub kept_count: u32,
    pub waiting_count: usize,
    pub cause: Option<Cause>,
}

impl EditionResult {
    /// Counts a rundown's outcome.
    pub fn add(&mut self, outcome: &Result<Outcome, Fault>) {
        match outcome {
            Ok(Outcome::Kept) => self.kept_count += 1,
            Ok(Outcome::Pending) => self.waiting_count += 1,
            Err(fault) => self.add_fault(fault),
        }
    }

    /// Notes a failed rundown; the first cause stays.
    pub fn add_fault(&mut self, fault: &Fault) {
        self.cause = self.cause.or(Some(Cause::from_fault(fault, Cause::NotWritten)));
    }

    /// The log line of a making that took `ms`.
    pub fn to_made_line(&self, ms: u128) -> String {
        let Self { kept_count, waiting_count, .. } = self;
        format!("news: {kept_count} rundowns kept and {waiting_count} on their way, {ms} ms after the making began")
    }

    /// The log line of a look that took `ms` and left `waiting` pending.
    pub fn to_looked_line(&self, waiting: usize, ms: u128) -> String {
        let kept_count = self.kept_count;
        format!("news: {kept_count} rundowns kept, {waiting} still on their way, {ms} ms after the looking began")
    }
}

fn note<'a>(text: Option<&'a str>, day: &str, line: &'a str) -> Option<String> {
    let is_of_day = |kept: &&str| kept.split(' ').next() == Some(day);
    let mut lines: Vec<&str> = text.unwrap_or_default().lines().map(str::trim).filter(is_of_day).collect();
    if lines.contains(&line) {
        return None;
    }
    lines.push(line);
    Some(lines.iter().map(|line| format!("{line}\n")).collect())
}

/// The tried file's text with the day's unasked try noted, none where it tells of that try already; another day's lines go.
pub fn note_try(text: Option<&str>, day: &str) -> Option<String> {
    note(text, day, day)
}

/// The tried file's text with a rundown noted as asked for anew that day, none where it was asked anew that day before.
pub fn note_anew(text: Option<&str>, day: &str, topic: &str) -> Option<String> {
    note(text, day, &format!("{day} {topic}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::refusal::Refusal;

    #[test]
    fn wanted_on_news_shelf() {
        assert_eq!(
            [Some(Shelf::Notes), Some(Shelf::Talks), Some(Shelf::News), None].map(is_wanted),
            [false, false, true, false]
        );
    }

    #[test]
    fn due_at_start_and_alarm() {
        let mut schedule = Schedule::default();
        assert!(schedule.is_due(0) && !schedule.is_due(0));
        assert_eq!((schedule.set_due(Some(1000)), schedule.set_due(Some(1000))), (Some(1000), None));
        assert!(!schedule.is_due(999) && schedule.is_due(1000) && schedule.is_due(5000));
        assert_eq!((schedule.set_due(None), schedule.set_due(Some(2000))), (None, Some(2000)));
        assert!(!schedule.is_due(1999));
    }

    #[test]
    fn tried_file_notes() {
        let day = "20261013";
        assert_eq!(note_try(None, day).as_deref(), Some("20261013\n"));
        assert_eq!(
            [note_try(Some("20261013"), day), note_try(Some("20261013\n"), day)],
            [None, None],
            "tried, as an earlier firmware wrote it and as this one does"
        );
        assert_eq!(
            note_try(Some("20261012\n20261012 news\n"), day).as_deref(),
            Some("20261013\n"),
            "another day's lines go"
        );
        let once = note_anew(Some("20261013\n"), day, "news");
        assert_eq!(once.as_deref(), Some("20261013\n20261013 news\n"));
        assert_eq!(note_anew(once.as_deref(), day, "news"), None, "a rundown once a day");
        assert_eq!(
            note_anew(once.as_deref(), day, "finance").as_deref(),
            Some("20261013\n20261013 news\n20261013 finance\n"),
            "each rundown for itself"
        );
        assert_eq!(note_try(once.as_deref(), day), None, "and the day's try stands noted beside them");
        assert_eq!(
            note_try(Some("20261013 news\n"), day).as_deref(),
            Some("20261013 news\n20261013\n"),
            "a rundown asked anew before the day's own try leaves that try to make"
        );
    }

    #[test]
    fn pass_steps() {
        let edition = crate::domain::news::Edition::default();
        let unasked = EditionRequest { offset: 60, edition, is_forced: false, is_asked: false };
        let mut schedule = Schedule::default();
        schedule.begin_pass(None, 0);
        assert_eq!(schedule.next_step(), Step::Wait, "nothing is asked");
        assert!(schedule.is_starting, "and the start stands");
        schedule.begin_pass(Some(unasked), 0);
        assert_eq!(
            [schedule.next_step(), schedule.next_step(), schedule.next_step(), schedule.next_step()],
            [Step::Try(unasked), Step::Look(unasked), Step::Arm(unasked), Step::Wait],
            "as it starts"
        );
        schedule.begin_pass(Some(unasked), 0);
        assert_eq!((schedule.next_step(), schedule.next_step()), (Step::Look(unasked), Step::Arm(unasked)), "not due");
        let asked = EditionRequest { is_asked: true, ..unasked };
        schedule.begin_pass(Some(asked), 0);
        assert_eq!(schedule.next_step(), Step::Try(asked));
    }

    #[test]
    fn made_and_looked_lines() {
        let result = EditionResult { kept_count: 1, waiting_count: 2, cause: None };
        assert_eq!(result.to_made_line(7), "news: 1 rundowns kept and 2 on their way, 7 ms after the making began");
        assert_eq!(
            result.to_looked_line(3, 7),
            "news: 1 rundowns kept, 3 still on their way, 7 ms after the looking began"
        );
    }

    #[test]
    fn result_counts_and_first_cause() {
        let mut result = EditionResult::default();
        result.add(&Ok(Outcome::Kept));
        result.add(&Err(Fault::Offline("asleep".into())));
        result.add(&Ok(Outcome::Pending));
        result.add(&Err(Fault::NoKey));
        result.add_fault(&Fault::Temporary("gone".into()));
        assert_eq!(result, EditionResult { kept_count: 1, waiting_count: 1, cause: Some(Cause::NoNetwork) });
        let refusal = Refusal { code: "server_error".into(), message: "Later.".into() };
        let mut busy = EditionResult::default();
        busy.add(&Err(Fault::from_refusal(Some(503), &refusal)));
        assert_eq!(busy.cause, Some(Cause::NotWritten));
    }
}
