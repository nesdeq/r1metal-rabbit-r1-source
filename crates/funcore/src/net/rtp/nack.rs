use super::{FIRST_CYCLE, extend};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

// The calling server's own NACKs: at most 250 missing a stream, as a kilobyte of NACK carries (rtp.rs, `SsrcState`); given up three seconds after the first or after ten (nack.rs, `send_nacks`); asked again a round trip and a tenth after the last (connection.rs, `RTT_GRACE_MULTIPLIER`).
const MOST_MISSING: usize = 250;
const GIVE_UP_AFTER: Duration = Duration::from_secs(3);
const MOST_TRIES: u8 = 10;
const GRACE_TENTHS: u32 = 11;
const TENTHS: u32 = 10;

// The packets of a stream received that are missing, by their extended sequence numbers, and when they were asked for.
#[derive(Default)]
pub(super) struct Missing {
    highest: Option<u64>,
    gaps: VecDeque<Gap>,
}

struct Gap {
    sequence: u64,
    first: Option<Duration>,
    last: Duration,
    tries: u8,
}

impl Missing {
    // Takes a packet's sequence number, extended by those taken before it: the numbers skipped are missing, and this one no more.
    pub(crate) fn receive(&mut self, sequence: u16) {
        let first = FIRST_CYCLE << u16::BITS | u64::from(sequence);
        let sequence = self.highest.map_or(first, |highest| extend(sequence, highest));
        let Some(highest) = self.highest.filter(|highest| sequence > *highest) else {
            self.highest = self.highest.or(Some(sequence));
            if let Ok(at) = self.gaps.binary_search_by_key(&sequence, |gap| gap.sequence) {
                self.gaps.remove(at);
            }
            return;
        };
        let skipped = (sequence - highest - 1).min(MOST_MISSING as u64);
        let gaps = (sequence - skipped..sequence).map(|sequence| Gap {
            sequence,
            first: None,
            last: Duration::ZERO,
            tries: 0,
        });
        self.gaps.extend(gaps);
        if let Some(over) = self.gaps.len().checked_sub(MOST_MISSING) {
            self.gaps.drain(..over);
        }
        self.highest = Some(sequence);
    }

    // The sequence numbers to ask for now, in order: those never asked for and those asked for a round trip ago, those asked for too long or too often given up.
    pub(super) fn due(&mut self, now: Duration, round_trip: Duration) -> Vec<u16> {
        let again = round_trip * GRACE_TENTHS / TENTHS;
        self.gaps.retain(|gap| {
            gap.tries < MOST_TRIES && gap.first.is_none_or(|first| now.saturating_sub(first) < GIVE_UP_AFTER)
        });
        let mut due = Vec::new();
        for gap in self.gaps.iter_mut() {
            if gap.first.is_none() || now.saturating_sub(gap.last) >= again {
                gap.first.get_or_insert(now);
                (gap.last, gap.tries) = (now, gap.tries + 1);
                due.push(gap.sequence as u16);
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asked_again_and_given_up() {
        let (mut missing, round_trip) = (Missing::default(), Duration::from_millis(100));
        missing.receive(3);
        missing.receive(4);
        assert!(missing.due(Duration::ZERO, round_trip).is_empty(), "nothing skipped");
        missing.receive(6);
        assert_eq!(missing.due(Duration::from_millis(10), round_trip), [5], "a gap is asked for at once");
        assert!(
            missing.due(Duration::from_millis(119), round_trip).is_empty(),
            "not again within a round trip and a tenth"
        );
        missing.receive(9);
        assert_eq!(
            missing.due(Duration::from_millis(120), round_trip),
            [5, 7, 8],
            "5 again after 110 ms, 7 and 8 at once"
        );
        missing.receive(7);
        assert_eq!(missing.due(Duration::from_millis(300), round_trip), [5, 8], "what came is asked no more");
        assert_eq!(missing.due(Duration::from_millis(3009), round_trip), [5, 8]);
        assert_eq!(
            missing.due(Duration::from_millis(3010), Duration::ZERO),
            [8],
            "5 given up three seconds after it was first asked for"
        );
        missing.receive(8);
        missing.receive(5);
        assert!(missing.due(Duration::from_secs(4), round_trip).is_empty());
    }

    #[test]
    fn tries_and_bounds() {
        let mut missing = Missing::default();
        missing.receive(0xFFFE);
        missing.receive(1);
        let asked: Vec<Vec<u16>> =
            (0..11).map(|step| missing.due(Duration::from_millis(step * 10), Duration::ZERO)).collect();
        assert_eq!(asked[0], [0xFFFF, 0], "across the wrap, by their 16 bits");
        assert!(asked[1..10].iter().all(|due| due == &[0xFFFF, 0]) && asked[10].is_empty(), "ten tries, then given up");
        missing.receive(1001);
        let due = missing.due(Duration::from_millis(200), Duration::ZERO);
        assert_eq!(due.len(), MOST_MISSING, "no more missing are kept than one NACK packet carries");
        assert_eq!((due[0], due[MOST_MISSING - 1]), (751, 1000), "the latest");
        let mut first = Missing::default();
        first.receive(10);
        first.receive(9);
        assert!(first.due(Duration::ZERO, Duration::ZERO).is_empty(), "what came before the first is not missing");
    }
}
