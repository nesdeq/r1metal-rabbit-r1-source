use super::packet::Packet;
use crate::base::time::NANOS_PER_SECOND;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

// libwebrtc's pacer (pacing_controller.cc:43-44, pacing_controller.h:89-104): its debt held under 500 ms at the rate, no more than 2 s reckoned between two sends, a burst of 40 ms (network_types.h:237, `PacerConfig::kDefaultTimeInterval`, which RingRTC leaves as it is) and of 63 000 bytes at most, and a queue drained within 2 s, the rate raised where it would take longer.
const MOST_DEBT: Duration = Duration::from_millis(500);
const MOST_ELAPSED: Duration = Duration::from_secs(2);
const BURST: Duration = Duration::from_millis(40);
const MOST_BURST_BITS: u64 = 63_000 * 8;
pub(super) const QUEUE_LIMIT: Duration = Duration::from_secs(2);
const LEAST_TIME_LEFT: Duration = Duration::from_millis(1);

// The kinds of packets the pacer keeps, in the order it sends them (prioritized_packet_queue.cc, `GetPriorityForType`): audio, and RingRTC's data messages, which go beside the pacer, at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Class {
    Unpaced,
    Retransmission,
    Video,
    KeyFrame,
}

struct Queued {
    packet: Vec<u8>,
    bits: u64,
    at: Duration,
    is_key: bool,
}

// The packets on their way out: the unpaced at once and uncounted (pacing_controller.cc:662-665, 714-721), retransmissions and then video at the pacing rate, each counted by its payload (pacing_controller.cc:499-505).
#[derive(Default)]
pub(super) struct Pacer {
    unpaced: VecDeque<Queued>,
    retransmissions: VecDeque<Queued>,
    video: VecDeque<Queued>,
    rate: u64,
    debt: u64,
    last: Option<Duration>,
    bits: u64,
}

fn bits_in(rate: u64, span: Duration) -> u64 {
    u64::try_from(u128::from(rate) * span.as_nanos() / u128::from(NANOS_PER_SECOND)).unwrap_or(u64::MAX)
}

fn span_of(bits: u64, rate: u64) -> Duration {
    let nanos = u128::from(bits) * u128::from(NANOS_PER_SECOND) / u128::from(rate.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

impl Pacer {
    // Takes the pacing rate in bits a second.
    pub(super) fn set_rate(&mut self, rate: u64) {
        self.rate = rate;
    }

    // Queues an RTP packet; the first packet of a key frame lets go of the video and retransmissions queued before it, so that the key frame goes at once (pacing_controller.cc:203-215).
    pub(crate) fn push(&mut self, class: Class, packet: Vec<u8>, now: Duration) {
        if self.retransmissions.is_empty() && self.video.is_empty() {
            self.reckon(now);
        }
        let is_key = class == Class::KeyFrame;
        if is_key && !self.video.iter().any(|queued| queued.is_key) {
            (self.retransmissions, self.video, self.bits) = (VecDeque::new(), VecDeque::new(), 0);
        }
        let bytes = Packet::read(&packet).map_or(packet.len(), |read| read.payload.len());
        let bits = bytes as u64 * u64::from(u8::BITS);
        let queued = Queued { packet, bits, at: now, is_key };
        match class {
            Class::Unpaced => self.unpaced.push_back(queued),
            Class::Retransmission => self.retransmissions.push_back(queued),
            Class::Video | Class::KeyFrame => self.video.push_back(queued),
        }
        if class != Class::Unpaced {
            self.bits += bits;
        }
    }

    // The next packet to send now, and its kind; none while the debt holds the rest back.
    pub(crate) fn pop(&mut self, now: Duration) -> Option<(Class, Vec<u8>)> {
        self.reckon(now);
        if let Some(queued) = self.unpaced.pop_front() {
            return Some((Class::Unpaced, queued.packet));
        }
        let rate = self.adjusted(now);
        if rate == 0 || self.drain(rate) >= self.burst(rate) {
            return None;
        }
        let (class, queued) = match self.retransmissions.pop_front() {
            Some(queued) => (Class::Retransmission, queued),
            None => {
                let queued = self.video.pop_front()?;
                (if queued.is_key { Class::KeyFrame } else { Class::Video }, queued)
            }
        };
        self.bits -= queued.bits;
        self.debt = (self.debt + queued.bits).min(bits_in(rate, MOST_DEBT));
        Some((class, queued.packet))
    }

    // When the next packet is due: at once for the unpaced, after the debt drains for the rest; none with nothing queued (pacing_controller.cc:326-392).
    pub(super) fn next_time(&self, now: Duration) -> Option<Duration> {
        if !self.unpaced.is_empty() {
            return Some(now);
        }
        if self.retransmissions.is_empty() && self.video.is_empty() {
            return None;
        }
        let rate = self.adjusted(now);
        let drain = self.drain(rate);
        let last = self.last.unwrap_or(now);
        Some(if self.burst(rate) > drain { last } else { last + drain })
    }

    // How long what is queued takes at the pacing rate (pacing_controller.cc, `ExpectedQueueTime`).
    pub(super) fn queue_time(&self, now: Duration) -> Duration {
        if self.bits == 0 {
            return Duration::ZERO;
        }
        match self.adjusted(now) {
            0 => Duration::MAX,
            rate => span_of(self.bits, rate),
        }
    }

    fn drain(&self, rate: u64) -> Duration {
        span_of(self.debt, rate)
    }

    // pacing_controller.cc:361-362: a burst no longer than its bytes at the rate.
    fn burst(&self, rate: u64) -> Duration {
        BURST.min(span_of(MOST_BURST_BITS, rate))
    }

    // The debt paid off for the time since the last reckoning.
    fn reckon(&mut self, now: Duration) {
        if let Some(last) = self.last.filter(|last| now > *last) {
            let paid = bits_in(self.adjusted(now), (now - last).min(MOST_ELAPSED));
            self.debt = self.debt.saturating_sub(paid);
        }
        if self.last.is_none_or(|last| now > last) {
            self.last = Some(now);
        }
    }

    // The pacing rate, raised where the queue would take longer than its limit, by the time its packets already waited (pacing_controller.cc:690-712).
    fn adjusted(&self, now: Duration) -> u64 {
        let count = (self.retransmissions.len() + self.video.len()) as u32;
        if count == 0 {
            return self.rate;
        }
        let waited: Duration =
            self.retransmissions.iter().chain(&self.video).map(|queued| now.saturating_sub(queued.at)).sum();
        let left = QUEUE_LIMIT.saturating_sub(waited / count).max(LEAST_TIME_LEFT);
        let needed = u128::from(self.bits) * u128::from(NANOS_PER_SECOND) / left.as_nanos();
        self.rate.max(u64::try_from(needed).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::rtp::tests::rtp;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn paced_at_the_rate() {
        let mut pacer = Pacer::default();
        pacer.set_rate(1_100_000);
        for sequence in 0..100 {
            pacer.push(Class::Video, rtp(1, sequence, &[0; 1100]), Duration::ZERO);
        }
        let mut sent = 0;
        let mut now = Duration::ZERO;
        while now < ms(500) {
            while pacer.pop(now).is_some() {
                sent += 1;
            }
            now += ms(1);
        }
        assert!(
            (67..=69).contains(&sent),
            "{sent} payloads of 1100 bytes in 500 ms at 1.1 Mbit/s: 62.5, and 5 ahead of the debt in a burst of 40 ms"
        );
    }

    #[test]
    fn audio_first_and_unpaced() {
        let mut pacer = Pacer::default();
        pacer.set_rate(110_000);
        pacer.push(Class::Video, rtp(1, 1, &[1; 1200]), Duration::ZERO);
        pacer.push(Class::Video, rtp(1, 2, &[2; 1200]), Duration::ZERO);
        pacer.push(Class::Retransmission, rtp(2, 1, &[3; 1200]), Duration::ZERO);
        pacer.push(Class::Unpaced, rtp(3, 1, &[4; 100]), Duration::ZERO);
        let class = |popped: Option<(Class, Vec<u8>)>| popped.map(|(class, packet)| (class, packet[12]));
        assert_eq!(class(pacer.pop(Duration::ZERO)), Some((Class::Unpaced, 4)), "audio first");
        assert_eq!(class(pacer.pop(Duration::ZERO)), Some((Class::Retransmission, 3)), "then what is asked again");
        assert_eq!(pacer.pop(Duration::ZERO), None, "the debt of a packet holds the next back");
        let next = pacer.next_time(Duration::ZERO).unwrap();
        assert_eq!(
            next,
            Duration::from_nanos(1200 * 8 * 1_000_000_000 / 110_000),
            "1200 bytes of payload at 110 kbit/s: 87 ms, past the 40 ms burst"
        );
        pacer.push(Class::Unpaced, rtp(3, 2, &[5; 100]), ms(1));
        assert_eq!(pacer.next_time(ms(1)), Some(ms(1)), "audio is due at once");
        assert_eq!(class(pacer.pop(ms(1))), Some((Class::Unpaced, 5)), "audio goes over the debt");
        assert_eq!(pacer.pop(next - ms(40)), None, "not before the debt drains to a burst");
        assert_eq!(class(pacer.pop(next)), Some((Class::Video, 1)));
    }

    #[test]
    fn key_frame_flushes_and_queue_drains() {
        let mut pacer = Pacer::default();
        pacer.set_rate(110_000);
        for sequence in 0..50 {
            pacer.push(Class::Video, rtp(1, sequence, &[0; 1000]), Duration::ZERO);
        }
        assert_eq!(
            pacer.queue_time(Duration::ZERO),
            Duration::from_nanos(400_000 * 1_000_000_000 / 200_000),
            "400 000 bits, which the 2 s limit drains at 200 kbit/s"
        );
        pacer.push(Class::KeyFrame, rtp(1, 50, &[9; 1000]), Duration::ZERO);
        pacer.push(Class::KeyFrame, rtp(1, 51, &[8; 1000]), Duration::ZERO);
        assert_eq!(
            pacer.video.len(),
            2,
            "the key frame's first packet let go of the frames before it, its second of none"
        );
        assert_eq!(pacer.pop(Duration::ZERO).map(|(class, packet)| (class, packet[12])), Some((Class::KeyFrame, 9)));
        for sequence in 52..152 {
            pacer.push(Class::Video, rtp(1, sequence, &[0; 1000]), ms(1));
        }
        let mut now = ms(1);
        while !pacer.video.is_empty() && now < ms(3000) {
            while pacer.pop(now).is_some() {}
            now += ms(1);
        }
        assert!(now <= ms(2010), "808 000 bits, 7.3 s at 110 kbit/s, drained within the 2 s limit: {now:?}");
    }
}
