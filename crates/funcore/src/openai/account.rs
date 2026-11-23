//! The state every thread that asks a service shares.

use super::fault::Fault;
use super::ledger::Ledger;
use super::models::{Task, Thinking};
use super::money::MICROS_PER_DOLLAR;
use super::summary::Jobs;
use crate::base::handoff::Loan;

/// What the owner has set for asking the services: the model and effort of each use, and the most a month may cost, in dollars.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Terms {
    pub models: [Thinking; Task::ALL.len()],
    pub limit: Option<u16>,
}

impl Default for Terms {
    fn default() -> Terms {
        Terms { models: [Thinking::STANDARD; Task::ALL.len()], limit: None }
    }
}

impl Terms {
    pub const fn thinking(&self, task: Task) -> Thinking {
        self.models[task as usize]
    }

    /// True where a month's cost is limited, so that the ledger is to be read before work is paid for anew.
    pub const fn is_limited(&self) -> bool {
        self.limit.is_some()
    }

    /// Whether work paid for anew may be asked for in that month, as the ledger has it.
    pub fn check_limit(&self, ledger: &Ledger, year: u32, month: u8) -> Result<(), Fault> {
        let limit = self.limit.map(|dollars| u64::from(dollars) * MICROS_PER_DOLLAR);
        match limit {
            Some(limit) if !ledger.is_within(year, month, limit) => Err(Fault::Limit),
            _ => Ok(()),
        }
    }
}

/// The pending background jobs, the ledger, whether a service refused the stored key, and the owner's terms.
#[derive(Default)]
pub struct State {
    pub jobs: Jobs,
    /// None where the file is unread; out while a thread changes and writes it.
    pub ledger: Loan<Option<Ledger>>,
    is_refused: bool,
    pub terms: Terms,
}

impl State {
    /// Notes whether a service said it does not know the stored key.
    pub const fn note_refused(&mut self, is_unknown: bool) {
        self.is_refused |= is_unknown;
    }

    /// True once after a service said so.
    pub const fn take_refused(&mut self) -> bool {
        core::mem::replace(&mut self.is_refused, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::ledger::{Entry, Kind};
    use crate::openai::models::{Effort, Model};

    #[test]
    fn ledger_loan() {
        let mut state = State::default();
        assert_eq!(state.ledger.take(), Some(None), "unread");
        assert_eq!(state.ledger.take(), None, "out");
        state.ledger.give(None);
        assert_eq!(state.ledger.take(), Some(None), "still unread");
        state.ledger.give(Some(Ledger::default()));
        assert_eq!(state.ledger.take(), Some(Some(Ledger::default())));
    }

    #[test]
    fn limit_of_month() {
        let mut ledger = Ledger::default();
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 2_000_000);
        let terms = |limit| Terms { limit, ..Terms::default() };
        assert!(!terms(None).is_limited() && terms(Some(1)).is_limited());
        assert_eq!(
            [None, Some(1), Some(2), Some(3)].map(|limit| terms(limit).check_limit(&ledger, 2026, 10)),
            [Ok(()), Err(Fault::Limit), Err(Fault::Limit), Ok(())],
            "two dollars spent: no limit and three let work through, one and two do not"
        );
        assert_eq!(terms(Some(1)).check_limit(&ledger, 2026, 11), Ok(()), "a new month begins at nothing");
    }

    #[test]
    fn thinking_of_task() {
        let mut terms = Terms::default();
        terms.models[Task::News as usize] = Thinking::new(Model::Astra, Effort::High);
        assert_eq!(
            (terms.thinking(Task::News).model(), terms.thinking(Task::Auto), terms.thinking(Task::Chat)),
            (Model::Astra, Thinking::STANDARD, Thinking::STANDARD)
        );
    }
}
