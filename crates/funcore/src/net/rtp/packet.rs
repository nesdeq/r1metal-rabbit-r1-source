use super::extension::{self, Element, Extensions, TRANSPORT_SEQUENCE};
use super::{PADDING_BIT, SEQUENCE_BYTES, SSRC_BYTES, VERSION, VERSION_SHIFT, WORD, is_version};
use crate::base::bytes::{be16, be32};
use alloc::vec::Vec;

// RFC 3550, 5.1.
const HEAD: usize = 12;
const EXTENSION_BIT: u8 = 0x10;
const CSRC_COUNT: u8 = 0x0F;
const MARKER: u8 = 0x80;
pub(super) const PAYLOAD_TYPE: u8 = 0x7F;
pub(super) const TYPE_AT: usize = 1;
pub(super) const SEQUENCE_AT: usize = 2;
const TIMESTAMP_AT: usize = 4;
pub(super) const SSRC_AT: usize = 8;

// An RTP packet read in place; its payload without its padding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Packet<'a> {
    pub(super) marker: bool,
    pub(super) payload_type: u8,
    pub(super) sequence: u16,
    pub(super) timestamp: u32,
    pub(super) ssrc: u32,
    pub(super) extensions: Extensions<'a>,
    pub(super) payload: &'a [u8],
    head: &'a [u8],
}

// What an RTP header written tells.
pub(super) struct Header<'a> {
    pub(super) marker: bool,
    pub(super) payload_type: u8,
    pub(super) sequence: u16,
    pub(super) timestamp: u32,
    pub(super) ssrc: u32,
    pub(super) csrcs: &'a [u32],
    pub(super) extensions: &'a [Element<'a>],
}

// Where a packet's CSRCs end and its extension begins.
fn csrcs_end(first: u8) -> usize {
    HEAD + usize::from(first & CSRC_COUNT) * WORD
}

// The bytes of a packet's header with its CSRCs and its extension; none for what is no RTP packet of version 2 or is cut short.
pub(super) fn head_length(packet: &[u8]) -> Option<usize> {
    let first = *packet.first().filter(|first| is_version(**first))?;
    let start = csrcs_end(first);
    let end = if first & EXTENSION_BIT == 0 {
        start
    } else {
        start + extension::HEAD + usize::from(be16(packet, start + extension::LENGTH_AT)?) * WORD
    };
    (packet.len() >= end.max(HEAD)).then_some(end)
}

// A packet's extension; the default for a packet without one.
fn extensions_of(packet: &[u8], head: usize) -> Option<(usize, Extensions<'_>)> {
    let start = csrcs_end(*packet.first()?);
    if packet[0] & EXTENSION_BIT == 0 {
        return Some((start, Extensions::default()));
    }
    let bytes = packet.get(start + extension::HEAD..head)?;
    Some((start + extension::HEAD, Extensions::new(be16(packet, start)?, bytes)))
}

impl<'a> Packet<'a> {
    // None for what is no RTP packet, or whose padding's count, which counts itself, is not within its payload.
    pub(crate) fn read(packet: &'a [u8]) -> Option<Packet<'a>> {
        let head = head_length(packet)?;
        let first = packet[0];
        let (_, extensions) = extensions_of(packet, head)?;
        let padding =
            if first & PADDING_BIT == 0 { 0 } else { usize::from(*packet.last().filter(|count| **count > 0)?) };
        let end = packet.len().checked_sub(padding).filter(|end| *end >= head)?;
        Some(Packet {
            marker: packet[TYPE_AT] & MARKER != 0,
            payload_type: packet[TYPE_AT] & PAYLOAD_TYPE,
            sequence: be16(packet, SEQUENCE_AT)?,
            timestamp: be32(packet, TIMESTAMP_AT)?,
            ssrc: be32(packet, SSRC_AT)?,
            extensions,
            payload: &packet[head..end],
            head: &packet[..head],
        })
    }

    // The header's bytes with its CSRCs and its extension.
    pub(super) fn head(&self) -> &'a [u8] {
        self.head
    }
}

impl Header<'_> {
    // Appends the header; false, appending nothing, where a payload type, the CSRCs or an extension do not fit RTP.
    pub(crate) fn write(&self, out: &mut Vec<u8>) -> bool {
        if self.payload_type > PAYLOAD_TYPE || self.csrcs.len() > usize::from(CSRC_COUNT) {
            return false;
        }
        let start = out.len();
        let extension = if self.extensions.is_empty() { 0 } else { EXTENSION_BIT };
        out.push(VERSION << VERSION_SHIFT | extension | self.csrcs.len() as u8);
        out.push(if self.marker { MARKER } else { 0 } | self.payload_type);
        out.extend(self.sequence.to_be_bytes());
        out.extend(self.timestamp.to_be_bytes());
        out.extend(self.ssrc.to_be_bytes());
        out.extend(self.csrcs.iter().flat_map(|csrc| csrc.to_be_bytes()));
        if !self.extensions.is_empty() && !extension::write(self.extensions, out) {
            out.truncate(start);
            return false;
        }
        true
    }
}

// Writes the transport-wide sequence number into the element a packet carries for it, as it goes out; false where it carries none of two bytes.
pub(super) fn set_transport_sequence(packet: &mut [u8], sequence: u16) -> bool {
    let Some(head) = head_length(packet) else {
        return false;
    };
    let Some((start, place)) = extensions_of(packet, head).and_then(|(start, extensions)| {
        extensions.place(TRANSPORT_SEQUENCE).filter(|place| place.len() == SEQUENCE_BYTES).map(|place| (start, place))
    }) else {
        return false;
    };
    packet[start + place.start..start + place.end].copy_from_slice(&sequence.to_be_bytes());
    true
}

// Rewrites a header's payload type, keeping its marker, its sequence number and its SSRC.
pub(super) fn rewrite(head: &mut [u8], payload_type: u8, sequence: u16, ssrc: u32) {
    head[TYPE_AT] = head[TYPE_AT] & MARKER | payload_type & PAYLOAD_TYPE;
    head[SEQUENCE_AT..SEQUENCE_AT + SEQUENCE_BYTES].copy_from_slice(&sequence.to_be_bytes());
    head[SSRC_AT..SSRC_AT + SSRC_BYTES].copy_from_slice(&ssrc.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::rtp::extension::{AudioLevel, Form};
    use alloc::vec;

    // Appends `count` bytes of padding to a packet written, the last telling their count, and marks its header (RFC 3550, 5.1); false for none.
    fn pad(packet: &mut Vec<u8>, count: u8) -> bool {
        let Some(first) = packet.first_mut().filter(|_| count > 0) else {
            return false;
        };
        *first |= PADDING_BIT;
        packet.resize(packet.len() + usize::from(count) - 1, 0);
        packet.push(count);
        true
    }

    #[test]
    fn header_from_rfc_figure() {
        let packet = [0x80, 0x60, 0x12, 0x34, 0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, b'o', b'p', b'u', b's'];
        let read = Packet::read(&packet).unwrap();
        assert_eq!(
            (read.marker, read.payload_type, read.sequence, read.timestamp, read.ssrc, read.payload),
            (false, 0x60, 0x1234, 0xDEAD_BEEF, 0x0102_0304, &b"opus"[..]),
            "V=2, no padding, extension or CSRC, PT 96"
        );
        assert_eq!((read.extensions.form(), read.head().len()), (None, 12));
    }

    #[test]
    fn csrcs_extension_padding_and_marker() {
        let packet = [
            0xB2, 0xE6, 0, 7, 0, 0, 0, 9, 0, 0, 0,
            1, // V=2, P, X, CC=2, M, PT 102, sequence 7, timestamp 9, SSRC 1
            0, 0, 0, 2, 0, 0, 0, 3, // CSRCs 2 and 3
            0xBE, 0xDE, 0, 1, 0x50, 0x9E, 0, 0, // one word of extension: audio level 30 with a voice
            0xAA, 0xBB, // payload
            0, 0, 3, // three bytes of padding
        ];
        let read = Packet::read(&packet).unwrap();
        assert!(read.marker && read.payload_type == 102 && read.sequence == 7 && read.timestamp == 9);
        assert_eq!(read.extensions.form(), Some(Form::OneByte));
        assert_eq!(read.extensions.audio_level(), Some(AudioLevel { level: 30, is_voice: true }));
        assert_eq!((read.payload, read.head().len()), (&[0xAA, 0xBB][..], 28), "the padding taken off");
        let mut no_count = packet;
        no_count[32] = 0;
        assert!(Packet::read(&no_count).is_none(), "a count of none, which counts not even itself");
        let mut too_much = packet;
        too_much[32] = 6;
        assert!(Packet::read(&too_much).is_none(), "padding into the header");
        too_much[32] = 5;
        assert_eq!(Packet::read(&too_much).map(|read| read.payload.len()), Some(0), "padding of the whole payload");
        for length in 0..28 {
            assert!(Packet::read(&packet[..length]).is_none(), "{length} bytes");
        }
        let mut version = packet;
        version[0] = 0x72;
        assert!(Packet::read(&version).is_none(), "version 1");
    }

    #[test]
    fn written_and_read_back() {
        let extensions =
            [Element::TransportSequence(0x0B0C), Element::AudioLevel(AudioLevel { level: 11, is_voice: false })];
        let header = Header {
            marker: true,
            payload_type: 108,
            sequence: 0xFFFF,
            timestamp: 90_000,
            ssrc: 0xAABB_CC02,
            csrcs: &[5],
            extensions: &extensions,
        };
        let mut out = Vec::new();
        assert!(header.write(&mut out));
        out.extend(b"vp8");
        assert!(pad(&mut out, 4));
        assert_eq!(
            out,
            [
                0xB1, 0xEC, 0xFF, 0xFF, 0, 1, 0x5F, 0x90, 0xAA, 0xBB, 0xCC, 0x02, 0, 0, 0, 5, 0xBE, 0xDE, 0, 2, 0x11,
                0x0B, 0x0C, 0x50, 0x0B, 0, 0, 0, b'v', b'p', b'8', 0, 0, 0, 4
            ],
            "RFC 3550's header, CSRC, the extension of two words, payload and four bytes of padding"
        );
        let read = Packet::read(&out).unwrap();
        assert_eq!(
            (read.marker, read.payload_type, read.sequence, read.timestamp, read.ssrc, read.payload),
            (true, 108, 0xFFFF, 90_000, 0xAABB_CC02, &b"vp8"[..])
        );
        assert_eq!(read.extensions.transport_sequence(), Some(0x0B0C));
        assert!(set_transport_sequence(&mut out, 0x1234));
        assert_eq!(Packet::read(&out).unwrap().extensions.transport_sequence(), Some(0x1234), "rewritten in place");
        let plain = Header { csrcs: &[], extensions: &[], ..header };
        let mut out = vec![];
        assert!(plain.write(&mut out) && out.len() == 12 && out[0] == 0x80);
        assert!(!set_transport_sequence(&mut out, 1), "a packet without the element");
        assert!(!pad(&mut out, 0) && !pad(&mut Vec::new(), 1));
        for refused in [
            Header { payload_type: 128, ..plain },
            Header { csrcs: &[0; 16], ..plain },
            Header { extensions: &[Element::Other { id: 0, data: &[1] }], ..plain },
        ] {
            let mut out = vec![1];
            assert!(!refused.write(&mut out) && out == [1], "a header RTP does not take");
        }
    }

    // The calling server's fuzz seeds (backend/fuzz/seeds/rtp), read as its `Header::parse` reads their headers and extensions; the payload starts where its parse puts it, though it asks 16 bytes more of a packet for SRTP's tag.
    #[test]
    fn calling_server_seeds() {
        use crate::net::rtp::rtx;
        use crate::net::rtp::tests::bytes;
        let level = bytes("90000102 03040506 0708090a bede0001 500b0000");
        let read = Packet::read(&level).unwrap();
        assert_eq!(
            (read.marker, read.payload_type, read.sequence, read.timestamp, read.ssrc, read.payload.len()),
            (false, 0, 0x0102, 0x0304_0506, 0x0708_090a, 0),
            "ext-audio-level"
        );
        assert_eq!(
            read.extensions.audio_level(),
            Some(AudioLevel { level: 11, is_voice: false }),
            "11 dB under the overload, which the server keeps as its 120 less 11"
        );
        assert_eq!(head_length(&level), Some(20), "the server's start of the payload");
        let sequence = bytes("90000102 03040506 0708090a bede0001 110b0c00");
        assert_eq!(Packet::read(&sequence).unwrap().extensions.transport_sequence(), Some(0x0b0c), "ext-tcc-seqnum");
        for (seed, sequence) in [
            ("80760102 03040506 0708090a 0b0c", None),
            ("90760102 03040506 0708090a bede0001 110b0c00 0d0e", Some(0x0b0c)),
        ] {
            let mut rtx = bytes(seed);
            let read = Packet::read(&rtx).unwrap();
            assert_eq!((read.payload_type, read.ssrc), (118, 0x0708_090a), "{seed}: VP8's RTX");
            let original = rtx::unwrap(&mut rtx, 0x0708_090a - 1, 118 - 10).unwrap();
            let read = Packet::read(original).unwrap();
            assert_eq!(
                (read.payload_type, read.ssrc, read.timestamp, read.payload.len()),
                (108, 0x0708_0909, 0x0304_0506, 0),
                "{seed}: the server's original, its SSRC one below and its payload type ten"
            );
            assert_eq!(read.extensions.transport_sequence(), sequence, "{seed}: the extension kept");
            assert_eq!(read.sequence, if sequence.is_some() { 0x0d0e } else { 0x0b0c }, "{seed}: the OSN read");
        }
        let descriptor = bytes(
            "90010002 00000003 00000004 10000007 06178008 578002044eaaaf2860414d34538a0940409fc077c0 000000 00000000000000000000000000000000",
        );
        let read = Packet::read(&descriptor).unwrap();
        assert_eq!(read.extensions.form(), Some(Form::TwoByte), "ext-dependency-descriptor: the two-byte form");
        let data = read.extensions.get(6).unwrap();
        assert_eq!((data.len(), &data[..3]), (23, &[0x80, 0x08, 0x57][..]), "the descriptor's 23 bytes, kept whole");
        assert_eq!(head_length(&descriptor), Some(44), "the server's start of the payload, its last 16 bytes the tag");
    }

    #[test]
    fn rewritten_header() {
        let mut head = [0x80, 0xEC, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        rewrite(&mut head, 118, 0xABCD, 0x0102_0304);
        assert_eq!(head, [0x80, 0xF6, 0xAB, 0xCD, 0, 0, 0, 2, 1, 2, 3, 4], "the marker kept, the timestamp too");
    }
}
