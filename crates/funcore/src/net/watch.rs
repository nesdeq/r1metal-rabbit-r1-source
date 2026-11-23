//! The TCP segments to and from a port as a packet socket hands them over, as a log line that tells a middlebox from the server.

use super::{DESTINATION, ID_AT, LENGTH_AT, SOURCE, TCP, TTL_AT, VERSION_SHIFT, parse_ipv4_head};
use crate::base::bytes::be16;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::net::IpAddr;
use core::ops::Range;

/// The bytes of a packet that are read of it, the rest of it never: the longest IPv4 head and the TCP head behind it.
pub const HEADER_BYTES: usize = 80;
/// The bytes the watch's socket holds of what it is handed while a query runs, as the kernel takes the size of a receive buffer.
pub const BUFFER_BYTES: i32 = 1 << 22;
const IPV6: u8 = 6;
const IPV6_HEAD: usize = 40;
const PAYLOAD_AT: usize = 4;
const NEXT_AT: usize = 6;
const HOPS_AT: usize = 7;
const IPV6_FROM: Range<usize> = 8..24;
const IPV6_TO: Range<usize> = 24..40;
const SOURCE_AT: usize = 0;
const DESTINATION_AT: usize = 2;
const WORDS_AT: usize = 12;
const FLAGS_AT: usize = 13;
const WINDOW_AT: usize = 14;
const TCP_HEAD: usize = 20;
const FLAGS: [(u8, char); 8] =
    [(0x01, 'F'), (0x02, 'S'), (0x04, 'R'), (0x08, 'P'), (0x10, '.'), (0x20, 'U'), (0x40, 'E'), (0x80, 'W')];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Segment {
    peer: IpAddr,
    is_incoming: bool,
    flags: u8,
    payload_bytes: usize,
    hops: u8,
    id: Option<u16>,
    window: u16,
}

fn parse_segment(packet: &[u8], port: u16) -> Option<Segment> {
    let (head, from, to, hops, id, length) = if packet.first()? >> VERSION_SHIFT == IPV6 {
        let ipv6 = |range: Range<usize>| IpAddr::from(<[u8; 16]>::try_from(&packet[range]).unwrap_or_default());
        (packet.len() >= IPV6_HEAD && packet[NEXT_AT] == TCP).then_some(())?;
        let length = IPV6_HEAD + usize::from(be16(packet, PAYLOAD_AT)?);
        (IPV6_HEAD, ipv6(IPV6_FROM), ipv6(IPV6_TO), packet[HOPS_AT], None, length)
    } else {
        let (head, protocol) = parse_ipv4_head(packet)?;
        let ipv4 = |range: Range<usize>| IpAddr::from(<[u8; 4]>::try_from(&packet[range]).unwrap_or_default());
        (protocol == TCP).then_some(())?;
        let length = usize::from(be16(packet, LENGTH_AT)?);
        (head, ipv4(SOURCE), ipv4(DESTINATION), packet[TTL_AT], Some(be16(packet, ID_AT)?), length)
    };
    let tcp = packet.get(head..head + TCP_HEAD)?;
    let is_incoming = be16(tcp, SOURCE_AT)? == port;
    (is_incoming || be16(tcp, DESTINATION_AT)? == port).then_some(())?;
    let words = usize::from(tcp[WORDS_AT] >> 4) * 4;
    Some(Segment {
        peer: if is_incoming { from } else { to },
        is_incoming,
        flags: tcp[FLAGS_AT],
        payload_bytes: length.saturating_sub(head + words),
        hops,
        id,
        window: be16(tcp, WINDOW_AT)?,
    })
}

/// A watch on a port: the segments to and from it, in the order they went and came.
pub struct Watch {
    port: u16,
    segments: Vec<Segment>,
}

impl Watch {
    pub const fn new(port: u16) -> Watch {
        Watch { port, segments: Vec::new() }
    }

    /// Takes a packet of the network layer, `HEADER_BYTES` of it at the most; any but a TCP segment of the port is skipped.
    pub fn take_packet(&mut self, packet: &[u8]) {
        self.segments.extend(parse_segment(packet, self.port));
    }

    /// Each peer's segments: `>` went and `<` came, flags as tcpdump writes them, bytes, and of one that came its TTL, id and window; none where nothing was seen.
    pub fn to_log_line(&self) -> Option<String> {
        let mut peers: Vec<IpAddr> = Vec::new();
        for segment in &self.segments {
            if !peers.contains(&segment.peer) {
                peers.push(segment.peer);
            }
        }
        let mut line = String::new();
        for (peer_index, peer) in peers.iter().enumerate() {
            let _ = write!(line, "{}segments of {peer} port {}:", if peer_index > 0 { "; " } else { "" }, self.port);
            for (index, segment) in self.segments.iter().filter(|segment| segment.peer == *peer).enumerate() {
                line.push_str(if index > 0 { ", " } else { " " });
                line.push(if segment.is_incoming { '<' } else { '>' });
                FLAGS.iter().filter(|(bit, _)| segment.flags & bit != 0).for_each(|(_, letter)| line.push(*letter));
                if segment.payload_bytes > 0 {
                    let _ = write!(line, " {}", segment.payload_bytes);
                }
                if segment.is_incoming {
                    let _ = write!(line, " ttl {}", segment.hops);
                    if let Some(id) = segment.id {
                        let _ = write!(line, " id {id}");
                    }
                    let _ = write!(line, " win {}", segment.window);
                }
            }
        }
        (!line.is_empty()).then_some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::net::{Ipv4Addr, Ipv6Addr};

    fn tcp(from: u16, to: u16, flags: u8, window: u16) -> Vec<u8> {
        let mut head = vec![0u8; TCP_HEAD];
        head[SOURCE_AT..SOURCE_AT + 2].copy_from_slice(&from.to_be_bytes());
        head[DESTINATION_AT..DESTINATION_AT + 2].copy_from_slice(&to.to_be_bytes());
        (head[WORDS_AT], head[FLAGS_AT]) = (5 << 4, flags);
        head[WINDOW_AT..WINDOW_AT + 2].copy_from_slice(&window.to_be_bytes());
        head
    }

    fn ipv4(from: Ipv4Addr, to: Ipv4Addr, (ttl, id): (u8, u16), head: &[u8], payload_bytes: u16) -> Vec<u8> {
        let mut packet = vec![0x45, 0, 0, 0, 0, 0, 0, 0, ttl, TCP, 0, 0];
        packet[LENGTH_AT..LENGTH_AT + 2].copy_from_slice(&(20 + head.len() as u16 + payload_bytes).to_be_bytes());
        packet[ID_AT..ID_AT + 2].copy_from_slice(&id.to_be_bytes());
        [packet, from.octets().to_vec(), to.octets().to_vec(), head.to_vec()].concat()
    }

    fn ipv6(from: Ipv6Addr, to: Ipv6Addr, hops: u8, head: &[u8], payload_bytes: u16) -> Vec<u8> {
        let mut packet = vec![0x60, 0, 0, 0, 0, 0, TCP, hops];
        packet[PAYLOAD_AT..PAYLOAD_AT + 2].copy_from_slice(&(head.len() as u16 + payload_bytes).to_be_bytes());
        [packet, from.octets().to_vec(), to.octets().to_vec(), head.to_vec()].concat()
    }

    #[test]
    fn peers_log_line() {
        let (resolver, device) = (Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(192, 168, 1, 23));
        let mut watch = Watch::new(853);
        assert_eq!(watch.to_log_line(), None, "nothing seen, nothing told");
        watch.take_packet(&ipv4(device, resolver, (64, 1), &tcp(40000, 853, 0x02, 64240), 0));
        watch.take_packet(&ipv4(resolver, device, (57, 0), &tcp(853, 40000, 0x12, 65535), 0));
        watch.take_packet(&ipv4(device, resolver, (64, 2), &tcp(40000, 853, 0x18, 502), 517));
        watch.take_packet(&ipv4(resolver, device, (249, 4321), &tcp(853, 40000, 0x04, 0), 0));
        let cloudflare = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
        let station = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        watch.take_packet(&ipv6(station, cloudflare, 64, &tcp(40002, 853, 0x02, 64800), 0));
        watch.take_packet(&ipv6(cloudflare, station, 52, &tcp(853, 40002, 0x14, 0), 0));
        assert_eq!(
            watch.to_log_line().as_deref(),
            Some(
                "segments of 1.1.1.1 port 853: >S, <S. ttl 57 id 0 win 65535, >P. 517, <R ttl 249 id 4321 win 0; segments of 2606:4700:4700::1111 port 853: >S, <R. ttl 52 win 0"
            ),
            "each side by itself, what went with its flags and bytes, what came with its hops, identification and window"
        );
    }

    #[test]
    fn foreign_packets() {
        let (from, to) = (Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2));
        let mut watch = Watch::new(853);
        let mut udp = ipv4(from, to, (64, 1), &tcp(853, 1000, 0x10, 1), 0);
        udp[9] = 17;
        let other = ipv4(from, to, (64, 1), &tcp(443, 1000, 0x10, 1), 0);
        let cut = ipv4(from, to, (64, 1), &tcp(853, 1000, 0x10, 1), 0)[..30].to_vec();
        let mut extended = ipv6(Ipv6Addr::LOCALHOST, Ipv6Addr::LOCALHOST, 64, &tcp(853, 1000, 0x10, 1), 0);
        extended[NEXT_AT] = 0;
        for packet in [udp, other, cut, extended, Vec::new(), vec![0x45], vec![0x80; HEADER_BYTES]] {
            watch.take_packet(&packet);
        }
        assert_eq!(
            watch.to_log_line(),
            None,
            "another protocol, another port, a cut head, a header before TCP's, nothing"
        );
        watch.take_packet(&ipv4(Ipv4Addr::UNSPECIFIED, to, (64, 1), &tcp(853, 1000, 0x11, 1), 0));
        assert_eq!(watch.to_log_line().as_deref(), Some("segments of 0.0.0.0 port 853: <F. ttl 64 id 1 win 1"));
    }
}
