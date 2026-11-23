//! A server's addresses tried in turn (RFC 8305).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

/// How long an address gets to answer before the next begins beside it: the delay RFC 8305 recommends.
pub const NEXT_ATTEMPT: Duration = Duration::from_millis(250);
/// How long the other side gets for an answer, and for each further piece of what it sends.
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const NOTHING_TO_CONNECT: &str = "no address to connect to";
pub const NOTHING_TO_ASK: &str = "no address to ask";
const ROUTE_WAIT: Duration = Duration::from_secs(4);

/// What a race does next, each time counted from its beginning.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step<A> {
    Begin(A),
    // Waits for the attempts under way until then.
    Wait(Duration),
    // Waits for a route to an address until then.
    AwaitRoutes(Duration),
    // No address is left, with no attempt under way.
    Failed,
    // Nothing was heard for as long as a race may last.
    Silent,
}

/// The addresses of one race, each time counted from its beginning.
pub struct Race<A> {
    addresses: VecDeque<A>,
    unrouted: Vec<A>,
    next_at: Duration,
    has_rerouted: bool,
    attempts: usize,
    is_silent: bool,
}

impl<A: Copy> Race<A> {
    /// A race of those addresses, in the order to try them.
    pub fn new(addresses: &[A]) -> Race<A> {
        Race {
            addresses: addresses.iter().copied().collect(),
            unrouted: Vec::new(),
            next_at: Duration::ZERO,
            has_rerouted: false,
            attempts: 0,
            is_silent: false,
        }
    }

    /// What it does at `now`.
    pub fn next_step(&mut self, now: Duration) -> Step<A> {
        if self.is_silent {
            return Step::Silent;
        }
        if let Some(address) = self.next(now) {
            return Step::Begin(address);
        }
        if self.attempts > 0 {
            return Step::Wait(self.wait_end());
        }
        if self.should_await_routes() { Step::AwaitRoutes(ROUTE_WAIT) } else { Step::Failed }
    }

    fn next(&mut self, now: Duration) -> Option<A> {
        if self.attempts > 0 && now < self.next_at {
            return None;
        }
        self.addresses.pop_front()
    }

    /// Notes an address begun at `now`: the next begins a moment later.
    pub fn note_begun(&mut self, now: Duration) {
        self.attempts += 1;
        self.next_at = now + NEXT_ATTEMPT;
    }

    /// Notes an attempt failed at `now`: the next address begins at once.
    pub fn note_failed(&mut self, now: Duration) {
        self.attempts -= 1;
        self.next_at = now;
    }

    /// Notes a wait that ended with nothing heard at `now`.
    pub const fn note_quiet(&mut self, now: Duration) {
        self.is_silent = is_over(now);
    }

    /// Notes an address no route led to, which begins once more after the routes have changed.
    pub fn add_unrouted(&mut self, address: A) {
        self.unrouted.push(address);
    }

    const fn should_await_routes(&self) -> bool {
        !self.unrouted.is_empty() && !self.has_rerouted
    }

    /// Notes changed routes: the addresses no route led to begin once more.
    pub fn note_rerouted(&mut self) {
        self.has_rerouted = true;
        self.addresses.extend(self.unrouted.drain(..));
    }

    fn wait_end(&self) -> Duration {
        if self.addresses.is_empty() { TIMEOUT } else { self.next_at.min(TIMEOUT) }
    }
}

const fn is_over(now: Duration) -> bool {
    now.as_nanos() >= TIMEOUT.as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOMENT: Duration = Duration::from_millis(10);

    #[test]
    fn attempt_spacing() {
        let mut race = Race::new(&[1, 2, 3]);
        assert_eq!(race.next(Duration::ZERO), Some(1), "with none under way, at once");
        race.note_begun(Duration::ZERO);
        assert_eq!(race.next(MOMENT), None, "the first is given its moment");
        assert_eq!(race.wait_end(), NEXT_ATTEMPT, "and the wait ends when that is over");
        assert_eq!(race.next(NEXT_ATTEMPT), Some(2), "then the next is begun beside it");
        race.note_begun(NEXT_ATTEMPT);
        assert_eq!(race.next(NEXT_ATTEMPT + MOMENT), None, "which is given its moment too");
        assert_eq!(race.next(2 * NEXT_ATTEMPT), Some(3));
        assert_eq!((race.next(TIMEOUT), race.wait_end()), (None, TIMEOUT), "none left: the race's end");
        assert!(!is_over(TIMEOUT - MOMENT) && is_over(TIMEOUT));
    }

    #[test]
    fn steps_wait_for_attempts_then_fail() {
        let mut race = Race::new(&[1, 2]);
        assert_eq!(race.next_step(Duration::ZERO), Step::Begin(1));
        race.note_begun(Duration::ZERO);
        assert_eq!(race.next_step(MOMENT), Step::Wait(NEXT_ATTEMPT));
        race.note_quiet(NEXT_ATTEMPT);
        assert_eq!(race.next_step(NEXT_ATTEMPT), Step::Begin(2), "a quiet wait before the end is no silence");
        race.note_begun(NEXT_ATTEMPT);
        assert_eq!(race.next_step(NEXT_ATTEMPT), Step::Wait(TIMEOUT));
        race.note_quiet(TIMEOUT);
        assert_eq!(race.next_step(TIMEOUT), Step::Silent);
        let mut refused = Race::new(&[1]);
        assert_eq!(refused.next_step(Duration::ZERO), Step::Begin(1));
        refused.note_begun(Duration::ZERO);
        refused.note_failed(MOMENT);
        assert_eq!(refused.next_step(MOMENT), Step::Failed);
    }

    #[test]
    fn steps_await_routes_once() {
        let mut race = Race::new(&[6]);
        assert_eq!(race.next_step(Duration::ZERO), Step::Begin(6));
        race.add_unrouted(6);
        assert_eq!(race.next_step(MOMENT), Step::AwaitRoutes(ROUTE_WAIT));
        race.note_rerouted();
        assert_eq!(race.next_step(MOMENT), Step::Begin(6));
        race.add_unrouted(6);
        assert_eq!(race.next_step(MOMENT), Step::Failed);
    }

    #[test]
    fn failure_begins_next() {
        let mut alone = Race::new(&[1, 2]);
        alone.next(Duration::ZERO);
        alone.note_begun(Duration::ZERO);
        alone.note_failed(MOMENT);
        assert_eq!(alone.next(MOMENT), Some(2), "the one attempt failed: the next begins at once");
        let mut race = Race::new(&[1, 2, 3]);
        race.next(Duration::ZERO);
        race.note_begun(Duration::ZERO);
        race.next(NEXT_ATTEMPT);
        race.note_begun(NEXT_ATTEMPT);
        race.note_failed(NEXT_ATTEMPT + MOMENT);
        assert_eq!(race.next(NEXT_ATTEMPT + MOMENT), Some(3), "and so it does while another is still under way");
    }

    #[test]
    fn unrouted_begin_once_more() {
        let mut race = Race::new(&[6, 4]);
        assert!(!race.should_await_routes(), "nothing to wait for");
        while let Some(address) = race.next(Duration::ZERO) {
            race.add_unrouted(address);
        }
        assert!(race.should_await_routes());
        race.note_rerouted();
        assert_eq!([race.next(MOMENT), race.next(MOMENT)], [Some(6), Some(4)], "in their order once more");
        race.add_unrouted(6);
        assert!(!race.should_await_routes(), "once a race");
    }
}
