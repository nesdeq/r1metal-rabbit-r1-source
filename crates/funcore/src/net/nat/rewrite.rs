use crate::base::bytes::be16;
use crate::net::{
    Address, DESTINATION, DONT_FRAGMENT, FRAGMENT_AT, FRAGMENT_BITS, FRAGMENT_UNIT, ICMP, ID_AT, LENGTH_AT,
    MORE_FRAGMENTS, PROTOCOL_AT, SOURCE, TCP, UDP, icmp, parse_ipv4_head, set_ipv4_checksum, to_udp_sum,
    to_updated_sum,
};
use alloc::vec::Vec;

const SOURCE_PORT_AT: usize = 0;
const TARGET_PORT_AT: usize = 2;
const UDP_CHECKSUM_AT: usize = 6;
const TCP_WORDS_AT: usize = 12;
const TCP_FLAGS_AT: usize = 13;
const TCP_CHECKSUM_AT: usize = 16;
const TCP_HEAD: usize = 20;
// What an error tells of the packet it is about beyond that packet's IP head (RFC 792).
const QUOTED: usize = 8;
pub(super) const FIN: u8 = 0x01;
pub(super) const SYN: u8 = 0x02;
pub(super) const RST: u8 = 0x04;
pub(super) const ACK: u8 = 0x10;
const OPTION_END: u8 = 0;
const OPTION_NONE: u8 = 1;
const OPTION_MSS: u8 = 2;
const MSS_BYTES: usize = 4;
// The IPv4 and TCP heads a segment of the largest packet has (RFC 6691).
const HEADS: usize = 40;
const ECHO_ID_AT: usize = 4;
const ICMP_CHECKSUM_AT: usize = 2;

// What a flow's packets are told apart by beside their addresses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Protocol {
    Tcp,
    Udp,
    Echo,
}

impl Protocol {
    pub(super) const COUNT: usize = 3;
}

// An address and what a protocol tells its flows apart by there: a port, or an echo's number at the asker's end and nought at the other.
pub(super) type End = (Address, u16);

// Where a packet stands among the pieces of what it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Piece {
    Whole,
    First,
    Later,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    // A packet of a flow: its protocol, and TCP's flags.
    Flow(Protocol, u8),
    // An error about a packet of a flow, whose head it quotes.
    Error,
    // A later piece, which names no port: the number its pieces share.
    Piece(u16),
}

// A packet as the translation reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Parsed {
    pub(super) head: usize,
    pub(super) kind: Kind,
    pub(super) piece: Piece,
    pub(super) source: End,
    pub(super) target: End,
}

fn address(packet: &[u8], place: core::ops::Range<usize>) -> Address {
    packet[place].try_into().expect("four bytes of a head")
}

// The two ends a transport head of `protocol` names; an echo's number stands at the end of whoever asked.
fn read_ends(packet: &[u8], head: usize, protocol: u8) -> Option<(Protocol, End, End, u8)> {
    let (source, target) = (address(packet, SOURCE), address(packet, DESTINATION));
    let transport = packet.get(head..).filter(|transport| transport.len() >= QUOTED)?;
    match protocol {
        TCP | UDP => {
            let ports = (be16(transport, SOURCE_PORT_AT)?, be16(transport, TARGET_PORT_AT)?);
            let flags = transport.get(TCP_FLAGS_AT).copied().unwrap_or_default();
            let kind = if protocol == TCP { Protocol::Tcp } else { Protocol::Udp };
            Some((kind, (source, ports.0), (target, ports.1), flags))
        }
        ICMP => {
            let number = be16(transport, ECHO_ID_AT)?;
            match transport[0] {
                icmp::REQUEST => Some((Protocol::Echo, (source, number), (target, 0), 0)),
                icmp::REPLY => Some((Protocol::Echo, (source, 0), (target, number), 0)),
                _ => None,
            }
        }
        _ => None,
    }
}

// Reads an IPv4 packet; none for what the translation passes none of.
pub(super) fn parse(packet: &[u8]) -> Option<Parsed> {
    let (head, protocol) = parse_ipv4_head(packet)?;
    let fragment = be16(packet, FRAGMENT_AT)?;
    let (source, target) = (address(packet, SOURCE), address(packet, DESTINATION));
    if fragment & FRAGMENT_BITS & !MORE_FRAGMENTS != 0 {
        let kind = Kind::Piece(be16(packet, ID_AT)?);
        return Some(Parsed { head, kind, piece: Piece::Later, source: (source, 0), target: (target, 0) });
    }
    let piece = if fragment & MORE_FRAGMENTS == 0 { Piece::Whole } else { Piece::First };
    if protocol == ICMP && icmp::is_error(*packet.get(head)?) {
        return (piece == Piece::Whole).then_some(Parsed {
            head,
            kind: Kind::Error,
            piece,
            source: (source, 0),
            target: (target, 0),
        });
    }
    if protocol == TCP && packet.len() < head + TCP_HEAD {
        return None;
    }
    let (protocol, source, target, flags) = read_ends(packet, head, protocol)?;
    Some(Parsed { head, kind: Kind::Flow(protocol, flags), piece, source, target })
}

// The number the pieces of a packet share, and its protocol's own number.
pub(super) fn piece_mark(packet: &[u8]) -> (u8, u16) {
    (packet[PROTOCOL_AT], be16(packet, ID_AT).unwrap_or_default())
}

// Writes a checksum's new value after the words at `place` of `packet` changed to `new`, and those words; a UDP datagram sent without a checksum keeps none.
fn replace(packet: &mut [u8], checksum_at: Option<usize>, place: usize, new: &[u8], is_optional: bool) {
    if let Some(at) = checksum_at.filter(|at| packet.len() >= at + 2) {
        let old = be16(packet, at).unwrap_or_default();
        if !(is_optional && old == 0) {
            let updated = to_updated_sum(old, &packet[place..place + new.len()], new);
            let updated = if is_optional { to_udp_sum(updated) } else { updated };
            packet[at..at + 2].copy_from_slice(&updated.to_be_bytes());
        }
    }
    packet[place..place + new.len()].copy_from_slice(new);
}

// Where the transport checksum of a packet's protocol lies, where the packet holds one that covers its addresses and ports.
fn checksum_place(packet: &[u8], head: usize, is_later: bool) -> (Option<usize>, bool) {
    match packet[PROTOCOL_AT] {
        _ if is_later => (None, false),
        TCP => (Some(head + TCP_CHECKSUM_AT), false),
        UDP => (Some(head + UDP_CHECKSUM_AT), true),
        _ => (None, false),
    }
}

// Rewrites the two ends of a packet read as `parsed`, its checksums with them; a later piece takes the addresses alone.
pub(super) fn set_ends(packet: &mut [u8], parsed: &Parsed, source: End, target: End) {
    let head = parsed.head;
    let is_later = parsed.piece == Piece::Later;
    let (checksum_at, is_optional) = checksum_place(packet, head, is_later);
    replace(packet, checksum_at, SOURCE.start, &source.0, is_optional);
    replace(packet, checksum_at, DESTINATION.start, &target.0, is_optional);
    match parsed.kind {
        Kind::Flow(Protocol::Tcp | Protocol::Udp, _) => {
            replace(packet, checksum_at, head + SOURCE_PORT_AT, &source.1.to_be_bytes(), is_optional);
            replace(packet, checksum_at, head + TARGET_PORT_AT, &target.1.to_be_bytes(), is_optional);
        }
        Kind::Flow(Protocol::Echo, _) => {
            let number = if packet[head] == icmp::REQUEST { source.1 } else { target.1 };
            replace(packet, Some(head + ICMP_CHECKSUM_AT), head + ECHO_ID_AT, &number.to_be_bytes(), false);
        }
        Kind::Error | Kind::Piece(_) => {}
    }
    set_ipv4_checksum(&mut packet[..head]);
}

// The packet an error quotes: the ends of its flow as its sender named them.
pub(super) fn parse_quoted(packet: &[u8], head: usize) -> Option<(Protocol, End, End)> {
    let quoted = packet.get(head + icmp::HEADER..)?;
    let (quoted_head, protocol) = parse_ipv4_head(quoted)?;
    let (protocol, source, target, _) = read_ends(quoted, quoted_head, protocol)?;
    Some((protocol, source, target))
}

// Rewrites an error and the packet it quotes: the error's own ends, the quoted packet's, and every checksum the error holds whole.
pub(super) fn set_error_ends(packet: &mut [u8], head: usize, own: (Address, Address), quoted: (End, End)) {
    packet[SOURCE].copy_from_slice(&own.0);
    packet[DESTINATION].copy_from_slice(&own.1);
    let at = head + icmp::HEADER;
    let inner = &mut packet[at..];
    let (inner_head, protocol) = parse_ipv4_head(inner).expect("an error read with the packet it quotes");
    inner[SOURCE].copy_from_slice(&quoted.0.0);
    inner[DESTINATION].copy_from_slice(&quoted.1.0);
    match protocol {
        TCP | UDP => {
            inner[inner_head + SOURCE_PORT_AT..][..2].copy_from_slice(&quoted.0.1.to_be_bytes());
            inner[inner_head + TARGET_PORT_AT..][..2].copy_from_slice(&quoted.1.1.to_be_bytes());
        }
        _ => {
            let number = if inner[inner_head] == icmp::REQUEST { quoted.0.1 } else { quoted.1.1 };
            inner[inner_head + ECHO_ID_AT..][..2].copy_from_slice(&number.to_be_bytes());
        }
    }
    set_ipv4_checksum(&mut inner[..inner_head]);
    icmp::set_checksum(&mut packet[head..]);
    set_ipv4_checksum(&mut packet[..head]);
}

// Lowers the largest segment a TCP packet that opens a connection asks for to what packets of `most` bytes carry.
pub(super) fn clamp_segment(packet: &mut [u8], head: usize, most: usize) {
    let limit = most.saturating_sub(HEADS).min(usize::from(u16::MAX)) as u16;
    let Some(words) = packet.get(head + TCP_WORDS_AT).map(|words| usize::from(words >> 4) * 4) else {
        return;
    };
    let (mut at, end) = (head + TCP_HEAD, (head + words).min(packet.len()));
    while at + 1 < end {
        match packet[at] {
            OPTION_END => return,
            OPTION_NONE => at += 1,
            OPTION_MSS if at + MSS_BYTES <= end => {
                if be16(packet, at + 2).is_some_and(|asked| asked > limit) {
                    replace(packet, Some(head + TCP_CHECKSUM_AT), at + 2, &limit.to_be_bytes(), false);
                }
                return;
            }
            _ => at += usize::from(packet[at + 1]).max(2),
        }
    }
}

// True for a packet its sender forbids to cut into pieces.
pub(super) fn is_whole_alone(packet: &[u8]) -> bool {
    be16(packet, FRAGMENT_AT).is_some_and(|fragment| fragment & DONT_FRAGMENT != 0)
}

// A packet in pieces of `most` bytes at the most, each with the head and its own place (RFC 791).
pub(super) fn cut(packet: &[u8], head: usize, most: usize) -> Vec<Vec<u8>> {
    let room = most.saturating_sub(head) / FRAGMENT_UNIT * FRAGMENT_UNIT;
    let field = be16(packet, FRAGMENT_AT).unwrap_or_default();
    let (first, follows) = (usize::from(field & FRAGMENT_BITS & !MORE_FRAGMENTS), field & MORE_FRAGMENTS != 0);
    let payload = &packet[head..];
    if room == 0 {
        return Vec::new();
    }
    let count = payload.len().div_ceil(room);
    payload
        .chunks(room)
        .enumerate()
        .map(|(index, chunk)| {
            let mut piece = packet[..head].to_vec();
            piece.extend_from_slice(chunk);
            let more = if index + 1 < count || follows { MORE_FRAGMENTS } else { 0 };
            let place = (first + index * room / FRAGMENT_UNIT) as u16 | more;
            piece[LENGTH_AT..LENGTH_AT + 2].copy_from_slice(&((head + chunk.len()) as u16).to_be_bytes());
            piece[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&place.to_be_bytes());
            set_ipv4_checksum(&mut piece[..head]);
            piece
        })
        .collect()
}
