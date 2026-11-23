use super::packet::{self, Packet};
use super::{PADDING_BIT, SEQUENCE_BYTES};
use crate::base::bytes::be16;
use alloc::vec::Vec;

// Appends the RTX packet (RFC 4588, 4) of an original packet: its header under the retransmission stream's payload type, sequence number and SSRC, its timestamp, marker, CSRCs and extension kept, then the original sequence number and payload, the padding left out; false where the original is no packet.
pub(super) fn wrap(original: &[u8], ssrc: u32, payload_type: u8, sequence: u16, out: &mut Vec<u8>) -> bool {
    let Some(read) = Packet::read(original) else {
        return false;
    };
    let start = out.len();
    out.extend_from_slice(read.head());
    out[start] &= !PADDING_BIT;
    packet::rewrite(&mut out[start..], payload_type, sequence, ssrc);
    out.extend(read.sequence.to_be_bytes());
    out.extend_from_slice(read.payload);
    true
}

// Turns a received RTX packet in place into its original, of the stream's SSRC and payload type: the packet less its first two bytes; none where it carries no original sequence number, as padding alone does.
pub(super) fn unwrap(rtx: &mut [u8], ssrc: u32, payload_type: u8) -> Option<&mut [u8]> {
    let (head, payload) = Packet::read(rtx).map(|read| (read.head().len(), read.payload.len()))?;
    let original = be16(rtx, head).filter(|_| payload >= SEQUENCE_BYTES)?;
    packet::rewrite(&mut rtx[..head], payload_type, original, ssrc);
    rtx.copy_within(..head, SEQUENCE_BYTES);
    Some(&mut rtx[SEQUENCE_BYTES..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // A VP8 packet of the group call's video layer: CSRC 7, the transport-wide sequence number, a payload and two bytes of padding.
    const ORIGINAL: [u8; 31] = [
        0xB1, 0xEC, 0x12, 0x34, 0, 0, 0x03, 0xE8, 0x00, 0x00, 0x01, 0x02, // marker, PT 108, sequence 0x1234
        0, 0, 0, 7, // CSRC
        0xBE, 0xDE, 0, 1, 0x11, 0x00, 0x05, 0, // transport-wide sequence number 5
        b'f', b'r', b'a', b'm', b'e', 0, 2, // payload and padding
    ];

    #[test]
    fn wrapped_by_rfc_4588() {
        let mut out = vec![0xEE];
        assert!(wrap(&ORIGINAL, 0x0103, 118, 1, &mut out));
        assert_eq!(
            out,
            [
                0xEE, 0x91, 0xF6, 0, 1, 0, 0, 0x03, 0xE8, 0x00, 0x00, 0x01, 0x03, 0, 0, 0, 7, 0xBE, 0xDE, 0, 1, 0x11,
                0x00, 0x05, 0, 0x12, 0x34, b'f', b'r', b'a', b'm', b'e'
            ],
            "the padding bit and the padding gone, PT 118, the RTX stream's sequence number and SSRC, the OSN ahead of the payload"
        );
        assert!(!wrap(&ORIGINAL[..10], 1, 118, 1, &mut out) && out.len() == 32, "no packet");
    }

    #[test]
    fn unwrapped_in_place() {
        let mut out = Vec::new();
        assert!(wrap(&ORIGINAL, 0x0103, 118, 1, &mut out));
        let original = unwrap(&mut out, 0x0102, 108).unwrap();
        assert_eq!(
            original,
            [
                0x91, 0xEC, 0x12, 0x34, 0, 0, 0x03, 0xE8, 0x00, 0x00, 0x01, 0x02, 0, 0, 0, 7, 0xBE, 0xDE, 0, 1, 0x11,
                0x00, 0x05, 0, b'f', b'r', b'a', b'm', b'e'
            ],
            "the original again, without its padding"
        );
        let mut padded = vec![0xA0, 0xF6, 0, 9, 0, 0, 0, 0, 0, 0, 1, 3, 0, 0, 3];
        assert_eq!(unwrap(&mut padded, 0x0102, 108), None, "padding alone carries no original");
        let mut short = vec![0x80, 0xF6, 0, 9, 0, 0, 0, 0, 0, 0, 1, 3, 7];
        assert_eq!(unwrap(&mut short, 0x0102, 108), None, "one byte of payload");
        let mut with_padding = vec![0xA0, 0xF6, 0, 9, 0, 0, 0, 5, 0, 0, 1, 3, 0x12, 0x34, b'x', 0, 2];
        let original = unwrap(&mut with_padding, 0x0102, 108).unwrap();
        assert_eq!(
            original,
            [0xA0, 0xEC, 0x12, 0x34, 0, 0, 0, 5, 0, 0, 1, 2, b'x', 0, 2],
            "the marker kept, and the RTX packet's padding stays the original's"
        );
        assert_eq!(Packet::read(original).map(|read| read.payload), Some(&b"x"[..]));
    }
}
