use super::packet::{PAYLOAD_TYPE, TYPE_AT};
use super::{is_version, packet, rtcp};
use crate::base::bytes::be32;
use core::ops::RangeInclusive;

// RFC 5761, 4: the RTCP packet types that share the byte of RTP's marker and payload type, and the payload types RTP leaves them.
const RTCP_TYPES: RangeInclusive<u8> = 192..=223;
const RTCP_PAYLOAD_TYPES: RangeInclusive<u8> = 64..=95;

// What a datagram on a call's one port is under rtcp-mux, and what it is routed by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Rtp { ssrc: u32, payload_type: u8 },
    Rtcp,
}

// The kind of a datagram by its first bytes; none for what is neither: STUN, another version, a payload type RTCP keeps, too few bytes.
pub(super) fn kind_of(datagram: &[u8]) -> Option<Kind> {
    let (first, kind) = (*datagram.first()?, *datagram.get(TYPE_AT)?);
    if !is_version(first) {
        return None;
    }
    if RTCP_TYPES.contains(&kind) {
        return be32(datagram, rtcp::SSRC_AT).map(|_| Kind::Rtcp);
    }
    let payload_type = kind & PAYLOAD_TYPE;
    let ssrc = be32(datagram, packet::SSRC_AT)?;
    (!RTCP_PAYLOAD_TYPES.contains(&payload_type)).then_some(Kind::Rtp { ssrc, payload_type })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_and_rtcp_apart() {
        let rtp = [0x80, 0xE6, 0, 1, 0, 0, 0, 2, 0x00, 0x00, 0x07, 0xEA];
        assert_eq!(kind_of(&rtp), Some(Kind::Rtp { ssrc: 2026, payload_type: 102 }), "Opus with its marker");
        let rtcp = [0x81, 0xC9, 0, 7, 0x00, 0x00, 0x07, 0xD1];
        assert_eq!(kind_of(&rtcp), Some(Kind::Rtcp), "a receiver report");
        for kind in [192, 200, 205, 206, 223] {
            assert_eq!(kind_of(&[0x80, kind, 0, 0, 0, 0, 0, 1]), Some(Kind::Rtcp), "type {kind}");
        }
        for (byte, is_rtcp) in [(64, false), (95, false), (64 + 128, true), (95 + 128, true)] {
            let mut packet = rtp;
            packet[1] = byte;
            assert_eq!(kind_of(&packet).is_some(), is_rtcp, "byte {byte}: payload types 64 to 95 are RTCP's");
        }
        assert_eq!(kind_of(&[0x80, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None, "an RTP header cut short");
        assert_eq!(kind_of(&[0x81, 0xC8, 0, 6, 0, 0, 0]), None, "an RTCP header cut short");
        assert_eq!(kind_of(&[0x00, 0x01, 0x00, 0x08, 0x21, 0x12, 0xA4, 0x42, 0, 0, 0, 0]), None, "STUN");
        assert_eq!(kind_of(&[0x40, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]), None, "version 1");
        assert_eq!(kind_of(&[]), None);
    }
}
