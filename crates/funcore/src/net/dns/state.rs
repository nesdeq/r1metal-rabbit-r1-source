use super::NoAddress;
use super::cache::Cache;
use super::lookup::{Lookup, Stage};
use crate::base::handoff::Turns;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

type Resolved = Result<(Vec<IpAddr>, u32), NoAddress>;

/// What a lookup does next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // Waits for the asking under way; `State::add_waiter` takes what wakes it.
    Await,
    // Asks for these names; `State::end_asking` takes the outcome.
    Ask(Vec<String>),
    Done,
}

/// The state every asker of the resolver shares: the cache, and the turn to ask, one at a time; `E` is an asking's failure and `W` wakes a waiter.
pub struct State<E, W> {
    cache: Cache,
    // The failure of an asking, with the stay it was made in.
    turns: Turns<(u64, E), W>,
}

impl<E: Clone, W> State<E, W> {
    pub const fn new() -> State<E, W> {
        State { cache: Cache::new(), turns: Turns::new() }
    }

    fn find(&self, name: &str, now: u64, lines: &mut Vec<String>) -> Option<Vec<IpAddr>> {
        let (addresses, remaining) = self.cache.get(name, now)?;
        lines.push(format!("names: {name} is kept for {remaining} s more"));
        Some(addresses.to_vec())
    }

    /// A lookup of those names with what is kept of them at `now`.
    pub fn begin(&self, names: &[&str], now: u64) -> Lookup<E> {
        let mut lines = Vec::new();
        let mut lookup = Lookup::new(names, |name| self.find(name, now, &mut lines));
        lookup.add_lines(lines);
        lookup
    }

    /// What a lookup made in the stay of that number does next at `now`: an asking it waited through that failed in its stay is its failure too, and whoever came after it asks for itself.
    pub fn next_step(&mut self, lookup: &mut Lookup<E>, stay: u64, now: u64) -> Step {
        let count = match lookup.stage() {
            Stage::Done => return Step::Done,
            Stage::Checked => self.turns.end_count(),
            Stage::Waiting(count) => count,
        };
        let mut lines = Vec::new();
        // What another's asking learned while this one waited for its turn is kept by now.
        lookup.check_cache(|name| self.find(name, now, &mut lines));
        lookup.add_lines(lines);
        if !lookup.has_missing() {
            lookup.note_done();
            return Step::Done;
        }
        if let Some((_, failure)) = self.turns.failure_since(count).filter(|(failed, _)| *failed == stay) {
            lookup.fail(failure.clone());
            return Step::Done;
        }
        if !self.turns.take() {
            lookup.await_turn(count);
            return Step::Await;
        }
        lookup.note_done();
        Step::Ask(lookup.missing_names())
    }

    /// Adds a waiter for the asking under way, which its end hands back.
    pub fn add_waiter(&mut self, waiter: W) {
        self.turns.add_waiter(waiter);
    }

    /// Takes the outcome of the asking made in the stay of that number: each name's addresses with the seconds they may be kept from `now`, or why not, in the order of the names asked; returns the waiters to wake.
    pub fn end_asking(
        &mut self,
        lookup: &mut Lookup<E>,
        resolved: Result<Vec<Resolved>, E>,
        stay: u64,
        now: u64,
    ) -> Vec<W> {
        let waiters = self.turns.end(resolved.as_ref().err().map(|failure| (stay, failure.clone())));
        let mut lines = Vec::new();
        let missing = lookup.missing_names();
        let mut keep = |result: Resolved, name: &String| {
            result.map(|(addresses, seconds)| {
                lines.push(format!("names: {name} has {} addresses, to be kept for {seconds} s", addresses.len()));
                self.cache.insert(name, &addresses, seconds, now);
                addresses
            })
        };
        let results = resolved
            .map(|resolved| resolved.into_iter().zip(&missing).map(|(result, name)| keep(result, name)).collect());
        lookup.take_results(results);
        lookup.add_lines(lines);
        waiters
    }
}

impl<E: Clone, W> Default for State<E, W> {
    fn default() -> Self {
        State::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::Unresolved;
    use super::*;
    use alloc::vec;

    const NOW: u64 = 100;
    const STAY: u64 = 1;

    fn address(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    #[test]
    fn kept_names_need_no_turn() {
        let mut state: State<&str, char> = State::new();
        let names = ["a.example"];
        let mut lookup = state.begin(&names, NOW);
        assert_eq!(state.next_step(&mut lookup, STAY, NOW), Step::Ask(vec![String::from("a.example")]));
        let mut other = state.begin(&names, NOW);
        assert_eq!(state.next_step(&mut other, STAY, NOW), Step::Await, "one at a time");
        state.add_waiter('w');
        let waiters = state.end_asking(&mut lookup, Ok(vec![Ok((vec![address(1)], 60))]), STAY, NOW);
        assert_eq!(waiters, ['w'], "the asking's end wakes whoever waits for it");
        assert_eq!(state.next_step(&mut lookup, STAY, NOW), Step::Done);
        assert_eq!(lookup.take_lines(), ["names: a.example has 1 addresses, to be kept for 60 s"]);
        assert_eq!(lookup.into_results(), [Ok(vec![address(1)])]);
        assert_eq!(state.next_step(&mut other, STAY, NOW), Step::Done, "the waiter finds the name kept");
        assert_eq!(other.take_lines(), ["names: a.example is kept for 60 s more"]);
        let mut later = state.begin(&names, NOW + 1);
        assert_eq!(later.take_lines(), ["names: a.example is kept for 59 s more"]);
        assert_eq!(state.next_step(&mut later, STAY, NOW + 1), Step::Done, "and so does whoever comes later");
        let mut literal = state.begin(&["192.0.2.9"], NOW);
        assert_eq!(state.next_step(&mut literal, STAY, NOW), Step::Done, "an address needs no turn");
        assert_eq!(literal.into_results(), [Ok(vec![address(9)])]);
    }

    #[test]
    fn failure_spares_waiter_of_its_stay() {
        let mut state: State<&str, char> = State::new();
        let names = ["a.example"];
        let (mut first, mut second, mut elsewhere) =
            (state.begin(&names, NOW), state.begin(&names, NOW), state.begin(&names, NOW));
        assert_eq!(state.next_step(&mut first, STAY, NOW), Step::Ask(vec![String::from("a.example")]));
        assert_eq!(state.next_step(&mut second, STAY, NOW), Step::Await);
        assert_eq!(state.next_step(&mut elsewhere, STAY + 1, NOW), Step::Await);
        state.end_asking(&mut first, Err("silent"), STAY, NOW);
        assert_eq!(state.next_step(&mut second, STAY, NOW), Step::Done, "told why, it asks nothing");
        assert_eq!(second.into_results(), [Err(Unresolved::Failed("silent"))]);
        assert_eq!(
            state.next_step(&mut elsewhere, STAY + 1, NOW),
            Step::Ask(vec![String::from("a.example")]),
            "a failure of another stay is none of this one's: it asks for itself"
        );
        let mut later = state.begin(&names, NOW);
        assert_eq!(state.next_step(&mut later, STAY, NOW), Step::Await, "and whoever comes after waits its turn");
    }

    #[test]
    fn stay_end_fails_waiter() {
        let mut state: State<&str, char> = State::new();
        let mut lookup = state.begin(&["a.example", "192.0.2.9"], NOW);
        let mut waiter = state.begin(&["a.example"], NOW);
        state.next_step(&mut lookup, STAY, NOW);
        assert_eq!(state.next_step(&mut waiter, STAY, NOW), Step::Await);
        waiter.fail("gone");
        assert_eq!(state.next_step(&mut waiter, STAY, NOW), Step::Done, "a waiter whose stay ended waits no more");
        assert_eq!(waiter.into_results(), [Err(Unresolved::Failed("gone"))]);
        state.end_asking(&mut lookup, Ok(vec![Err(NoAddress::Absent)]), STAY, NOW);
        assert_eq!(
            lookup.into_results(),
            [Err(Unresolved::NoAddress(NoAddress::Absent)), Ok(vec![address(9)])],
            "a name without an address is told so, and nothing is kept of it"
        );
    }
}
