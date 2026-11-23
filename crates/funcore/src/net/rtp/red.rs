use super::packet::PAYLOAD_TYPE;
use crate::base::bytes::be32;

// RFC 2198, 3: the F bit over a block's payload type; a redundant block's header holds a timestamp offset of 14 bits and a length of 10 after it, the primary block's the byte alone.
const FOLLOWS: u8 = 0x80;
const REDUNDANT_HEAD: usize = 4;
const LENGTH_BITS: u32 = 0x03FF;

// The payload type and the data of a RED payload's primary block, its redundant blocks passed over; none where the headers or the lengths do not fit.
pub(super) fn primary(payload: &[u8]) -> Option<(u8, &[u8])> {
    let (mut at, mut redundant) = (0, 0usize);
    while *payload.get(at)? & FOLLOWS != 0 {
        redundant += (be32(payload, at)? & LENGTH_BITS) as usize;
        at += REDUNDANT_HEAD;
    }
    let payload_type = payload[at] & PAYLOAD_TYPE;
    Some((payload_type, payload.get(at + 1 + redundant..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_blocks() {
        assert_eq!(primary(&[108, 0x90, 1, 2]), Some((108, &[0x90, 1, 2][..])), "libwebrtc's VP8 in RED: one byte");
        assert_eq!(primary(&[122, 9]), Some((122, &[9][..])), "ULPFEC in RED");
        let redundant = [0x80 | 102, 0x03, 0xC0, 0x02, 102, 0xAA, 0xBB, 0xCC, 0xDD];
        assert_eq!(
            primary(&redundant),
            Some((102, &[0xCC, 0xDD][..])),
            "RFC 2198, 3: a redundant block's header, its offset 240 and its length 2, the primary's, the redundant data, then the primary's"
        );
        assert_eq!(primary(&[0x80 | 102, 0, 0, 9, 102, 1]), None, "a redundant block longer than the payload");
        assert_eq!(primary(&[0x80, 0, 0]), None, "a redundant header cut short");
        assert_eq!(primary(&[]), None);
        assert_eq!(primary(&[108]), Some((108, &[][..])), "a primary block of no data");
    }
}
