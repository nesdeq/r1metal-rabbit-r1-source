use super::feedback::{self, Fir, Nack, Pli, Remb};
use super::transport_cc::{self, Feedback};
use super::{PADDING_BIT, SSRC_BYTES, VERSION, VERSION_SHIFT, WORD, is_version, ntp, ntp_middle};
use crate::base::bytes::{be16, be32};
use crate::base::time::NANOS_PER_SECOND;
use alloc::vec::Vec;
use core::time::Duration;

// RFC 3550, 12.1, and RFC 4585, 6.1: the packet types.
const SENDER_REPORT: u8 = 200;
const RECEIVER_REPORT: u8 = 201;
pub(super) const RTPFB: u8 = 205;
pub(super) const PSFB: u8 = 206;
// RFC 3550, 6.4.1: five bits of count or format, the length in words less one, then the sender's SSRC.
const COUNT: u8 = 0x1F;
const HEAD: usize = 4;
const LENGTH_AT: usize = 2;
pub(super) const SSRC_AT: usize = HEAD;
const SENDER_INFO: usize = 20;
const BLOCK: usize = 24;
const MOST_COUNT: usize = COUNT as usize;
// RFC 3550, 6.4.1: the cumulative loss in 24 bits, signed.
const LOSS_BITS: u32 = 24;
pub(super) const MOST_LOSS: i32 = (1 << (LOSS_BITS - 1)) - 1;
const LEAST_LOSS: i32 = -(1 << (LOSS_BITS - 1));
// RFC 3550, 6.4.1: the delay since the last report counts in units of 1/65536 second.
const DLSR_PER_SECOND: u64 = 1 << 16;

// A packet of an RTCP datagram, read in place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Packet<'a> {
    SenderReport(SenderReport<'a>),
    ReceiverReport(ReceiverReport<'a>),
    Nack(Nack<'a>),
    TransportFeedback(Feedback<'a>),
    Pli(Pli),
    Fir(Fir<'a>),
    Remb(Remb),
}

// RFC 3550, 6.4.1: the sender information of a report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct SenderInfo {
    pub(super) ntp: u64,
    pub(super) timestamp: u32,
    pub(super) packets: u32,
    pub(super) octets: u32,
}

// RFC 3550, 6.4.1: a reception report block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ReportBlock {
    pub(super) ssrc: u32,
    pub(super) fraction_lost: u8,
    pub(super) cumulative_lost: i32,
    pub(super) highest_sequence: u32,
    pub(super) jitter: u32,
    pub(super) lsr: u32,
    pub(super) dlsr: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct SenderReport<'a> {
    pub(super) ssrc: u32,
    pub(super) info: SenderInfo,
    blocks: &'a [u8],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ReceiverReport<'a> {
    blocks: &'a [u8],
}

// The packets of an RTCP compound or reduced-size datagram (RFC 5506), those of an unknown type or format and those that do not read passed over; they end where a packet is of another version or its length does not fit.
pub(super) struct Packets<'a> {
    rest: &'a [u8],
}

pub(super) fn read(datagram: &[u8]) -> Packets<'_> {
    Packets { rest: datagram }
}

impl<'a> Iterator for Packets<'a> {
    type Item = Packet<'a>;

    fn next(&mut self) -> Option<Packet<'a>> {
        loop {
            let (Some(&first), Some(words)) = (self.rest.first(), be16(self.rest, LENGTH_AT)) else {
                self.rest = &[];
                return None;
            };
            let length = (usize::from(words) + 1) * WORD;
            if !is_version(first) || length > self.rest.len() {
                self.rest = &[];
                return None;
            }
            let (packet, rest) = self.rest.split_at(length);
            self.rest = rest;
            let padding = if first & PADDING_BIT == 0 { 0 } else { usize::from(packet[length - 1]) };
            let body =
                length.checked_sub(padding).filter(|end| *end >= HEAD && (padding > 0 || first & PADDING_BIT == 0));
            if let Some(read) = body.and_then(|end| Packet::parse(first & COUNT, packet[1], &packet[HEAD..end])) {
                return Some(read);
            }
        }
    }
}

impl<'a> Packet<'a> {
    // A packet of a known type and format by its body, after its first word and before its padding.
    fn parse(count: u8, kind: u8, body: &'a [u8]) -> Option<Packet<'a>> {
        match kind {
            SENDER_REPORT => {
                let blocks =
                    body.get(SSRC_BYTES + SENDER_INFO..SSRC_BYTES + SENDER_INFO + usize::from(count) * BLOCK)?;
                let info = SenderInfo {
                    ntp: u64::from(be32(body, 4)?) << u32::BITS | u64::from(be32(body, 8)?),
                    timestamp: be32(body, 12)?,
                    packets: be32(body, 16)?,
                    octets: be32(body, 20)?,
                };
                Some(Packet::SenderReport(SenderReport { ssrc: be32(body, 0)?, info, blocks }))
            }
            RECEIVER_REPORT => {
                let blocks = body.get(SSRC_BYTES..SSRC_BYTES + usize::from(count) * BLOCK)?;
                Some(Packet::ReceiverReport(ReceiverReport { blocks }))
            }
            RTPFB => {
                let (sender, media, fci) = feedback::parts(body)?;
                match count {
                    feedback::NACK => Nack::read(sender, media, fci).map(Packet::Nack),
                    transport_cc::FORMAT => Feedback::read(sender, media, fci).map(Packet::TransportFeedback),
                    _ => None,
                }
            }
            PSFB => {
                let (sender, media, fci) = feedback::parts(body)?;
                match count {
                    feedback::PLI => Some(Packet::Pli(Pli { sender, media })),
                    feedback::FIR => Fir::read(sender, fci).map(Packet::Fir),
                    feedback::AFB => Remb::read(sender, fci).map(Packet::Remb),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

fn blocks(bytes: &[u8]) -> impl Iterator<Item = ReportBlock> + '_ {
    bytes.as_chunks::<BLOCK>().0.iter().filter_map(ReportBlock::read)
}

impl<'a> SenderReport<'a> {
    pub(crate) fn blocks(&self) -> impl Iterator<Item = ReportBlock> + 'a {
        blocks(self.blocks)
    }
}

impl<'a> ReceiverReport<'a> {
    pub(crate) fn blocks(&self) -> impl Iterator<Item = ReportBlock> + 'a {
        blocks(self.blocks)
    }
}

impl ReportBlock {
    fn read(bytes: &[u8; BLOCK]) -> Option<ReportBlock> {
        let loss = be32(bytes, 4)?;
        Some(ReportBlock {
            ssrc: be32(bytes, 0)?,
            fraction_lost: (loss >> LOSS_BITS) as u8,
            cumulative_lost: (loss << (u32::BITS - LOSS_BITS)) as i32 >> (u32::BITS - LOSS_BITS),
            highest_sequence: be32(bytes, 8)?,
            jitter: be32(bytes, 12)?,
            lsr: be32(bytes, 16)?,
            dlsr: be32(bytes, 20)?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        let lost = self.cumulative_lost.clamp(LEAST_LOSS, MOST_LOSS) as u32 & ((1 << LOSS_BITS) - 1);
        out.extend(self.ssrc.to_be_bytes());
        out.extend((u32::from(self.fraction_lost) << LOSS_BITS | lost).to_be_bytes());
        for word in [self.highest_sequence, self.jitter, self.lsr, self.dlsr] {
            out.extend(word.to_be_bytes());
        }
    }

    // The round trip to the receiver that sent the block (RFC 3550, 6.4.1): now, of the clock this side's reports are stamped by, less the LSR and the DLSR; none before it heard a report, or for a span under nought.
    pub(super) fn round_trip(&self, now: Duration) -> Option<Duration> {
        let span = ntp_middle(ntp(now)).wrapping_sub(self.lsr).wrapping_sub(self.dlsr);
        (self.lsr != 0 && span <= i32::MAX as u32)
            .then(|| Duration::from_nanos(u64::from(span) * NANOS_PER_SECOND / DLSR_PER_SECOND))
    }
}

// The DLSR of a span (RFC 3550, 6.4.1): 1/65536 second each, the largest it tells for one longer.
pub(super) fn dlsr_of(span: Duration) -> u32 {
    u32::try_from(span.as_nanos() * u128::from(DLSR_PER_SECOND) / u128::from(NANOS_PER_SECOND)).unwrap_or(u32::MAX)
}

// Begins a packet of a count or format and a type; `finish` writes its length.
pub(super) fn begin(out: &mut Vec<u8>, count: u8, kind: u8) -> usize {
    let start = out.len();
    out.extend([VERSION << VERSION_SHIFT | count, kind, 0, 0]);
    start
}

// Fills a packet begun to a whole word with noughts and writes its length in words less one; false, the packet taken back, where it is too long for its length field.
pub(super) fn finish(out: &mut Vec<u8>, start: usize) -> bool {
    let length = (out.len() - start).next_multiple_of(WORD);
    out.resize(start + length, 0);
    let Ok(words) = u16::try_from(length / WORD - 1) else {
        out.truncate(start);
        return false;
    };
    out[start + LENGTH_AT..start + HEAD].copy_from_slice(&words.to_be_bytes());
    true
}

// Appends a sender report with its blocks; false, appending nothing, for more blocks than one report carries.
pub(super) fn write_sender_report(out: &mut Vec<u8>, ssrc: u32, info: &SenderInfo, blocks: &[ReportBlock]) -> bool {
    if blocks.len() > MOST_COUNT {
        return false;
    }
    let start = begin(out, blocks.len() as u8, SENDER_REPORT);
    out.extend(ssrc.to_be_bytes());
    out.extend(info.ntp.to_be_bytes());
    for word in [info.timestamp, info.packets, info.octets] {
        out.extend(word.to_be_bytes());
    }
    blocks.iter().for_each(|block| block.write(out));
    finish(out, start)
}

// Appends a receiver report with its blocks; false, appending nothing, for more blocks than one report carries.
pub(super) fn write_receiver_report(out: &mut Vec<u8>, ssrc: u32, blocks: &[ReportBlock]) -> bool {
    if blocks.len() > MOST_COUNT {
        return false;
    }
    let start = begin(out, blocks.len() as u8, RECEIVER_REPORT);
    out.extend(ssrc.to_be_bytes());
    blocks.iter().for_each(|block| block.write(out));
    finish(out, start)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::net::rtp::feedback::write_nack;
    use crate::net::rtp::tests::bytes;
    use alloc::vec;

    // RFC 3550, 6.5 and 6.6: a source description of one chunk, RingRTC's CNAME of 16 bytes, and a BYE of two SSRCs with its reason, neither of which a call's session reads.
    pub(in crate::net::rtp) const SDES: &str = "81ca0006 000003ea 0110 434e414d45434e414d45434e414d4521 0000";
    pub(in crate::net::rtp) const BYE: &str = "82cb0004 00000001 00000002 04646f6e 65000000";

    fn all(datagram: &[u8]) -> Vec<Packet<'_>> {
        read(datagram).collect()
    }

    // The packet of a datagram that holds one alone.
    pub(in crate::net::rtp) fn one(datagram: &[u8]) -> Packet<'_> {
        let [packet] = all(datagram)[..] else { panic!("one packet alone") };
        packet
    }

    const BLOCK_BYTES: &str = "000007d2 1c000001 00010002 00000018 b7052000 00054000";

    fn report_block() -> ReportBlock {
        ReportBlock {
            ssrc: 2002,
            fraction_lost: 28,
            cumulative_lost: 1,
            highest_sequence: 0x1_0002,
            jitter: 24,
            lsr: 0xb705_2000,
            dlsr: 0x0005_4000,
        }
    }

    #[test]
    fn sender_and_receiver_reports() {
        let info = SenderInfo { ntp: 0xb44d_b705_2000_0000, timestamp: 8000, packets: 100, octets: 16000 };
        let mut out = Vec::new();
        assert!(write_sender_report(&mut out, 1001, &info, &[report_block()]));
        let expected = ["81c8000c 000003e9 b44db705 20000000 00001f40 00000064 00003e80 ", BLOCK_BYTES].concat();
        assert_eq!(out, bytes(&expected), "RFC 3550, 6.4.1: 13 words, the length 12");
        let [Packet::SenderReport(report)] = all(&out)[..] else { panic!("one sender report") };
        assert_eq!((report.ssrc, report.info, report.blocks().collect::<Vec<_>>()), (1001, info, vec![report_block()]));
        let mut out = Vec::new();
        assert!(write_receiver_report(&mut out, 1001, &[report_block()]));
        assert_eq!(out, bytes(&["81c90007 000003e9 ", BLOCK_BYTES].concat()), "6.4.2: the RR, 8 words");
        let [Packet::ReceiverReport(report)] = all(&out)[..] else { panic!("one receiver report") };
        assert_eq!(report.blocks().collect::<Vec<_>>(), vec![report_block()]);
        let mut out = Vec::new();
        assert!(write_receiver_report(&mut out, 7, &[]) && out == bytes("80c90001 00000007"), "an empty RR");
        let negative = ReportBlock { cumulative_lost: -1, ..report_block() };
        let mut out = Vec::new();
        assert!(write_receiver_report(&mut out, 7, &[negative]));
        assert_eq!(out[13..16], [0xFF, 0xFF, 0xFF], "a loss under nought in 24 bits");
        let [Packet::ReceiverReport(report)] = all(&out)[..] else { panic!("one receiver report") };
        assert_eq!(report.blocks().next().map(|block| block.cumulative_lost), Some(-1), "read back signed");
        let huge = ReportBlock { cumulative_lost: i32::MAX, ..report_block() };
        let mut out = Vec::new();
        assert!(write_receiver_report(&mut out, 7, &[huge]) && out[13..16] == [0x7F, 0xFF, 0xFF], "clamped to 24 bits");
        assert!(!write_receiver_report(&mut out, 7, &[report_block(); 32]) && out.len() == 32, "32 blocks refused");
    }

    #[test]
    fn round_trip_of_rfc_figure() {
        let block = ReportBlock { lsr: 0xb705_2000, dlsr: 0x0005_4000, ..report_block() };
        let arrival = Duration::from_millis(46_864_500);
        assert_eq!(block.round_trip(arrival), Some(Duration::from_millis(6125)), "RFC 3550's figure 2: 6.125 s");
        assert_eq!(ReportBlock { lsr: 0, ..block }.round_trip(arrival), None, "no report heard yet");
        assert_eq!(block.round_trip(Duration::from_millis(46_858_000)), None, "a span under nought");
        assert_eq!(dlsr_of(Duration::from_millis(5250)), 0x0005_4000, "5.25 s in 65536ths");
        assert_eq!(dlsr_of(Duration::from_secs(1 << 20)), u32::MAX, "the longest it tells");
    }

    #[test]
    fn compound_with_what_is_unknown() {
        let mut compound = Vec::new();
        assert!(write_receiver_report(&mut compound, 1, &[]));
        compound.extend(bytes("80cc0002 00000001 6e616d65"));
        compound.extend(bytes("80cf0001 00000001"));
        compound.extend(bytes("82ce0002 00000001 00000002"));
        compound.extend(bytes(SDES));
        compound.extend(bytes("82c80006 00000001 00000000 00000000 00000000 00000000 00000000"));
        compound.extend(bytes("a0c90002 00000001 00000004"));
        compound.extend(bytes(BYE));
        assert!(write_nack(&mut compound, 1, 2, &[5]));
        let read = all(&compound);
        let kinds: Vec<&str> = read
            .iter()
            .map(|packet| match packet {
                Packet::ReceiverReport(_) => "RR",
                Packet::Nack(_) => "NACK",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["RR", "RR", "NACK"],
            "APP, XR, SLI, a source description, a sender report short of its two blocks and a BYE passed over; a receiver report with its padding read"
        );
        let mut cut = compound.clone();
        cut.truncate(cut.len() - 1);
        assert_eq!(all(&cut).len(), 2, "the last packet cut short ends the reading");
        let mut other_version = compound.clone();
        other_version[8] = 0x40;
        assert_eq!(all(&other_version).len(), 1, "a packet of another version ends it");
        assert!(
            all(&bytes("a0c90000")).is_empty() && all(&bytes("80c90000")).is_empty() && all(&[0x80, 0xc9]).is_empty()
        );
        let reduced = bytes("81ce0002 00000001 00000002");
        assert!(matches!(all(&reduced)[..], [Packet::Pli(Pli { sender: 1, media: 2 })]), "RFC 5506: a PLI alone");
    }

    // The calling server's fuzz seeds (backend/fuzz/seeds/rtcp): each claims 512 words in 28 bytes, so the server's parse, which takes the last 20 for the tag and the index, meets a packet that does not fit, as this reading does.
    #[test]
    fn calling_server_seeds() {
        let zeros = "00".repeat(20);
        let seeds = [
            ("bye", "81cb0200 01020304"),
            ("generic-feedback-nack", "81cd0200 01020304"),
            ("generic-feedback-transportcc", "8fcd0200 01020304"),
            ("receiver-report", "81c90200 01020304"),
            ("sdes", "81ca0200 01020304"),
            ("sender-report", "81c80200 01020304"),
            ("specific-feedback-format-loss", "8fce0200 01020304"),
            ("specific-feedback-pli", "81ce0200 01020304"),
        ];
        for (name, head) in seeds {
            let seed = bytes(&[head, zeros.as_str()].concat());
            assert_eq!(seed.len(), 28, "{name}: as the seed's file holds it");
            assert!(all(&seed).is_empty() && all(&seed[..8]).is_empty(), "{name}: its length does not fit");
            let mut fitted = seed[..8].to_vec();
            fitted[2..4].copy_from_slice(&[0, 1]);
            assert!(
                all(&fitted).is_empty(),
                "{name}: too short for its type, as the server's parse finds it, or a BYE, which is passed over"
            );
        }
    }
}
