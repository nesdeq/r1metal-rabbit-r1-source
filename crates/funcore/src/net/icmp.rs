use super::{Address, ICMP, SOURCE, ipv4_head, parse_ipv4_head, sum};
use crate::base::bytes::be16;
use alloc::vec::Vec;

pub(super) const REQUEST: u8 = 8;
pub(super) const REPLY: u8 = 0;
pub(super) const UNREACHABLE: u8 = 3;
pub(super) const TIME_EXCEEDED: u8 = 11;
pub(super) const PARAMETER_PROBLEM: u8 = 12;
// The code of a packet too large to pass whole that may not be cut (RFC 1191).
pub(super) const TOO_BIG: u8 = 4;
pub(crate) const HEADER: usize = 8;
const IDENTIFIER: core::ops::Range<usize> = 4..6;
const SEQUENCE: core::ops::Range<usize> = 6..8;
const CHECKSUM: core::ops::Range<usize> = 2..4;
const NEXT_MTU: core::ops::Range<usize> = 6..8;

pub(crate) fn checksum(bytes: &[u8]) -> u16 {
    !sum(&[bytes])
}

// True for the kinds of message that tell of a packet that did not arrive, and quote it.
pub(super) fn is_error(kind: u8) -> bool {
    matches!(kind, UNREACHABLE | TIME_EXCEEDED | PARAMETER_PROBLEM)
}

// Writes a message's checksum into it.
pub(super) fn set_checksum(message: &mut [u8]) {
    message[CHECKSUM].fill(0);
    let sum = checksum(message);
    message[CHECKSUM].copy_from_slice(&sum.to_be_bytes());
}

// The packet that tells the sender of `packet`, whose head is `head` bytes, that it is too large to pass whole where packets of `mtu` bytes pass, from `source` (RFC 1191).
pub(super) fn too_big(packet: &[u8], head: usize, source: Address, mtu: u16) -> Vec<u8> {
    let quoted = &packet[..packet.len().min(head + HEADER)];
    let mut message = alloc::vec![UNREACHABLE, TOO_BIG, 0, 0, 0, 0, 0, 0];
    message[NEXT_MTU].copy_from_slice(&mtu.to_be_bytes());
    message.extend_from_slice(quoted);
    set_checksum(&mut message);
    let target: Address = packet[SOURCE].try_into().expect("four bytes of a head");
    [&ipv4_head(ICMP, source, target, message.len())[..], &message].concat()
}

pub(crate) fn request(identifier: u16, sequence: u16) -> [u8; HEADER] {
    let mut packet = [0u8; HEADER];
    packet[0] = REQUEST;
    packet[IDENTIFIER].copy_from_slice(&identifier.to_be_bytes());
    packet[SEQUENCE].copy_from_slice(&sequence.to_be_bytes());
    set_checksum(&mut packet);
    packet
}

pub(crate) fn parse_reply(packet: &[u8], identifier: u16, source: [u8; 4]) -> Option<u16> {
    let (header, protocol) = parse_ipv4_head(packet)?;
    if packet.len() < header + HEADER || protocol != ICMP || packet[SOURCE] != source {
        return None;
    }
    let echo = &packet[header..];
    let is_match = echo[0] == REPLY && echo[1] == 0 && echo[IDENTIFIER] == identifier.to_be_bytes();
    be16(echo, SEQUENCE.start).filter(|_| is_match && checksum(echo) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::IPV4;
    use crate::net::PROTOCOL_AT;
    use crate::net::VERSION_SHIFT;
    use crate::net::WORD;
    use alloc::vec::Vec;

    const FROM: [u8; 4] = [1, 1, 1, 1];

    fn packet(words: u8, from: [u8; 4], kind: u8, identifier: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
        let mut header = alloc::vec![0u8; usize::from(words) * WORD];
        (header[0], header[PROTOCOL_AT]) = (IPV4 << VERSION_SHIFT | words, ICMP);
        header[SOURCE].copy_from_slice(&from);
        let mut echo = request(identifier, sequence).to_vec();
        (echo[0], echo[2], echo[3]) = (kind, 0, 0);
        echo.extend_from_slice(data);
        let sum = checksum(&echo);
        echo[CHECKSUM].copy_from_slice(&sum.to_be_bytes());
        header.extend(echo);
        header
    }

    #[test]
    fn rfc_1071_checksum() {
        assert_eq!(checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]), !0xddf2, "the RFC's own example");
        assert_eq!(checksum(&[]), u16::MAX);
        let sent = request(0x0001, 0x0007);
        assert_eq!(sent, [8, 0, 0xf7, 0xf7, 0, 1, 0, 7]);
        assert_eq!(checksum(&sent), 0, "a request carries its checksum");
    }

    #[test]
    fn echo_reply_sequence() {
        assert_eq!(parse_reply(&packet(5, FROM, REPLY, 1, 7, &[]), 1, FROM), Some(7));
        assert_eq!(parse_reply(&packet(5, FROM, REPLY, 1, 7, b"odd"), 1, FROM), Some(7), "with data, of any length");
        assert_eq!(
            parse_reply(&packet(6, FROM, REPLY, 1, 65535, &[]), 1, FROM),
            Some(65535),
            "behind a header with options"
        );
        assert_eq!(parse_reply(&packet(5, FROM, REQUEST, 1, 7, &[]), 1, FROM), None, "a request is no reply");
        assert_eq!(parse_reply(&packet(5, FROM, REPLY, 2, 7, &[]), 1, FROM), None, "a reply to another asker");
        assert_eq!(
            parse_reply(&packet(5, [9, 9, 9, 9], REPLY, 1, 7, &[]), 1, FROM),
            None,
            "a reply from another address"
        );
        let mut corrupt = packet(5, FROM, REPLY, 1, 7, &[]);
        corrupt[27] ^= 1;
        assert_eq!(parse_reply(&corrupt, 1, FROM), None, "a reply whose checksum is wrong");
        let mut other = packet(5, FROM, REPLY, 1, 7, &[]);
        other[PROTOCOL_AT] = 6;
        assert_eq!(parse_reply(&other, 1, FROM), None, "a packet of another protocol");
        let mut sixth = packet(5, FROM, REPLY, 1, 7, &[]);
        sixth[0] = 6 << VERSION_SHIFT | 5;
        assert_eq!(parse_reply(&sixth, 1, FROM), None, "a packet of another version");
        let whole = packet(5, FROM, REPLY, 1, 7, &[]);
        assert!((0..whole.len()).all(|cut| parse_reply(&whole[..cut], 1, FROM).is_none()), "a packet cut short");
        assert_eq!(
            parse_reply(&packet(4, FROM, REPLY, 1, 7, &[]), 1, FROM),
            None,
            "a header shorter than any IPv4 has"
        );
    }
}
