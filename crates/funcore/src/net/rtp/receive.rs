use super::rtcp::{MOST_LOSS, ReportBlock, dlsr_of};
use super::{extend, ntp_middle, units};
use core::time::Duration;

// RFC 3550, A.1: a number within MAX_DROPOUT ahead is in order, one up to MAX_MISORDER behind late or a duplicate, any other a jump that only a second in sequence makes the start of the stream anew.
const MAX_DROPOUT: u16 = 3000;
const MAX_MISORDER: u16 = 100;
const LAST_JUMP: u16 = (u16::MAX - MAX_MISORDER) + 1;
const CYCLE: u64 = 1 << u16::BITS;
// A.8: the jitter kept sixteen times over, rounded as it is taken back.
const JITTER_SHIFT: u32 = 4;
const JITTER_ROUND: u64 = 8;
// A.3: the fraction lost, eight bits of it.
const FRACTION_SHIFT: u32 = 8;

// What a stream received keeps for its reports (RFC 3550, A.1, A.3 and A.8).
pub(super) struct Receiver {
    ssrc: u32,
    rate: u32,
    sequence: Option<Sequence>,
    transit: Option<u32>,
    jitter: u64,
    report: Option<(u32, Duration)>,
}

struct Sequence {
    base: u16,
    max: u16,
    cycles: u64,
    bad: Option<u16>,
    received: u64,
    expected_prior: u64,
    received_prior: u64,
}

impl Sequence {
    fn new(first: u16) -> Sequence {
        Sequence { base: first, max: first, cycles: 0, bad: None, received: 0, expected_prior: 0, received_prior: 0 }
    }
}

impl Receiver {
    // A stream of an SSRC and an RTP clock rate.
    pub(super) fn new(ssrc: u32, rate: u32) -> Receiver {
        Receiver { ssrc, rate, sequence: None, transit: None, jitter: 0, report: None }
    }

    // Takes a packet that opened: its extended sequence number, the extension nearest the highest for one late; none for a jump, which only the next number in sequence makes the stream's new start (A.1, `update_seq`).
    pub(crate) fn receive(&mut self, sequence: u16, timestamp: u32, now: Duration) -> Option<u64> {
        let state = self.sequence.get_or_insert_with(|| Sequence::new(sequence));
        let delta = sequence.wrapping_sub(state.max);
        let extended = if delta < MAX_DROPOUT {
            if sequence < state.max {
                state.cycles += CYCLE;
            }
            state.max = sequence;
            state.cycles + u64::from(sequence)
        } else if delta <= LAST_JUMP {
            if state.bad != Some(sequence) {
                state.bad = Some(sequence.wrapping_add(1));
                return None;
            }
            *state = Sequence::new(sequence);
            u64::from(sequence)
        } else {
            extend(sequence, state.cycles + u64::from(state.max))
        };
        state.received += 1;
        let transit = units(now, self.rate).wrapping_sub(timestamp);
        if let Some(previous) = self.transit {
            let difference = u64::from((transit.wrapping_sub(previous) as i32).unsigned_abs());
            self.jitter = self.jitter + difference - ((self.jitter + JITTER_ROUND) >> JITTER_SHIFT);
        }
        self.transit = Some(transit);
        Some(extended)
    }

    // Takes the NTP time of a sender report from this stream's source, received now.
    pub(super) fn note_report(&mut self, ntp: u64, now: Duration) {
        self.report = Some((ntp_middle(ntp), now));
    }

    // The report block of the stream sent now, which begins the next interval of its fraction lost; none before a packet came.
    pub(super) fn block(&mut self, now: Duration) -> Option<ReportBlock> {
        let state = self.sequence.as_mut()?;
        let highest = state.cycles + u64::from(state.max);
        let expected = (highest + 1).saturating_sub(u64::from(state.base));
        let expected_interval = expected.saturating_sub(state.expected_prior);
        let lost_interval = expected_interval as i64 - (state.received - state.received_prior) as i64;
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << FRACTION_SHIFT) / expected_interval as i64).min(i64::from(u8::MAX)) as u8
        };
        (state.expected_prior, state.received_prior) = (expected, state.received);
        let (lsr, dlsr) = self.report.map_or((0, 0), |(lsr, at)| (lsr, dlsr_of(now.saturating_sub(at))));
        Some(ReportBlock {
            ssrc: self.ssrc,
            fraction_lost,
            // None under nought, as the calling server sends, which libwebrtc does not take well (rtcp.rs, `cumulative_packets_lost`).
            cumulative_lost: (expected as i64 - state.received as i64).clamp(0, i64::from(MOST_LOSS)) as i32,
            highest_sequence: highest as u32,
            jitter: u32::try_from(self.jitter >> JITTER_SHIFT).unwrap_or(u32::MAX),
            lsr,
            dlsr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(
        fraction_lost: u8,
        cumulative_lost: i32,
        highest_sequence: u32,
        jitter: u32,
        lsr: u32,
        dlsr: u32,
    ) -> ReportBlock {
        ReportBlock { ssrc: 7, fraction_lost, cumulative_lost, highest_sequence, jitter, lsr, dlsr }
    }

    // A stream of 8000 units a second, a packet every 20 ms, 160 units apart: loss, a late packet, a wrap, a duplicate, then a sender report.
    #[test]
    fn loss_reorder_duplicate_and_wrap() {
        let mut receiver = Receiver::new(7, 8000);
        assert_eq!(receiver.block(Duration::ZERO), None, "no block before a packet");
        let arrivals = [
            (65530, 0, 1000, 65530),
            (65531, 160, 1020, 65531),
            (65533, 480, 1060, 65533),
            (65532, 320, 1065, 65532),
            (65535, 800, 1100, 65535),
            (0, 960, 1120, 65536),
            (0, 960, 1125, 65536),
            (2, 1280, 1160, 65538),
        ];
        for (sequence, timestamp, at, extended) in arrivals {
            assert_eq!(
                receiver.receive(sequence, timestamp, Duration::from_millis(at)),
                Some(extended),
                "packet {sequence} at {at} ms"
            );
        }
        // Transits 8000, 8000, 8000, 8200, 8000, 8000, 8040, 8000: J16 = 0, 0, 200, 387, 363, 380, 396.
        assert_eq!(
            receiver.block(Duration::from_millis(1200)),
            Some(block(28, 1, 0x1_0002, 24, 0, 0)),
            "9 expected, 8 received with the duplicate: 1 lost, 256 / 9; jitter 396 / 16"
        );
        receiver.note_report(0xb44d_b705_2000_0000, Duration::from_millis(1250));
        assert_eq!(receiver.receive(3, 1440, Duration::from_millis(1220)), Some(65539));
        assert_eq!(receiver.receive(5, 1760, Duration::from_millis(1240)), Some(65541));
        // Transits 8320, 8160: J16 = 396 + 320 - 25 = 691, then 691 + 160 - 43 = 808.
        let last = Duration::from_millis(1300);
        assert_eq!(
            receiver.block(last),
            Some(block(85, 2, 0x1_0005, 50, 0xb705_2000, 3276)),
            "3 expected and 2 received since: 256 / 3; 12 and 10 in all; the report's middle bits, 50 ms in 65536ths"
        );
        assert_eq!(receiver.block(last).map(|block| block.fraction_lost), Some(0), "an interval of nothing");
    }

    #[test]
    fn jumps_restarts_and_duplicates() {
        let (mut receiver, now) = (Receiver::new(7, 90_000), Duration::ZERO);
        assert_eq!((receiver.receive(100, 0, now), receiver.receive(101, 0, now)), (Some(100), Some(101)));
        assert_eq!(receiver.receive(5000, 0, now), None, "a jump of 4899 is not taken");
        assert_eq!(receiver.receive(1, 0, now), None, "nor one of 100 behind");
        assert_eq!(receiver.receive(2, 0, now), Some(2), "99 behind is late");
        assert_eq!(receiver.receive(5001, 0, now), None, "5001 does not follow 1");
        assert_eq!(receiver.receive(5002, 0, now), Some(5002), "5002 follows 5001: the stream begins anew");
        assert_eq!(receiver.block(now), Some(block(0, 0, 5002, 0, 0, 0)), "its counts begin anew too");
        assert_eq!(receiver.receive(5004, 0, now), Some(5004));
        for _ in 0..3 {
            receiver.receive(5004, 0, now);
        }
        assert_eq!(
            receiver.block(now).map(|block| (block.fraction_lost, block.cumulative_lost)),
            Some((0, 0)),
            "duplicates outnumber the loss: none under nought"
        );
        let mut far = Receiver::new(7, 90_000);
        far.receive(0, 0, now);
        far.receive(2999, 0, now);
        assert_eq!(far.block(now).map(|block| block.cumulative_lost), Some(2998), "a dropout within 3000");
    }
}
