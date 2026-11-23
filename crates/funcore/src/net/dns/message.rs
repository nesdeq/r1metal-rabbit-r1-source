// DNS messages: questions for a name's addresses, padded (RFC 7830, 8467), their frames on a stream (RFC 1035, 4.2.2), and what an answer says.

use crate::base::bytes::be16;
use crate::base::bytes::be32;
use alloc::vec::Vec;
use core::net::IpAddr;

const LABEL_BYTES: usize = 63;
const NAME_BYTES: usize = 253;
pub(super) const HEAD: usize = 12;
const RECURSION_DESIRED: u16 = 0x0100;
pub(super) const QR: u16 = 0x8000;
const RCODE: u16 = 0x000F;
const NXDOMAIN: u16 = 3;
pub(super) const TYPE_A: u16 = 1;
pub(super) const TYPE_AAAA: u16 = 28;
pub(super) const TYPE_OPT: u16 = 41;
const CLASS_IN: u16 = 1;
const ANSWER_BYTES: u16 = 1232;
pub(super) const OPTION_PADDING: u16 = 12;
pub(super) const OPTION_HEAD: usize = 4;
// The root's name, the record's kind, the longest answer taken, no flags, and the length of what follows.
const OPTIONS_HEAD: usize = 11;
const BLOCK: usize = 128;
const POINTER: u8 = 0xC0;
const TTL_TOP_BIT: u32 = 1 << 31;

// Adds the option that fills a message up to whole blocks (RFC 7830, 8467) behind the options it ends with.
pub(super) fn add_padding(message: &mut Vec<u8>) {
    let unpadded = message.len() + OPTION_HEAD;
    let padding = unpadded.next_multiple_of(BLOCK) - unpadded;
    message.extend(OPTION_PADDING.to_be_bytes());
    message.extend((padding as u16).to_be_bytes());
    message.resize(message.len() + padding, 0);
}

// Adds the options record to a message that has none, with the padding in it; the count of the message's records is its caller's.
pub(super) fn add_options(message: &mut Vec<u8>) {
    let start = message.len();
    message.push(0);
    message.extend(TYPE_OPT.to_be_bytes());
    message.extend(ANSWER_BYTES.to_be_bytes());
    message.extend([0; 6]);
    add_padding(message);
    let length = (message.len() - start - OPTIONS_HEAD) as u16;
    message[start + OPTIONS_HEAD - 2..start + OPTIONS_HEAD].copy_from_slice(&length.to_be_bytes());
}

pub(super) fn question(id: u16, name: &str, kind: u16) -> Option<Vec<u8>> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > NAME_BYTES {
        return None;
    }
    let mut message = Vec::with_capacity(BLOCK);
    message.extend(id.to_be_bytes());
    message.extend(RECURSION_DESIRED.to_be_bytes());
    // One question, no answers, no authorities, and one record more: the options.
    message.extend([0, 1, 0, 0, 0, 0, 0, 1]);
    for label in name.split('.') {
        if label.is_empty() || label.len() > LABEL_BYTES {
            return None;
        }
        message.push(label.len() as u8);
        message.extend(label.bytes());
    }
    message.push(0);
    message.extend(kind.to_be_bytes());
    message.extend(CLASS_IN.to_be_bytes());
    add_options(&mut message);
    Some(message)
}

pub(super) fn to_frame(message: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(2 + message.len());
    frame.extend((message.len() as u16).to_be_bytes());
    frame.extend(message);
    frame
}

/// The length of a message that comes over a stream, by the two bytes before it.
pub const fn message_length(prefix: [u8; 2]) -> usize {
    u16::from_be_bytes(prefix) as usize
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Answer {
    pub(super) id: u16,
    pub(super) addresses: Vec<IpAddr>,
    pub(super) seconds: u32,
    pub(super) is_failed: bool,
}

pub(super) fn skip_name(message: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *message.get(at)?;
        if length & POINTER == POINTER {
            return Some(at + 2);
        }
        if length == 0 {
            return Some(at + 1);
        }
        at += 1 + usize::from(length);
    }
}

pub(super) fn parse_answer(message: &[u8]) -> Option<Answer> {
    let (id, flags) = (be16(message, 0)?, be16(message, 2)?);
    if flags & QR == 0 {
        return None;
    }
    let (questions, records) = (be16(message, 4)?, be16(message, 6)?);
    let mut at = HEAD;
    for _ in 0..questions {
        at = skip_name(message, at)? + 4;
    }
    let (mut addresses, mut seconds) = (Vec::new(), u32::MAX);
    for _ in 0..records {
        at = skip_name(message, at)?;
        let (kind, class, length) = (be16(message, at)?, be16(message, at + 2)?, usize::from(be16(message, at + 8)?));
        let ttl = be32(message, at + 4)?;
        let ttl = if ttl & TTL_TOP_BIT == 0 { ttl } else { 0 };
        let data = message.get(at + 10..at + 10 + length)?;
        at += 10 + length;
        let address = match (kind, class) {
            (TYPE_A, CLASS_IN) => <[u8; 4]>::try_from(data).ok().map(IpAddr::from),
            (TYPE_AAAA, CLASS_IN) => <[u8; 16]>::try_from(data).ok().map(IpAddr::from),
            _ => None,
        };
        if let Some(address) = address {
            addresses.push(address);
            seconds = seconds.min(ttl);
        }
    }
    let rcode = flags & RCODE;
    Some(Answer {
        id,
        seconds: if addresses.is_empty() { 0 } else { seconds },
        addresses,
        is_failed: rcode != 0 && rcode != NXDOMAIN,
    })
}

#[cfg(test)]
pub(super) mod samples {
    use super::CLASS_IN;
    use alloc::vec::Vec;

    pub(in super::super) fn reply_message(id: u16, flags: u16, records: &[(u16, u32, &[u8])]) -> Vec<u8> {
        let mut message = id.to_be_bytes().to_vec();
        message.extend(flags.to_be_bytes());
        message.extend([0, 1, 0, records.len() as u8, 0, 0, 0, 0]);
        message.extend(b"\x07example\x03org\x00\x00\x01\x00\x01");
        for (kind, seconds, data) in records {
            // A pointer to the question's name.
            message.extend([0xC0, 12]);
            message.extend(kind.to_be_bytes());
            message.extend(CLASS_IN.to_be_bytes());
            message.extend(seconds.to_be_bytes());
            message.extend((data.len() as u16).to_be_bytes());
            message.extend(*data);
        }
        message
    }
}

#[cfg(test)]
mod tests {
    use super::samples::reply_message;
    use super::*;
    use alloc::vec;

    #[test]
    fn question_bytes() {
        let bytes = question(0x1234, "one.one.one.one", TYPE_A).unwrap();
        assert_eq!(
            bytes[..12],
            [0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1],
            "its number, the wish that the server ask on, one question and one record of options"
        );
        assert_eq!(
            &bytes[12..33],
            b"\x03one\x03one\x03one\x03one\x00\x00\x01\x00\x01",
            "the name label by label, an IPv4 address, of the internet"
        );
        assert_eq!(
            bytes[33..44],
            [0, 0, 41, 0x04, 0xD0, 0, 0, 0, 0, 0, 84],
            "the options: answers of 1232 bytes taken, and 84 bytes of them"
        );
        assert_eq!(
            (&bytes[44..48], bytes.len(), bytes[48..].iter().all(|byte| *byte == 0)),
            (&[0, 12, 0, 80][..], 128, true),
            "of which 80 fill the message up to 128"
        );
        let longer = question(7, "a-name-that-is-rather-long.with-several-labels.of-some-length.and-more-of-them.to-pass-a-block.example.org", TYPE_AAAA).unwrap();
        assert_eq!((longer.len(), longer[2..4] == [1, 0]), (256, true), "a longer one fills two blocks");
        assert_eq!(
            question(1, "example.org.", TYPE_AAAA).map(|bytes| bytes[12..29].to_vec()),
            Some(b"\x07example\x03org\x00\x00\x1C\x00\x01".to_vec()),
            "a dot at the end is the root, which is there anyway; an IPv6 address is kind 28"
        );
        assert_eq!(to_frame(&[1, 2, 3]), [0, 3, 1, 2, 3]);
        assert_eq!(
            (message_length([0, 3]), message_length([0x01, 0x00]), message_length([0xFF, 0xFF])),
            (3, 256, 65535),
            "and read back"
        );
        let label = "a".repeat(64);
        assert!(
            question(1, "", TYPE_A).is_none()
                && question(1, "a..b", TYPE_A).is_none()
                && question(1, &label, TYPE_A).is_none()
                && question(1, &label[..63], TYPE_A).is_some()
        );
        let long = [&label[..63]; 4].join(".");
        assert!(
            long.len() == 255 && question(1, &long, TYPE_A).is_none() && question(1, &long[..253], TYPE_A).is_some()
        );
    }

    fn reply(flags: u16, records: &[(u16, u32, &[u8])]) -> Vec<u8> {
        reply_message(0x1234, flags, records)
    }

    #[test]
    fn answer_records() {
        let other_name: &[u8] = b"\x03www\xC0\x0C";
        let actual = parse_answer(&reply(
            0x8180,
            &[(5, 300, other_name), (1, 120, &[93, 184, 216, 34]), (1, 60, &[93, 184, 216, 35])],
        ))
        .unwrap();
        assert_eq!(
            actual,
            Answer {
                id: 0x1234,
                addresses: vec![IpAddr::from([93, 184, 216, 34]), IpAddr::from([93, 184, 216, 35])],
                seconds: 60,
                is_failed: false
            },
            "the record that names another name is passed over, and the shortest time counts"
        );
        let v6 = [0x26, 0x06, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11];
        assert_eq!(
            parse_answer(&reply(0x8180, &[(28, 30, &v6)])).map(|answer| answer.addresses),
            Some(vec![IpAddr::from(v6)])
        );
        assert_eq!(
            parse_answer(&reply(0x8180, &[(1, 0x8000_0001, &[93, 184, 216, 34])]))
                .map(|answer| (answer.addresses.len(), answer.seconds)),
            Some((1, 0)),
            "a time with its top bit set is none, and the address is kept for nothing"
        );
        assert_eq!(
            parse_answer(&reply(0x8180, &[])),
            Some(Answer { id: 0x1234, addresses: vec![], seconds: 0, is_failed: false }),
            "a name without addresses of the kind asked for"
        );
        assert_eq!(
            parse_answer(&reply(0x8183, &[])).map(|answer| (answer.addresses.len(), answer.is_failed)),
            Some((0, false)),
            "a name that does not exist is an answer"
        );
        assert_eq!(
            parse_answer(&reply(0x8182, &[])).map(|answer| answer.is_failed),
            Some(true),
            "a server that could not say is none to keep"
        );
        assert_eq!(parse_answer(&reply(0x0100, &[])), None, "a question is no answer");
        assert_eq!(
            parse_answer(&reply(0x8180, &[(1, 60, &[1, 2, 3])])).map(|answer| answer.addresses.len()),
            Some(0),
            "an address of the wrong length is passed over"
        );
        let complete = reply(0x8180, &[(1, 120, &[93, 184, 216, 34])]);
        for length in 0..complete.len() {
            assert_eq!(parse_answer(&complete[..length]), None, "{length} bytes");
        }
    }
}
