use super::rtcp::SenderInfo;
use super::{HALF, ntp, units};
use core::time::Duration;

// RFC 3711, 3.3.1: a far side takes the first packet it hears for the first cycle's, so a stream begun below 2^15 is 2^15 packets from its first wrap.
const FIRST_SEQUENCES: u16 = HALF - 1;

// What a stream sent keeps: the next sequence number, the counts a sender report tells, and the last packet's RTP timestamp with the time it stands for.
pub(super) struct Sender {
    pub(super) ssrc: u32,
    rate: u32,
    sequence: u16,
    packets: u32,
    octets: u32,
    last: Option<(u32, Duration)>,
}

impl Sender {
    // A stream of an SSRC and an RTP clock rate, its first sequence number of random bits below 2^15.
    pub(super) fn new(ssrc: u32, rate: u32, random: u16) -> Sender {
        Sender { ssrc, rate, sequence: random & FIRST_SEQUENCES, packets: 0, octets: 0, last: None }
    }

    // The sequence number of the next packet, taken.
    pub(super) fn next_sequence(&mut self) -> u16 {
        let sequence = self.sequence;
        self.sequence = sequence.wrapping_add(1);
        sequence
    }

    // Counts a packet sent of its payload's bytes, its padding left out, and its timestamp with the time the timestamp stands for; the counts wrap as RFC 3550, 6.4.1 has them.
    pub(super) fn note_sent(&mut self, payload: usize, timestamp: u32, sampled: Duration) {
        self.packets = self.packets.wrapping_add(1);
        self.octets = self.octets.wrapping_add(payload as u32);
        self.last = Some((timestamp, sampled));
    }

    // The sender information of a report sent now: the NTP time of the caller's clock and the RTP time the last packet's timestamp tells for it (RFC 3550, 6.4.1); none before a packet went.
    pub(super) fn info(&self, now: Duration) -> Option<SenderInfo> {
        let (timestamp, sampled) = self.last?;
        Some(SenderInfo {
            ntp: ntp(now),
            timestamp: timestamp.wrapping_add(units(now.saturating_sub(sampled), self.rate)),
            packets: self.packets,
            octets: self.octets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_times() {
        let mut sender = Sender::new(1002, 48_000, 0xFFFE);
        assert_eq!((sender.next_sequence(), sender.next_sequence()), (0x7FFE, 0x7FFF), "begun below 2^15");
        assert_eq!(sender.info(Duration::from_secs(1)), None, "no report before a packet");
        sender.note_sent(80, 0xFFFF_FC40, Duration::from_millis(1000));
        sender.note_sent(120, 0xFFFF_FFF0, Duration::from_millis(1020));
        assert_eq!(
            sender.info(Duration::from_millis(1500)),
            Some(SenderInfo { ntp: 1 << 32 | 1 << 31, timestamp: 0x59F0, packets: 2, octets: 200 }),
            "1.5 s in NTP; 480 ms after the last packet's 0xFFFFFFF0 is 23 040 units on, wrapped"
        );
        let mut wrapping = Sender::new(1, 90_000, 0x7FFF);
        wrapping.packets = u32::MAX;
        wrapping.octets = u32::MAX - 9;
        wrapping.note_sent(20, 0, Duration::ZERO);
        let info = wrapping.info(Duration::ZERO).unwrap();
        assert_eq!((info.packets, info.octets), (0, 10), "the counts wrap");
        let mut last = Sender::new(1, 90_000, 0x7FFF);
        last.sequence = u16::MAX;
        assert_eq!((last.next_sequence(), last.next_sequence()), (u16::MAX, 0), "sequence numbers wrap");
    }
}
