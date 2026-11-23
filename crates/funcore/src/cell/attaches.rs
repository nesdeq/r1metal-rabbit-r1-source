//! The record of the last day's attaches, which the log tells as the network refuses a connection.

use crate::base::time::SECONDS_PER_DAY;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The attaches of the last day in `cell::DIRECTORY`, one second of UTC a line.
pub const FILE: &str = "attaches";

/// The attaches of the last day, in seconds of UTC.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Attaches(Vec<u64>);

impl Attaches {
    /// The record in the file's text; a line that does not read is passed over.
    pub fn parse(text: &str) -> Attaches {
        Attaches(text.lines().filter_map(|line| line.trim().parse().ok()).collect())
    }

    pub fn to_text(&self) -> String {
        self.0.iter().map(|second| format!("{second}\n")).collect()
    }

    // Notes an attach at `now`, forgetting those a day before.
    pub(super) fn note(&mut self, now: u64) {
        self.0.retain(|&second| second + SECONDS_PER_DAY > now && second <= now);
        self.0.push(now);
    }

    // The attaches of the day up to `now`.
    pub(super) fn count(&self, now: u64) -> usize {
        self.0.iter().filter(|&&second| second + SECONDS_PER_DAY > now && second <= now).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaches_of_the_day() {
        let mut attaches = Attaches::default();
        let start = 1_800_000_000;
        for at in 0..10 {
            assert_eq!(attaches.count(start + at), at as usize);
            attaches.note(start + at);
        }
        assert_eq!(attaches.count(start + 200), 10, "the day's are counted");
        assert_eq!(attaches.count(start + SECONDS_PER_DAY), 9, "and fall out a day after each");
        attaches.note(start + SECONDS_PER_DAY + 5);
        assert_eq!(attaches.0.len(), 5, "those a day old are forgotten as one is noted");
        assert_eq!(Attaches::parse(&attaches.to_text()), attaches);
        assert_eq!(Attaches::parse("1\nx\n2\n").0, [1, 2]);
    }
}
