//! The protocols of a station in a network: ARP, DHCP, IPv6, DNS, connection racing, TCP's settings, TLS, WebSocket, SNTP and ICMP; a document fetched over HTTPS; a call's media packets: RTP, RTCP and SRTP, and their connection: STUN, TURN and ICE; and what a router of one host takes: address translation and a WireGuard tunnel.

pub mod arp;
pub mod dhcp;
pub mod dns;
pub(crate) mod ethernet;
pub mod fetch;
pub mod ice;
pub(crate) mod icmp;
pub mod ipv6;
pub(crate) mod nat;
pub mod race;
pub mod rtp;
pub(crate) mod siit;
pub mod sntp;
pub mod stun;
pub mod tcp;
pub mod tls;
pub(crate) mod turn;
pub mod watch;
pub mod websocket;
mod window;
pub mod wireguard;

use crate::base::bytes::be16;
use alloc::format;
use alloc::string::String;
use core::ops::Range;

/// The log line of a server reached, its times in milliseconds.
pub fn to_web_line(host: &str, name_ms: u128, address: &str, connect_ms: u128, tls: &str, tls_ms: u128) -> String {
    format!("web: {host} named in {name_ms} ms, reached at {address} after {connect_ms} ms, {tls} after {tls_ms} ms")
}

/// The largest packet on an Ethernet network, in bytes.
pub const MTU: usize = 1500;
const IPV4: u8 = 4;
const VERSION_SHIFT: u8 = 4;
const WORDS_MASK: u8 = 0x0F;
const WORD: usize = 4;
const IP_HEAD: usize = 20;
const LENGTH_AT: usize = 2;
const ID_AT: usize = 4;
const FRAGMENT_AT: usize = 6;
const FRAGMENT_BITS: u16 = 0x3FFF;
const MORE_FRAGMENTS: u16 = 0x2000;
const DONT_FRAGMENT: u16 = 0x4000;
// A fragment's place in its packet is counted in units of eight bytes.
const FRAGMENT_UNIT: usize = 8;
const TTL_AT: usize = 8;
const PROTOCOL_AT: usize = 9;
const CHECKSUM_AT: usize = 10;
const SOURCE: Range<usize> = 12..16;
const DESTINATION: Range<usize> = 16..20;
const ADDRESSES: Range<usize> = 12..20;
const ICMP: u8 = 1;
const TCP: u8 = 6;
const UDP: u8 = 17;
// The hops a packet made here may make (RFC 1700).
const TTL: u8 = 64;

// An IPv4 address.
type Address = [u8; 4];

fn parse_ipv4_head(packet: &[u8]) -> Option<(usize, u8)> {
    let first = *packet.first()?;
    let head = usize::from(first & WORDS_MASK) * WORD;
    (first >> VERSION_SHIFT == IPV4 && head >= IP_HEAD && packet.len() >= head).then(|| (head, packet[PROTOCOL_AT]))
}

// The head of an IPv4 packet made here, of a protocol's payload of that many bytes, with its checksum.
fn ipv4_head(protocol: u8, source: Address, target: Address, payload: usize) -> [u8; IP_HEAD] {
    let mut head = [0u8; IP_HEAD];
    head[0] = IPV4 << VERSION_SHIFT | (IP_HEAD / WORD) as u8;
    head[LENGTH_AT..LENGTH_AT + 2].copy_from_slice(&((IP_HEAD + payload) as u16).to_be_bytes());
    (head[TTL_AT], head[PROTOCOL_AT]) = (TTL, protocol);
    head[SOURCE].copy_from_slice(&source);
    head[DESTINATION].copy_from_slice(&target);
    set_ipv4_checksum(&mut head);
    head
}

// Writes the checksum of an IPv4 head, options included, into it.
fn set_ipv4_checksum(head: &mut [u8]) {
    head[CHECKSUM_AT..CHECKSUM_AT + 2].fill(0);
    let checksum = !sum(&[head]);
    head[CHECKSUM_AT..CHECKSUM_AT + 2].copy_from_slice(&checksum.to_be_bytes());
}

// A checksum after whole words it covers changed from `old` to `new` (RFC 1624, equation 3).
fn to_updated_sum(checksum: u16, old: &[u8], new: &[u8]) -> u16 {
    let unsummed = sum(&[&(!checksum).to_be_bytes(), new]);
    let removed = !sum(&[old]);
    !sum(&[&unsummed.to_be_bytes(), &removed.to_be_bytes()])
}

// A UDP checksum as it is sent: one that comes to nought goes as all ones, nought telling that there is none (RFC 768).
const fn to_udp_sum(checksum: u16) -> u16 {
    if checksum == 0 { u16::MAX } else { checksum }
}

// An IPv4 packet cut to the length its head tells, whatever filling follows it; none for what is no whole packet.
fn to_whole_ipv4(packet: &[u8]) -> Option<&[u8]> {
    let (head, _) = parse_ipv4_head(packet)?;
    packet.get(..usize::from(be16(packet, LENGTH_AT)?)).filter(|whole| whole.len() >= head)
}

fn sum(parts: &[&[u8]]) -> u16 {
    let mut total = 0u32;
    for part in parts {
        let (words, rest) = part.as_chunks::<2>();
        total += words.iter().map(|&word| u32::from(u16::from_be_bytes(word))).sum::<u32>();
        total += rest.first().map_or(0, |last| u32::from(u16::from_be_bytes([*last, 0])));
    }
    while total > u32::from(u16::MAX) {
        total = (total & u32::from(u16::MAX)) + (total >> u16::BITS);
    }
    total as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_line() {
        assert_eq!(
            to_web_line("api.openai.com", 3, "[2606:4700::6812:1a78]:443", 41, "TLS 1.3", 95),
            "web: api.openai.com named in 3 ms, reached at [2606:4700::6812:1a78]:443 after 41 ms, TLS 1.3 after 95 ms"
        );
    }

    #[test]
    fn internet_checksum() {
        assert_eq!(sum(&[&[0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7][..]]), 0xDDF2, "the example of RFC 1071");
        assert_eq!(
            sum(&[&[0x00, 0x01][..], &[0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7][..]]),
            0xDDF2,
            "in parts it is the same sum"
        );
        assert_eq!(sum(&[&[0x12, 0x34, 0x56][..]]), 0x1234 + 0x5600, "a last byte alone is the high half of a word");
    }
}
