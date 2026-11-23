//! A join's proof that its network reaches the resolvers every name is asked of: each is asked for a connection at once, and the first that answers proves it.

use crate::net::dns::PORT;
use crate::net::dns::RESOLVERS;
use crate::wifi::time_left;
use alloc::format;
use alloc::string::String;
use core::net::SocketAddr;
use core::time::Duration;

/// The resolvers' addresses, at the port names are asked on.
pub fn to_resolvers() -> [SocketAddr; RESOLVERS.len()] {
    RESOLVERS.map(|resolver| SocketAddr::new(resolver, PORT))
}

/// What a proof does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
    Wait(Duration),
    Reached,
    // No resolver answered: none could be asked, every asking failed, or the limit came.
    Unreached,
}

/// A proof under way: the resolvers asked and not heard of yet, and whether one answered.
#[derive(Default)]
pub struct Proof {
    asked: usize,
    is_reached: bool,
}

impl Proof {
    /// Notes a resolver asked for a connection.
    pub const fn note_asked(&mut self) {
        self.asked += 1;
    }

    /// Notes how the asking of a resolver ended: it answered, or its connection failed.
    pub const fn note_end(&mut self, is_answered: bool) {
        self.asked = self.asked.saturating_sub(1);
        self.is_reached |= is_answered;
    }

    /// What it does `elapsed` after it began, with `limit` for an answer.
    pub fn next(&self, elapsed: Duration, limit: Duration) -> Next {
        match (self.is_reached, self.asked, time_left(elapsed, limit)) {
            (true, ..) => Next::Reached,
            (false, 0, _) | (false, _, None) => Next::Unreached,
            (false, _, Some(left)) => Next::Wait(left),
        }
    }
}

/// The log line of the resolver at `address` answering `ms` after the proof began.
pub fn to_reached_line(address: &str, ms: u128) -> String {
    format!("wifi: the resolver at {address} answered after {ms} ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(1);
    const MOMENT: Duration = Duration::from_millis(20);

    #[test]
    fn resolvers_at_the_names_port() {
        let resolvers = to_resolvers();
        assert!(resolvers.iter().all(|resolver| resolver.port() == PORT));
        assert_eq!(resolvers.map(|resolver| resolver.ip()), RESOLVERS, "every one a name could be asked of");
    }

    #[test]
    fn first_answer_proves() {
        let mut proof = Proof::default();
        assert_eq!(proof.next(Duration::ZERO, LIMIT), Next::Unreached, "nobody could be asked");
        proof.note_asked();
        proof.note_asked();
        assert_eq!(proof.next(MOMENT, LIMIT), Next::Wait(LIMIT - MOMENT), "the answer is waited for up to the limit");
        proof.note_end(false);
        assert_eq!(proof.next(MOMENT, LIMIT), Next::Wait(LIMIT - MOMENT), "one that refused leaves the other");
        proof.note_end(true);
        assert_eq!(proof.next(MOMENT, LIMIT), Next::Reached);
        assert_eq!(proof.next(LIMIT, LIMIT), Next::Reached, "an answer that came counts, whatever the time");
    }

    #[test]
    fn silence_and_refusals_prove_nothing() {
        let mut silent = Proof::default();
        silent.note_asked();
        assert_eq!(silent.next(LIMIT, LIMIT), Next::Unreached, "nothing heard by the limit");
        let mut refused = Proof::default();
        refused.note_asked();
        refused.note_asked();
        refused.note_end(false);
        refused.note_end(false);
        assert_eq!(refused.next(MOMENT, LIMIT), Next::Unreached, "every asking failed: no wait for the limit");
        assert_eq!(to_reached_line("1.1.1.1", 23), "wifi: the resolver at 1.1.1.1 answered after 23 ms");
    }
}
