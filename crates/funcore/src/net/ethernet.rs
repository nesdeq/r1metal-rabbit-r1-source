use crate::base::bytes::be16;
use alloc::vec::Vec;

// A station's hardware address.
pub(crate) type Hardware = [u8; 6];

// The address every station of a link takes.
pub(crate) const BROADCAST: Hardware = [0xFF; 6];
const HEAD: usize = 14;
const SOURCE_AT: usize = 6;
const KIND_AT: usize = 12;
// The numbers a link names IPv4 and IPv6 packets by.
pub(crate) const IPV4: u16 = 0x0800;
pub(crate) const IPV6: u16 = 0x86DD;
const ARP: u16 = 0x0806;

// What a frame of a link of two stations carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Ipv4,
    Arp,
}

impl Kind {
    const fn number(self) -> u16 {
        match self {
            Kind::Ipv4 => IPV4,
            Kind::Arp => ARP,
        }
    }
}

// A frame's sender, what it carries, and that; none for a frame of any other kind.
pub(crate) fn parse(frame: &[u8]) -> Option<(Hardware, Kind, &[u8])> {
    let kind = match be16(frame, KIND_AT)? {
        IPV4 => Kind::Ipv4,
        ARP => Kind::Arp,
        _ => return None,
    };
    Some((*frame[SOURCE_AT..].first_chunk()?, kind, &frame[HEAD..]))
}

// A frame from `source` to `target`.
pub(crate) fn to_frame(target: Hardware, source: Hardware, kind: Kind, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEAD + payload.len());
    frame.extend(target);
    frame.extend(source);
    frame.extend(kind.number().to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let (host, router) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9]);
        let frame = to_frame(router, host, Kind::Ipv4, &[0x45, 0, 0, 20]);
        assert_eq!(
            frame,
            [2, 0, 0, 0, 0, 9, 2, 0, 0, 0, 0, 1, 0x08, 0x00, 0x45, 0, 0, 20],
            "the receiver, the sender, the kind, the packet"
        );
        assert_eq!(parse(&frame), Some((host, Kind::Ipv4, &[0x45, 0, 0, 20][..])));
        assert_eq!(parse(&to_frame(BROADCAST, host, Kind::Arp, &[1])), Some((host, Kind::Arp, &[1][..])));
        let mut ipv6 = frame.clone();
        ipv6[12..14].copy_from_slice(&[0x86, 0xDD]);
        assert_eq!(
            (parse(&ipv6), parse(&frame[..13]), parse(&frame[..14]).map(|(.., payload)| payload.len())),
            (None, None, Some(0))
        );
    }
}
