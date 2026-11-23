use super::writer::Status;
use super::{Days, Logging, Queue};
use alloc::string::String;

#[derive(Default)]
struct Scrubs {
    latest: u64,
    done: u64,
    failure: Option<String>,
}

/// The state the writer, the packer, their waiters and the main loop share.
pub struct State<R> {
    /// The settings' logging, none before they are read.
    pub logging: Option<Logging>,
    /// How the writing stands, for the main loop.
    pub report: Option<Status>,
    pub queue: Queue,
    pub days: Days<R>,
    scrubs: Scrubs,
}

impl<R> Default for State<R> {
    fn default() -> State<R> {
        State { logging: None, report: None, queue: Queue::default(), days: Days::default(), scrubs: Scrubs::default() }
    }
}

impl<R> State<R> {
    /// Asks for a scrub and gives its number.
    pub const fn ask_scrub(&mut self) -> u64 {
        self.scrubs.latest += 1;
        self.scrubs.latest
    }

    /// The number of the latest scrub asked for, while one waits.
    pub fn scrub_due(&self) -> Option<u64> {
        (self.scrubs.done < self.scrubs.latest).then_some(self.scrubs.latest)
    }

    /// Marks every scrub up to that number done, with its failure's text.
    pub fn end_scrub(&mut self, number: u64, failure: Option<String>) {
        (self.scrubs.done, self.scrubs.failure) = (number, failure);
    }

    /// The outcome of the scrub of that number, once it is done.
    pub fn scrub_outcome(&self, number: u64) -> Option<Result<(), String>> {
        (self.scrubs.done >= number).then(|| self.scrubs.failure.clone().map_or(Ok(()), Err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_by_number() {
        let mut state = State::<()>::default();
        assert_eq!(state.scrub_due(), None, "none asked for, none due");
        let first = state.ask_scrub();
        assert_eq!((first, state.scrub_due(), state.scrub_outcome(first)), (1, Some(1), None), "asked, it is due");
        let second = state.ask_scrub();
        assert_eq!(state.scrub_due(), Some(second), "one asked for while it is due makes the later number due");
        state.end_scrub(first, Some("no room".into()));
        assert_eq!(
            (state.scrub_outcome(first), state.scrub_outcome(second), state.scrub_due()),
            (Some(Err("no room".into())), None, Some(second)),
            "the first's asker hears why it failed, and the second, asked while it ran, is still due"
        );
        state.end_scrub(second, None);
        assert_eq!(
            (state.scrub_outcome(first), state.scrub_outcome(second), state.scrub_due()),
            (Some(Ok(())), Some(Ok(())), None),
            "a scrub that ends covers every one asked for up to its number"
        );
    }
}
