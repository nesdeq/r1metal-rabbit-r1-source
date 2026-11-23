//! State shared by the scribe's thread and the main loop.

use super::Entry;
use crate::domain::backlog::{Backlog, Holder};
use crate::domain::cause::Cause;
use crate::gfx::palette::Roles;
use crate::openai::fault::Fault;
use crate::openai::ledger::SummaryKind;
use alloc::string::String;

/// The model an entry is with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Writing,
    Summing,
}

/// The scribe's report for the screen.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    writing: Option<Entry>,
    summing: Option<Entry>,
    pub has_changed: bool,
    /// Entries that got what they lacked.
    pub kept_count: u32,
    /// The reason one did not.
    pub cause: Option<Cause>,
}

impl Report {
    pub(crate) fn names_of(&self, kind: SummaryKind) -> (Option<&str>, Option<&str>) {
        let [writing, summing] = [&self.writing, &self.summing]
            .map(|entry| entry.as_ref().filter(|entry| entry.0 == kind).map(|entry| entry.1.as_str()));
        (writing, summing)
    }
}

/// State shared by the scribe's thread and the main loop.
#[derive(Default)]
pub struct State {
    pub backlog: Backlog<Entry, Model>,
    /// The note being recorded, left alone until its sound is whole.
    pub recording: Option<String>,
    /// The page colours.
    pub roles: Roles,
    pub report: Report,
}

impl Holder for State {
    type Key = Entry;
    type Busy = Model;

    fn backlog(&mut self) -> &mut Backlog<Entry, Model> {
        &mut self.backlog
    }
}

impl State {
    /// Takes an entry's outcome; the screen hears the reason if the entry is new or the work stalls.
    pub fn take_outcome(&mut self, entry: &Entry, outcome: &Result<bool, Fault>, is_new: bool) {
        let is_stalled = outcome.as_ref().is_err_and(Fault::is_stalling);
        self.backlog.end(entry, outcome.is_ok(), is_stalled);
        match outcome {
            Ok(true) => {
                self.report.kept_count += 1;
                self.report.has_changed = true;
            }
            Ok(false) => {}
            Err(fault) if is_new || is_stalled => {
                self.report.cause = Some(Cause::from_fault(fault, Cause::NotWritten));
            }
            Err(_) => {}
        }
    }

    fn entry_with(&self, model: Model) -> Option<Entry> {
        self.backlog.busy().find(|busy| busy.1 == model).map(|busy| busy.0.clone())
    }

    /// Takes the report, with the entry each model has.
    pub fn take_report(&mut self) -> Report {
        let (writing, summing) = (self.entry_with(Model::Writing), self.entry_with(Model::Summing));
        Report { writing, summing, ..core::mem::take(&mut self.report) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backlog::Status;
    use crate::openai::refusal::Refusal;
    use alloc::vec;
    use alloc::vec::Vec;

    fn pieces(state: &State) -> Vec<(SummaryKind, &str, Status<Model>)> {
        state.backlog.pieces.iter().map(|((kind, stem), status)| (*kind, stem.as_str(), *status)).collect()
    }

    fn note(stem: &str) -> Entry {
        (SummaryKind::Note, stem.into())
    }

    #[test]
    fn same_stem_two_entries() {
        let mut state = State::default();
        state.backlog.set_wanted(true);
        state.backlog.add_waiting(vec![note("old"), (SummaryKind::Talk, "old".into())]);
        state.backlog.add_waiting(vec![note("old")]);
        assert_eq!(
            pieces(&state),
            [(SummaryKind::Note, "old", Status::Waiting), (SummaryKind::Talk, "old", Status::Waiting)]
        );
        assert_eq!(state.backlog.take_next(Model::Writing), Some((note("old"), false)));
        assert_eq!((state.entry_with(Model::Writing), state.entry_with(Model::Summing)), (Some(note("old")), None));
        state.backlog.set_busy(&note("old"), Model::Summing);
        assert_eq!((state.entry_with(Model::Writing), state.entry_with(Model::Summing)), (None, Some(note("old"))));
        assert_eq!(state.backlog.take_next(Model::Writing), Some(((SummaryKind::Talk, "old".into()), false)));
        let report = state.take_report();
        assert_eq!(
            (report.names_of(SummaryKind::Note), report.names_of(SummaryKind::Talk)),
            ((None, Some("old")), (Some("old"), None))
        );
    }

    #[test]
    fn outcome_report() {
        let mut state = State::default();
        state.backlog.set_wanted(true);
        state.backlog.add_waiting(["a", "b", "c", "d", "e"].map(note).to_vec());
        for _ in 0..5 {
            state.backlog.take_next(Model::Writing);
        }
        state.take_outcome(&note("a"), &Ok(true), false);
        state.take_outcome(&note("b"), &Ok(false), false);
        state.take_outcome(&note("c"), &Err(Fault::Refused("no".into())), false);
        assert_eq!(
            (
                pieces(&state).len(),
                state.report.kept_count,
                state.report.has_changed,
                state.report.cause,
                state.backlog.is_stalled
            ),
            (3, 1, true, None, false)
        );
        let busy = Refusal { code: "server_error".into(), message: "Try again.".into() };
        state.take_outcome(&note("d"), &Err(Fault::from_refusal(Some(503), &busy)), true);
        assert_eq!(state.report.cause, Some(Cause::NotWritten));
        state.take_outcome(&note("e"), &Err(Fault::Offline("asleep".into())), false);
        assert_eq!((state.report.cause, state.backlog.is_stalled), (Some(Cause::NoNetwork), true));
        assert_eq!(
            pieces(&state),
            [
                (SummaryKind::Note, "c", Status::Failed),
                (SummaryKind::Note, "d", Status::Failed),
                (SummaryKind::Note, "e", Status::Failed)
            ]
        );
        state.backlog.is_stalled = false;
        state.backlog.add_new((SummaryKind::Talk, "f".into()));
        assert_eq!(state.backlog.take_next(Model::Writing), Some(((SummaryKind::Talk, "f".into()), true)));
        state.take_outcome(&(SummaryKind::Talk, "f".into()), &Err(Fault::NoKey), true);
        assert_eq!((state.report.cause, state.backlog.is_stalled), (Some(Cause::NoKey), true));
        let report = state.take_report();
        assert_eq!((report.kept_count, report.has_changed, report.cause), (1, true, Some(Cause::NoKey)));
        assert_eq!(state.take_report(), Report::default());
    }
}
