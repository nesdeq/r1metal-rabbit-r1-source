use super::packet::set_transport_sequence;
use super::rtcp::{RTPFB, begin, finish};
use super::{HALF, extend};
use crate::base::bits::{BitReader, BitWriter};
use crate::base::bytes::{be16, be32};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

// draft-holmer-rmcat-transport-wide-cc-extensions-01, 3.1: the format, and the base sequence number, status count, 24-bit reference time and feedback count ahead of the chunks.
pub(super) const FORMAT: u8 = 15;
const HEAD: usize = 8;
const REFERENCE_SHIFT: u32 = 8;
// 3.1 and 3.1.5: the reference time counts in 64 ms, a delta in 250 microseconds.
const REFERENCE_MICROS: i128 = 64_000;
const DELTA_MICROS: i128 = 250;
const DELTAS_PER_REFERENCE: i128 = REFERENCE_MICROS / DELTA_MICROS;
const REFERENCE_BITS: u32 = 0x00FF_FFFF;
// 3.1.1: the symbols of a packet's status.
const NOT_RECEIVED: u8 = 0;
const SMALL: u8 = 1;
const LARGE: u8 = 2;
// 3.1.3 and 3.1.4: a chunk's type bit, then a symbol and a run of 13 bits, or the symbols' width and 14 bits of them.
const CHUNK_BYTES: usize = 2;
const RUN: u32 = 0;
const VECTOR: u32 = 1;
const TYPE_BITS: u32 = 1;
const SYMBOL_BITS: u32 = 2;
const RUN_BITS: u32 = 13;
const LIST_BITS: u32 = 14;
const MOST_RUN: usize = (1 << RUN_BITS) - 1;
const NARROW_SYMBOLS: usize = 14;
const WIDE_SYMBOLS: usize = 7;
// 3.1.5: a small delta is a byte, unsigned; a large one two, signed.
const SMALL_DELTAS: core::ops::RangeInclusive<i128> = 0..=u8::MAX as i128;
const LARGE_DELTAS: core::ops::RangeInclusive<i128> = i16::MIN as i128..=i16::MAX as i128;
// The calling server's bound on one message's chunks and deltas (transportcc.rs, `write_feedback`); a packet received takes a delta's byte at the least, so no more arrivals are kept than one message carries.
const MOST_BYTES: usize = 1100;
const MOST_ARRIVALS: usize = MOST_BYTES;
// A sender keeps its departures ten seconds, as the calling server keeps its own (transportcc.rs, `Sender::new`), and no more than half the space of the numbers, which feedback names by 16 bits; its first number is the server's, 1.
const DEPARTURE_LIFETIME: Duration = Duration::from_secs(10);
const MOST_DEPARTURES: usize = HALF as usize;
const FIRST_NUMBER: u64 = 1;

// A transport-wide feedback message read in place, its chunks and deltas checked whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Feedback<'a> {
    sender: u32,
    media: u32,
    base: u16,
    count: u16,
    // In 64 ms, signed.
    reference: i32,
    feedback_count: u8,
    chunks: &'a [u8],
    deltas: &'a [u8],
}

// A chunk being read: a run of one symbol, or a vector of symbols of a width.
#[derive(Clone)]
enum Chunk<'a> {
    Run { symbol: u8, left: u32 },
    Vector { list: BitReader<'a>, width: u32, left: u32 },
}

// The symbols of the chunks, as many as the status count, those a vector holds past it unread.
#[derive(Clone)]
struct Symbols<'a> {
    chunks: &'a [u8],
    chunk: Option<Chunk<'a>>,
    left: u16,
}

// Each packet's sequence number and its arrival in microseconds of the receiver's clock; none for one not received.
pub(super) struct Statuses<'a> {
    symbols: Symbols<'a>,
    deltas: &'a [u8],
    sequence: u16,
    time: i64,
}

// The arrivals a receiver keeps of packets with a transport-wide sequence number, until it feeds them back.
#[derive(Default)]
pub(super) struct Recorder {
    highest: Option<u64>,
    arrivals: VecDeque<(u64, Duration)>,
    count: u8,
}

// The packets a sender sent with a transport-wide sequence number, which it numbers, each kept with its bytes and its departure until feedback tells of it.
pub(super) struct Departures {
    next: u64,
    sent: VecDeque<Departure>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Departure {
    number: u64,
    bytes: usize,
    at: Duration,
}

// What feedback told of a packet sent: when it left and its bytes, and its arrival in microseconds of the receiver's clock, none where it was lost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Outcome {
    pub(super) sent: Duration,
    pub(super) bytes: usize,
    pub(super) arrival: Option<i64>,
}

impl Default for Departures {
    fn default() -> Departures {
        Departures { next: FIRST_NUMBER, sent: VecDeque::new() }
    }
}

impl<'a> Chunk<'a> {
    fn read(bytes: &'a [u8]) -> Option<Chunk<'a>> {
        let mut reader = BitReader::new(bytes);
        if reader.take(TYPE_BITS)? == RUN {
            return Some(Chunk::Run { symbol: reader.take(SYMBOL_BITS)? as u8, left: reader.take(RUN_BITS)? });
        }
        let width = reader.take(TYPE_BITS)? + 1;
        Some(Chunk::Vector { list: reader, width, left: LIST_BITS / width })
    }

    fn next_symbol(&mut self) -> Option<u8> {
        match self {
            Chunk::Run { left: 0, .. } | Chunk::Vector { left: 0, .. } => None,
            Chunk::Run { symbol, left } => {
                *left -= 1;
                Some(*symbol)
            }
            Chunk::Vector { list, width, left } => {
                *left -= 1;
                Some(list.take(*width)? as u8)
            }
        }
    }
}

impl<'a> Iterator for Symbols<'a> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.left == 0 {
            return None;
        }
        loop {
            if let Some(symbol) = self.chunk.as_mut().and_then(Chunk::next_symbol) {
                self.left -= 1;
                return Some(symbol);
            }
            let (bytes, rest) = self.chunks.split_at_checked(CHUNK_BYTES)?;
            (self.chunk, self.chunks) = (Chunk::read(bytes), rest);
        }
    }
}

impl<'a> Feedback<'a> {
    // None where the chunks hold fewer statuses than the count, a status is the reserved symbol, or the deltas do not fit.
    pub(super) fn read(sender: u32, media: u32, fci: &'a [u8]) -> Option<Feedback<'a>> {
        let (base, count, word) = (be16(fci, 0)?, be16(fci, 2)?, be32(fci, 4)?);
        let area = fci.get(HEAD..)?;
        let mut symbols = Symbols { chunks: area, chunk: None, left: count };
        let mut delta_bytes = 0;
        for symbol in symbols.by_ref() {
            delta_bytes += match symbol {
                NOT_RECEIVED => 0,
                SMALL => 1,
                LARGE => 2,
                _ => return None,
            };
        }
        let chunks = area.len() - symbols.chunks.len();
        Some(Feedback {
            sender,
            media,
            base,
            count,
            reference: word as i32 >> REFERENCE_SHIFT,
            feedback_count: word as u8,
            chunks: &area[..chunks],
            deltas: area.get(chunks..chunks + delta_bytes).filter(|_| symbols.left == 0)?,
        })
    }

    pub(super) fn statuses(&self) -> Statuses<'a> {
        Statuses {
            symbols: Symbols { chunks: self.chunks, chunk: None, left: self.count },
            deltas: self.deltas,
            sequence: self.base,
            time: i64::from(self.reference) * REFERENCE_MICROS as i64,
        }
    }
}

impl Iterator for Statuses<'_> {
    type Item = (u16, Option<i64>);

    fn next(&mut self) -> Option<(u16, Option<i64>)> {
        let symbol = self.symbols.next()?;
        let sequence = self.sequence;
        self.sequence = sequence.wrapping_add(1);
        let (delta, rest) = match symbol {
            SMALL => (i64::from(*self.deltas.first()?), self.deltas.get(1..)?),
            LARGE => (i64::from(be16(self.deltas, 0)? as i16), self.deltas.get(2..)?),
            _ => return Some((sequence, None)),
        };
        self.deltas = rest;
        self.time += delta * DELTA_MICROS as i64;
        Some((sequence, Some(self.time)))
    }
}

// Writes statuses as chunks: a run of one symbol where seven or more follow or it ends them, else a vector of fourteen narrow symbols where they hold no large delta, else of seven wide; every chunk but the last takes seven statuses at the least.
fn write_chunks(statuses: &[u8], writer: &mut BitWriter) {
    let mut at = 0;
    while let Some(&symbol) = statuses.get(at) {
        let run = statuses[at..].iter().take(MOST_RUN).take_while(|other| **other == symbol).count();
        if run >= WIDE_SYMBOLS || at + run == statuses.len() {
            writer.put(RUN, TYPE_BITS);
            writer.put(u32::from(symbol), SYMBOL_BITS);
            writer.put(run as u32, RUN_BITS);
            at += run;
            continue;
        }
        let narrow = &statuses[at..statuses.len().min(at + NARROW_SYMBOLS)];
        let (taken, width) = if narrow.contains(&LARGE) {
            (&statuses[at..statuses.len().min(at + WIDE_SYMBOLS)], SYMBOL_BITS)
        } else {
            (narrow, 1)
        };
        writer.put(VECTOR, TYPE_BITS);
        writer.put(width - 1, TYPE_BITS);
        taken.iter().for_each(|symbol| writer.put(u32::from(*symbol), width));
        writer.put(0, LIST_BITS - taken.len() as u32 * width);
        at += taken.len();
    }
}

impl Recorder {
    pub(super) fn note(&mut self, sequence: u16, at: Duration) {
        let extended = self.highest.map_or(u64::from(sequence), |highest| extend(sequence, highest));
        self.highest = Some(self.highest.map_or(extended, |highest| highest.max(extended)));
        if let Err(place) = self.arrivals.binary_search_by_key(&extended, |(sequence, _)| *sequence) {
            self.arrivals.insert(place, (extended, at));
            if self.arrivals.len() > MOST_ARRIVALS {
                self.arrivals.pop_front();
            }
        }
    }

    pub(super) fn has_arrivals(&self) -> bool {
        !self.arrivals.is_empty()
    }

    // Appends one feedback message of the arrivals kept, the earliest sequence number its base, as many as one message carries: up to the status count's 16 bits, a delta too wide or the server's bound on its bytes; false where none are kept.
    pub(crate) fn write(&mut self, out: &mut Vec<u8>, sender: u32, media: u32) -> bool {
        let Some(&(base, first)) = self.arrivals.front() else {
            return false;
        };
        let reference = first.as_micros() as i128 / REFERENCE_MICROS;
        let mut position = reference * DELTAS_PER_REFERENCE;
        let (mut statuses, mut deltas, mut taken) = (Vec::new(), Vec::new(), 0);
        for &(sequence, at) in &self.arrivals {
            let span = (sequence - base + 1) as usize;
            let tick = at.as_micros() as i128 / DELTA_MICROS;
            let delta = tick - position;
            let width = if SMALL_DELTAS.contains(&delta) { 1 } else { 2 };
            let chunks_most = CHUNK_BYTES * (span / WIDE_SYMBOLS + 1);
            if span > usize::from(u16::MAX)
                || !LARGE_DELTAS.contains(&delta)
                || chunks_most + deltas.len() + width > MOST_BYTES
            {
                break;
            }
            statuses.resize(span - 1, NOT_RECEIVED);
            if width == 1 {
                statuses.push(SMALL);
                deltas.push(delta as u8);
            } else {
                statuses.push(LARGE);
                deltas.extend((delta as i16).to_be_bytes());
            }
            (position, taken) = (tick, taken + 1);
        }
        let start = begin(out, FORMAT, RTPFB);
        out.extend(sender.to_be_bytes());
        out.extend(media.to_be_bytes());
        out.extend((base as u16).to_be_bytes());
        out.extend((statuses.len() as u16).to_be_bytes());
        out.extend(((reference as u32 & REFERENCE_BITS) << REFERENCE_SHIFT | u32::from(self.count)).to_be_bytes());
        let mut writer = BitWriter::with_bytes(core::mem::take(out));
        write_chunks(&statuses, &mut writer);
        *out = writer.finish();
        out.extend(deltas);
        if !finish(out, start) {
            return false;
        }
        self.arrivals.drain(..taken);
        self.count = self.count.wrapping_add(1);
        true
    }
}

impl Departures {
    // Writes the next number into a packet that carries the element for it as the packet leaves, and keeps its departure of `bytes` on the wire; false where it carries none.
    pub(crate) fn send(&mut self, packet: &mut [u8], bytes: usize, now: Duration) -> bool {
        if !set_transport_sequence(packet, self.next as u16) {
            return false;
        }
        while self.sent.front().is_some_and(|oldest| {
            now.saturating_sub(oldest.at) > DEPARTURE_LIFETIME || self.sent.len() >= MOST_DEPARTURES
        }) {
            self.sent.pop_front();
        }
        self.sent.push_back(Departure { number: self.next, bytes, at: now });
        self.next += 1;
        true
    }

    // The outcome of each packet a feedback message tells of and this side still keeps, in the order of their numbers; those it told of are let go.
    pub(super) fn outcomes(&mut self, feedback: &Feedback<'_>) -> Vec<Outcome> {
        let highest = self.next - 1;
        let mut outcomes = Vec::new();
        let mut last = None;
        for (sequence, arrival) in feedback.statuses() {
            let number = extend(sequence, highest);
            if let Ok(at) = self.sent.binary_search_by_key(&number, |departure| departure.number) {
                let departure = self.sent[at];
                outcomes.push(Outcome { sent: departure.at, bytes: departure.bytes, arrival });
                last = Some(at);
            }
        }
        if let Some(at) = last {
            self.sent.drain(..=at);
        }
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::rtp::rtcp::tests::one;
    use crate::net::rtp::rtcp::{Packet, read};
    use crate::net::rtp::tests::bytes;
    use alloc::vec;

    fn feedback(datagram: &[u8]) -> Feedback<'_> {
        let Packet::TransportFeedback(feedback) = one(datagram) else { panic!("a transport-wide feedback") };
        feedback
    }

    // Derived by hand from the draft: the reference 100 (6.4 s); 1000 a millisecond on, 1001 lost, 1002 100 ms later, 1003 a millisecond before it, ten more 250 microseconds apart.
    #[test]
    fn hand_derived_message() {
        let mut recorder = Recorder::default();
        recorder.note(1000, Duration::from_micros(6_401_000));
        recorder.note(1002, Duration::from_micros(6_501_000));
        recorder.note(1003, Duration::from_micros(6_500_000));
        for step in 1..=10 {
            recorder.note(1003 + step as u16, Duration::from_micros(6_500_000 + 250 * step));
        }
        let mut out = Vec::new();
        assert!(recorder.write(&mut out, 1, 1002));
        assert_eq!(
            out,
            bytes("8fcd0009 00000001 000003ea 03e8000e 00006400 d295 2007 04 0190 fffc 01010101 01010101 0101 00"),
            "base 1000, 14 statuses, reference 100; a vector of seven wide symbols (small, lost, large, large, small, small, small), a run of seven small; deltas 4, 400, -4 and ten of 1"
        );
        let read = feedback(&out);
        assert_eq!(
            (read.sender, read.media, read.base, read.count, read.reference, read.feedback_count),
            (1, 1002, 1000, 14, 100, 0)
        );
        let mut expected =
            vec![(1000, Some(6_401_000)), (1001, None), (1002, Some(6_501_000)), (1003, Some(6_500_000))];
        expected.extend((1..=10).map(|step| (1003 + step as u16, Some(6_500_000 + 250 * step))));
        assert_eq!(read.statuses().collect::<Vec<_>>(), expected, "each arrival back to the microsecond");
        assert!(!recorder.write(&mut out, 1, 1002), "nothing more to feed back");
    }

    #[test]
    fn chunks_of_the_draft() {
        // 3.1.3 example 1, a run of 221 not received; 3.1.4 example 1, a vector of one bit each: 0 1 1 1 1 1 0 0 0 1 1 1 0 0.
        let message = bytes("8fcd0007 00000001 00000002 000000eb 00000000 00dd 9f1c 0102030405060708");
        let statuses: Vec<_> = feedback(&message).statuses().collect();
        assert_eq!(statuses.len(), 235);
        assert!(statuses[..222].iter().all(|(_, arrival)| arrival.is_none()), "221 lost, then the vector's first");
        let received: Vec<_> =
            statuses.iter().filter_map(|(sequence, arrival)| Some((*sequence, (*arrival)?))).collect();
        assert_eq!(
            received,
            [(222, 250), (223, 750), (224, 1500), (225, 2500), (226, 3750), (230, 5250), (231, 7000), (232, 9000)],
            "five received, three lost, three received, two lost; deltas of 1 to 8 quarters of a millisecond"
        );
        for refused in [
            "8fcd0005 00000001 00000002 00000018 00000000 6018 0000",
            "8fcd0005 00000001 00000002 00000007 00000000 cd50 0000",
        ] {
            assert_eq!(read(&bytes(refused)).count(), 0, "the reserved symbol 11 of the draft's second examples");
        }
        let short = bytes("8fcd0004 00000001 00000002 00000003 00000000");
        assert_eq!(read(&short).count(), 0, "no chunk for three statuses");
        let no_delta = bytes("afcd0005 00000001 00000002 00000001 00000000 2001 0002");
        assert_eq!(read(&no_delta).count(), 0, "a received status without its delta, the padding apart");
        let negative = bytes("8fcd0005 00000001 00000002 00000001 ffffff00 4001 ff9c");
        assert_eq!(
            feedback(&negative).statuses().collect::<Vec<_>>(),
            [(0, Some(-64_000 - 100 * 250))],
            "a reference under nought, a large delta under nought"
        );
    }

    // The payloads of the calling server's transportcc.rs tests after the sender's SSRC: read as the server reads them, and written alike from the same arrivals.
    #[test]
    fn feedback_of_the_calling_server() {
        let cases = [
            (5, "01020304 0001 0003 000000 05 2003 005c04", vec![(1, 0), (2, 23_000), (3, 24_000)]),
            (6, "01020304 0002 0001 000000 06 2001 b0", vec![(2, 44_000)]),
            (5, "01020304 0001 0003 000000 05 d600 005c0368", vec![(1, 0), (2, 23_000), (3, 241_000)]),
            (6, "01020304 0004 0001 0007ff 06 2001 c4", vec![(4, 131_057_000)]),
        ];
        for (count, payload, arrivals) in cases {
            let mut message = bytes("8fcd0000 00000000");
            message.extend(bytes(payload));
            message.resize(message.len().next_multiple_of(4), 0);
            message[3] = (message.len() / 4 - 1) as u8;
            let read = feedback(&message);
            let statuses: Vec<_> = read.statuses().map(|(sequence, arrival)| (sequence, arrival.unwrap())).collect();
            assert_eq!(statuses, arrivals, "{payload}");
            assert_eq!(read.feedback_count, count);
            let mut recorder = Recorder { count, ..Recorder::default() };
            for (sequence, at) in &arrivals {
                recorder.note(*sequence, Duration::from_micros(*at as u64));
            }
            let mut out = Vec::new();
            assert!(recorder.write(&mut out, 0, 0x0102_0304));
            assert_eq!(out, message, "{payload}, as the server writes it");
        }
    }

    #[test]
    fn losses_reorder_and_wrap() {
        let mut recorder = Recorder::default();
        for (sequence, at) in [(65534, 1_000), (65535, 1_250), (1, 1_500), (0, 1_750), (3, 2_000), (1, 9_000)] {
            recorder.note(sequence, Duration::from_micros(at));
        }
        let mut out = Vec::new();
        assert!(recorder.write(&mut out, 1, 2));
        assert_eq!(
            feedback(&out).statuses().collect::<Vec<_>>(),
            [
                (65534, Some(1_000)),
                (65535, Some(1_250)),
                (0, Some(1_750)),
                (1, Some(1_500)),
                (2, None),
                (3, Some(2_000))
            ],
            "across the wrap, 0 after 1, 2 lost, the duplicate of 1 not kept"
        );
        recorder.note(4, Duration::from_micros(9_000_000));
        recorder.note(5, Duration::from_micros(9_000_250));
        recorder.note(6, Duration::from_micros(20_000_000));
        let mut out = Vec::new();
        assert!(recorder.write(&mut out, 1, 2) && recorder.write(&mut out, 1, 2) && !recorder.write(&mut out, 1, 2));
        let messages: Vec<_> = read(&out).collect();
        let [Packet::TransportFeedback(first), Packet::TransportFeedback(second)] = messages[..] else { panic!("two") };
        assert_eq!((first.base, first.count, first.feedback_count, first.reference), (4, 2, 1, 140), "9 s in 64 ms");
        assert_eq!(
            (second.base, second.count, second.feedback_count, second.reference),
            (6, 1, 2, 312),
            "11 s on is too far for a delta: a message of its own"
        );
    }

    // A sender numbers its packets from 1 as they go, and a feedback message names them back: those received with their arrival, those lost with none, those it no longer keeps passed over.
    #[test]
    fn departures_and_outcomes() {
        use crate::net::rtp::extension::Element;
        use crate::net::rtp::packet::{Header, Packet};
        let mut departures = Departures::default();
        let element = [Element::TransportSequence(0)];
        let header = Header {
            marker: false,
            payload_type: 108,
            sequence: 9,
            timestamp: 0,
            ssrc: 2,
            csrcs: &[],
            extensions: &element,
        };
        let mut sent = Vec::new();
        for at in 0..4u64 {
            let mut packet = Vec::new();
            assert!(header.write(&mut packet));
            assert!(departures.send(&mut packet, 100 + at as usize, Duration::from_millis(at * 10)));
            sent.push(Packet::read(&packet).unwrap().extensions.transport_sequence());
        }
        assert_eq!(sent, [Some(1), Some(2), Some(3), Some(4)], "numbered from the calling server's first, 1");
        assert!(!departures.send(&mut crate::net::rtp::tests::rtp(2, 1, &[]), 50, Duration::ZERO), "no element");
        let mut recorder = Recorder::default();
        recorder.note(2, Duration::from_micros(64_000 + 5_000));
        recorder.note(4, Duration::from_micros(64_000 + 7_000));
        let mut out = Vec::new();
        assert!(recorder.write(&mut out, 1, 2));
        let outcomes = departures.outcomes(&feedback(&out));
        assert_eq!(
            outcomes,
            [
                Outcome { sent: Duration::from_millis(10), bytes: 101, arrival: Some(69_000) },
                Outcome { sent: Duration::from_millis(20), bytes: 102, arrival: None },
                Outcome { sent: Duration::from_millis(30), bytes: 103, arrival: Some(71_000) },
            ],
            "2 and 4 received, 3 lost between them; 1, never told of, not among them"
        );
        assert!(departures.outcomes(&feedback(&out)).is_empty(), "what was told of is let go");
        let mut late = Departures::default();
        for at in 0..=MOST_DEPARTURES as u64 {
            let mut packet = Vec::new();
            assert!(header.write(&mut packet) && late.send(&mut packet, 1, Duration::from_micros(at)));
        }
        assert_eq!(late.sent.len(), MOST_DEPARTURES, "no more kept than half the space of the numbers");
        let mut packet = Vec::new();
        assert!(header.write(&mut packet) && late.send(&mut packet, 1, Duration::from_secs(11)));
        assert_eq!(late.sent.len(), 1, "nor any older than ten seconds");
    }

    #[test]
    fn bounded() {
        let mut recorder = Recorder::default();
        for sequence in 0..=MOST_ARRIVALS as u16 {
            recorder.note(sequence, Duration::from_millis(u64::from(sequence) * 100));
        }
        assert_eq!(recorder.arrivals.len(), MOST_ARRIVALS, "the oldest let go");
        let mut out = Vec::new();
        let mut taken = 0;
        while recorder.write(&mut out, 1, 2) {
            let read = feedback(&out);
            assert!(out.len() <= 4 + 8 + HEAD + MOST_BYTES + 3, "within the server's bound and the filling to a word");
            assert_eq!(read.base, taken as u16 + 1, "each message where the last ended");
            taken += read.statuses().count();
            out.clear();
        }
        assert_eq!(taken, MOST_ARRIVALS, "every arrival fed back once, 100 ms deltas of two bytes each");
        let mut sparse = Recorder::default();
        sparse.note(0, Duration::ZERO);
        sparse.note(40_000, Duration::ZERO);
        let mut out = Vec::new();
        assert!(sparse.write(&mut out, 1, 2));
        assert_eq!(feedback(&out).count, 1, "a gap past what a message's bound holds ends it");
    }
}
