use super::HALF;
use super::packet::Packet;
use super::rtx;
use super::send::Sender;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

// As long as the calling server keeps packets to resend, which it has from libwebrtc's history (rtp.rs, `PACKET_LIFETIME`).
const LIFETIME: Duration = Duration::from_secs(3);
// The most a call sends of all its media, 2 Mbit/s (RingRTC's common/mod.rs, `DataMode::max_bitrate` of `Normal`), for that long.
const MOST_BYTES: usize = 2_000_000 / 8 * 3;
// A NACK names a packet by 16 bits; half their space keeps a stream's old numbers apart from its new (RFC 3711, appendix A).
const MOST_PACKETS: usize = HALF as usize;

// The packets of a stream sent, by sequence number, as they went before their protection, from which a NACK is answered.
#[derive(Default)]
pub(super) struct History {
    packets: VecDeque<Kept>,
    bytes: usize,
}

struct Kept {
    sequence: u16,
    sent: Duration,
    resent: Option<Duration>,
    packet: Vec<u8>,
}

impl History {
    // Keeps a packet sent, the stream's packets in the order of their sequence numbers; one that does not follow the last begins the history anew, and what is too old or too much is let go, its buffer kept for the next.
    pub(super) fn keep(&mut self, packet: &[u8], now: Duration) {
        let Some(sequence) = Packet::read(packet).map(|read| read.sequence).filter(|_| packet.len() <= MOST_BYTES)
        else {
            return;
        };
        if self.packets.back().is_some_and(|last| last.sequence.wrapping_add(1) != sequence) {
            (self.packets, self.bytes) = (VecDeque::new(), 0);
        }
        let mut spare = Vec::new();
        while let Some(oldest) = self.packets.front() {
            let is_kept = now.saturating_sub(oldest.sent) < LIFETIME
                && self.bytes + packet.len() <= MOST_BYTES
                && self.packets.len() < MOST_PACKETS;
            if is_kept {
                break;
            }
            if let Some(oldest) = self.packets.pop_front() {
                self.bytes -= oldest.packet.len();
                spare = oldest.packet;
            }
        }
        spare.clear();
        spare.extend_from_slice(packet);
        self.bytes += packet.len();
        self.packets.push_back(Kept { sequence, sent: now, resent: None, packet: spare });
    }

    // The packet of a sequence number to send again as it went: a plain retransmission, which is to be protected unchanged under its index and sent as it was; none where it is let go, older than the lifetime or resent less than a round trip ago.
    pub(super) fn resend(&mut self, sequence: u16, now: Duration, round_trip: Duration) -> Option<&[u8]> {
        let first = self.packets.front()?.sequence;
        let kept = self.packets.get_mut(usize::from(sequence.wrapping_sub(first)))?;
        let is_due = now.saturating_sub(kept.sent) < LIFETIME
            && kept.resent.is_none_or(|resent| now.saturating_sub(resent) >= round_trip);
        if !is_due {
            return None;
        }
        kept.resent = Some(now);
        Some(&kept.packet)
    }

    // Appends the RTX packet (RFC 4588) of a sequence number under the retransmission stream's next sequence number, its SSRC and a payload type; false as `resend` gives none.
    pub(super) fn resend_rtx(
        &mut self,
        sequence: u16,
        now: Duration,
        round_trip: Duration,
        stream: &mut Sender,
        payload_type: u8,
        out: &mut Vec<u8>,
    ) -> bool {
        let Some(original) = self.resend(sequence, now, round_trip) else {
            return false;
        };
        rtx::wrap(original, stream.ssrc, payload_type, stream.next_sequence(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::rtp::tests::rtp;

    #[test]
    fn answers_by_sequence_number() {
        let mut history = History::default();
        let packet = |sequence| rtp(0x0102, sequence, &[7; 88]);
        for sequence in 0xFFFE..=0xFFFF {
            history.keep(&packet(sequence), Duration::ZERO);
        }
        history.keep(&packet(0), Duration::from_millis(10));
        let (round_trip, at) = (Duration::from_millis(50), Duration::from_millis(70));
        assert_eq!(
            history.resend(0xFFFF, Duration::from_millis(20), round_trip),
            Some(&packet(0xFFFF)[..]),
            "across the wrap"
        );
        assert_eq!(
            history.resend(0xFFFF, Duration::from_millis(60), round_trip),
            None,
            "not again within a round trip"
        );
        assert!(history.resend(0xFFFF, at, round_trip).is_some(), "again after one");
        assert_eq!(history.resend(1, at, round_trip), None, "not sent yet");
        assert_eq!(history.resend(0xFFFD, at, round_trip), None, "before the history");
        assert_eq!(history.resend(0, Duration::from_millis(3010), round_trip), None, "older than three seconds");
        let mut rtx_stream = Sender::new(0x0103, 90_000, 5);
        let mut out = Vec::new();
        assert!(history.resend_rtx(0xFFFE, Duration::from_millis(30), round_trip, &mut rtx_stream, 118, &mut out));
        assert_eq!(
            (&out[..4], &out[8..14], out.len()),
            (&[0x80, 118, 0, 5][..], &[0, 0, 1, 3, 0xFF, 0xFE][..], 102),
            "PT 118, the RTX stream's first sequence number and SSRC, the original sequence number"
        );
        assert_eq!(rtx_stream.next_sequence(), 6, "the RTX stream's number taken");
        assert!(!history.resend_rtx(0xFFFE, Duration::from_millis(31), round_trip, &mut rtx_stream, 118, &mut out));
    }

    #[test]
    fn bounded_and_begun_anew() {
        let mut history = History::default();
        for sequence in 0..4 {
            history.keep(&rtp(1, sequence, &[]), Duration::from_secs(u64::from(sequence)));
        }
        assert_eq!(history.packets.len(), 3, "the packet of three seconds ago let go");
        history.keep(&rtp(1, 9, &[]), Duration::from_secs(3));
        assert_eq!(history.packets.len(), 1, "a jump begins anew");
        let mut full = History::default();
        let big = MOST_BYTES / 1200 + 1;
        for sequence in 0..big as u16 {
            full.keep(&rtp(1, sequence, &[7; 1188]), Duration::ZERO);
        }
        assert!(full.bytes <= MOST_BYTES && full.packets.len() == big - 1, "no more than three seconds of 2 Mbit/s");
        let mut many = History::default();
        for sequence in 0..=MOST_PACKETS as u16 {
            many.keep(&rtp(1, sequence, &[]), Duration::ZERO);
        }
        assert_eq!(many.packets.len(), MOST_PACKETS, "nor more than half the space of sequence numbers");
        assert_eq!(many.resend(0, Duration::ZERO, Duration::ZERO), None);
        assert!(many.resend(MOST_PACKETS as u16, Duration::ZERO, Duration::ZERO).is_some());
        let mut none = History::default();
        none.keep(&[0x80, 1], Duration::ZERO);
        assert!(none.packets.is_empty(), "what is no packet is not kept");
    }
}
