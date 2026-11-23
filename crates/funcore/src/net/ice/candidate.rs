use super::transport_word;
use crate::base::checksum::crc32;
use crate::net::stun::Transport;
use alloc::format;
use alloc::string::{String, ToString};
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

// The one component of a stream that carries RTP and RTCP together (RFC 8445, 5.1.2.1).
const COMPONENT: u32 = 1;
// A priority's parts (RFC 8445, 5.1.2.1): 2^24 times the type preference, 2^8 times the local preference of 16 bits, and 256 less the component.
const TYPE_SHIFT: u32 = 24;
const LOCAL_SHIFT: u32 = 8;
const LOCAL_MOST: u32 = 0xFFFF;
const COMPONENT_ROOM: u32 = 256;
// The type preferences (RFC 8445, 5.1.2.2); a TCP candidate's one below its UDP kind (RFC 6544, 4.2), under TLS one below that.
pub(super) const HOST: u32 = 126;
const PRFLX: u32 = 110;
pub(super) const SRFLX: u32 = 100;
const RELAY: u32 = 0;
// The direction preference of an active TCP candidate and of a passive one (RFC 6544, 4.2).
const ACTIVE: u32 = 6;
const PASSIVE: u32 = 4;
const DIRECTION_SHIFT: u32 = 13;
const OTHER_MOST: u32 = 8191;
// Of a host's local preference of 16 bits, the bit that tells its family, and the most its order takes of the 13 bits below a direction, under that bit.
const FAMILY_SHIFT: u32 = 15;
const ORDER_MOST: u32 = OTHER_MOST >> 1;
// A candidate's priority is at least 1 and below 2^31 (RFC 8445, 5.1.2).
pub(super) const PRIORITIES: core::ops::RangeInclusive<u32> = 1..=(1 << 31) - 1;
// The port an active TCP candidate is told with (RFC 6544, 4.5).
pub(super) const DISCARD_PORT: u16 = 9;
// A foundation is 1 to 32 ice-chars (RFC 8839, 5.1).
const FOUNDATION_MOST: usize = 32;
const LINE_PREFIX: &str = "candidate:";
const SDP_PREFIX: &str = "a=";

/// A candidate's type (RFC 8445, 5.1.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Host,
    Srflx,
    Prflx,
    Relay,
}

impl Kind {
    pub(super) const fn word(self) -> &'static str {
        match self {
            Kind::Host => "host",
            Kind::Srflx => "srflx",
            Kind::Prflx => "prflx",
            Kind::Relay => "relay",
        }
    }

    const fn preference(self) -> u32 {
        match self {
            Kind::Host => HOST,
            Kind::Srflx => SRFLX,
            Kind::Prflx => PRFLX,
            Kind::Relay => RELAY,
        }
    }
}

// The direction of a TCP candidate (RFC 6544, 4.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Direction {
    Active,
    Passive,
    Simultaneous,
}

// A candidate, this side's or the far side's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Candidate {
    pub(super) kind: Kind,
    pub(super) transport: Transport,
    pub(super) address: SocketAddr,
    pub(super) priority: u32,
    pub(super) foundation: String,
    pub(super) related: Option<SocketAddr>,
    pub(super) direction: Option<Direction>,
    pub(super) ufrag: Option<String>,
}

pub(super) const fn priority(type_preference: u32, local_preference: u32) -> u32 {
    (type_preference << TYPE_SHIFT) | (local_preference << LOCAL_SHIFT) | (COMPONENT_ROOM - COMPONENT)
}

pub(super) const fn local_preference(priority: u32) -> u32 {
    priority >> LOCAL_SHIFT & LOCAL_MOST
}

// The type preference of a kind over a transport: TCP below UDP, TLS below TCP.
pub(super) const fn type_preference(kind: Kind, transport: Transport) -> u32 {
    let below = match transport {
        Transport::Udp => 0,
        Transport::Tcp => 1,
        Transport::Tls => 2,
    };
    kind.preference().saturating_sub(below)
}

// The local preference of a TCP candidate: its direction above the preference of its address (RFC 6544, 4.2), of which the family stays above the order, as in the host preference of 16 bits it is made of.
pub(super) const fn tcp_preference(is_active: bool, address: u32) -> u32 {
    let family = (address >> FAMILY_SHIFT) & 1;
    let other = (family * (ORDER_MOST + 1)) | (address & ORDER_MOST);
    (if is_active { ACTIVE } else { PASSIVE }) << DIRECTION_SHIFT | other
}

// A foundation, the same for candidates of one type, base address, server and transport (RFC 8445, 5.1.1.3).
pub(super) fn foundation(kind: Kind, base: IpAddr, server: Option<IpAddr>, transport: Transport) -> String {
    let octets = |ip: IpAddr| match ip {
        IpAddr::V4(ip) => ip.to_ipv6_mapped().octets(),
        IpAddr::V6(ip) => ip.octets(),
    };
    let mut bytes = alloc::vec![kind as u8, transport as u8];
    bytes.extend(octets(base));
    bytes.extend(server.map_or([0; 16], octets));
    crc32(&bytes).to_string()
}

// The characters of foundations and credentials (RFC 8839, 5.1).
pub(super) fn is_ice_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '+' || character == '/'
}

impl Candidate {
    // The attribute line libwebrtc exchanges in signalling (RFC 8839, 5.1), of a candidate over UDP, as every one told is, with this side's user fragment; `is_hidden` writes the related address as nought, so that it tells nothing of this side.
    pub(super) fn to_line(&self, ufrag: &str, is_hidden: bool) -> String {
        let mut line = format!(
            "{LINE_PREFIX}{} {COMPONENT} {} {} {} {} typ {}",
            self.foundation,
            transport_word(self.transport),
            self.priority,
            self.address.ip(),
            self.address.port(),
            self.kind.word()
        );
        if let Some(related) = self.related {
            let related = if is_hidden {
                match related {
                    SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                    SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
                }
            } else {
                related
            };
            line.push_str(&format!(" raddr {} rport {}", related.ip(), related.port()));
        }
        line.push_str(&format!(" generation 0 ufrag {ufrag}"));
        line
    }

    // Reads a candidate's attribute line, with or without `a=`; none for one of another component, transport or kind, an address that is a name or no address, or a malformed one.
    pub(super) fn parse(line: &str) -> Option<Candidate> {
        let line = line.trim();
        let line = line.strip_prefix(SDP_PREFIX).unwrap_or(line);
        let rest = line.get(..LINE_PREFIX.len()).filter(|prefix| prefix.eq_ignore_ascii_case(LINE_PREFIX))?;
        let mut words = line[rest.len()..].split_ascii_whitespace();
        let foundation = words.next()?;
        if foundation.is_empty() || foundation.len() > FOUNDATION_MOST || !foundation.chars().all(is_ice_char) {
            return None;
        }
        if words.next()?.parse::<u32>().ok()? != COMPONENT {
            return None;
        }
        let transport = match words.next()?.to_ascii_lowercase().as_str() {
            "udp" => Transport::Udp,
            "tcp" => Transport::Tcp,
            _ => return None,
        };
        let priority: u32 = words.next()?.parse().ok().filter(|priority| PRIORITIES.contains(priority))?;
        let ip: IpAddr = words.next()?.parse().ok()?;
        let port: u16 = words.next()?.parse().ok()?;
        if words.next()? != "typ" {
            return None;
        }
        let kind = match words.next()? {
            "host" => Kind::Host,
            "srflx" => Kind::Srflx,
            "prflx" => Kind::Prflx,
            "relay" => Kind::Relay,
            _ => return None,
        };
        let (mut related_ip, mut related_port, mut direction, mut ufrag) = (None, None, None, None);
        while let Some(key) = words.next() {
            let value = words.next()?;
            match key {
                "raddr" => related_ip = Some(value.parse::<IpAddr>().ok()?),
                "rport" => related_port = Some(value.parse::<u16>().ok()?),
                "tcptype" => {
                    direction = Some(match value {
                        "active" => Direction::Active,
                        "passive" => Direction::Passive,
                        "so" => Direction::Simultaneous,
                        _ => return None,
                    })
                }
                "ufrag" => ufrag = Some(value.to_string()),
                _ => {}
            }
        }
        if ip.is_unspecified() || (port == 0 && direction != Some(Direction::Active)) {
            return None;
        }
        if (transport == Transport::Tcp) != direction.is_some() {
            return None;
        }
        Some(Candidate {
            kind,
            transport,
            address: SocketAddr::new(ip, port),
            priority,
            foundation: foundation.to_string(),
            related: related_ip.zip(related_port).map(|(ip, port)| SocketAddr::new(ip, port)),
            direction,
            ufrag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities() {
        assert_eq!(priority(HOST, 65535), 2_130_706_431, "RFC 8445's host of one address");
        assert_eq!(priority(PRFLX, 65535), 1_862_270_975);
        assert_eq!(priority(SRFLX, 65535), 1_694_498_815);
        assert_eq!(priority(RELAY, 65535), 16_777_215);
        assert_eq!(
            [priority(HOST, tcp_preference(true, 0xFFFF)), priority(HOST, tcp_preference(false, 0xFFFF))],
            [2_128_609_279, 2_124_414_975],
            "RFC 6544, appendix C's active and passive host candidates, of the first IPv6 address"
        );
        let other = |address: u32| tcp_preference(false, address) & OTHER_MOST;
        assert_eq!(
            [other(0xFFFF), other(0xFFFE), other(0x7FFF), other(0x7FFE)],
            [8191, 8190, 4095, 4094],
            "over TCP too an IPv6 address stands above an IPv4 one, and each address of a family above the next"
        );
        assert!(
            type_preference(Kind::Host, Transport::Tls) < type_preference(Kind::Host, Transport::Tcp)
                && type_preference(Kind::Host, Transport::Tcp) < type_preference(Kind::Host, Transport::Udp),
            "UDP before TCP before TLS"
        );
        assert_eq!(type_preference(Kind::Relay, Transport::Tls), 0);
    }

    #[test]
    fn foundations() {
        let ip: IpAddr = "192.0.2.4".parse().unwrap();
        let other: IpAddr = "192.0.2.5".parse().unwrap();
        let server: IpAddr = "198.51.100.1".parse().unwrap();
        let of = |kind, base, server, transport| foundation(kind, base, server, transport);
        assert_eq!(
            of(Kind::Srflx, ip, Some(server), Transport::Udp),
            of(Kind::Srflx, ip, Some(server), Transport::Udp)
        );
        for different in [
            of(Kind::Host, ip, Some(server), Transport::Udp),
            of(Kind::Srflx, other, Some(server), Transport::Udp),
            of(Kind::Srflx, ip, Some(other), Transport::Udp),
            of(Kind::Srflx, ip, Some(server), Transport::Tcp),
        ] {
            assert_ne!(
                different,
                of(Kind::Srflx, ip, Some(server), Transport::Udp),
                "type, base, server and transport each tell"
            );
        }
    }

    #[test]
    fn libwebrtc_lines() {
        let host = Candidate::parse(
            "candidate:1467250027 1 udp 2122260223 192.168.0.196 46243 typ host generation 0 ufrag EsAw network-id 1 network-cost 10",
        )
        .unwrap();
        assert_eq!(
            (host.kind, host.transport, host.address, host.priority, host.foundation.as_str(), host.ufrag.as_deref()),
            (
                Kind::Host,
                Transport::Udp,
                "192.168.0.196:46243".parse().unwrap(),
                2_122_260_223,
                "1467250027",
                Some("EsAw")
            )
        );
        let srflx = Candidate::parse(
            "a=candidate:842163049 1 UDP 1686052607 203.0.113.4 46243 typ srflx raddr 192.168.0.196 rport 46243 generation 0",
        )
        .unwrap();
        assert_eq!((srflx.kind, srflx.related), (Kind::Srflx, Some("192.168.0.196:46243".parse().unwrap())));
        let relay = Candidate::parse(
            "candidate:3 1 udp 41885439 2001:db8::7 50000 typ relay raddr 2001:db8::1 rport 3 generation 0",
        )
        .unwrap();
        assert_eq!((relay.kind, relay.address), (Kind::Relay, "[2001:db8::7]:50000".parse().unwrap()));
        let passive = Candidate::parse("candidate:2 1 TCP 2124414975 10.0.1.1 8998 typ host tcptype passive").unwrap();
        assert_eq!((passive.transport, passive.direction), (Transport::Tcp, Some(Direction::Passive)));
        let active = Candidate::parse("candidate:1 1 TCP 2128609279 10.0.1.1 9 typ host tcptype active").unwrap();
        assert_eq!(active.direction, Some(Direction::Active));
        for none in [
            "candidate:FAKE 1 tcp 0 127.0.0.1 0 typ host",
            "candidate:1 2 udp 2122260223 192.168.0.196 46243 typ host",
            "candidate:1 1 sctp 2122260223 192.168.0.196 46243 typ host",
            "candidate:1 1 udp 2122260223 abcd.local 46243 typ host",
            "candidate:1 1 udp 2122260223 0.0.0.0 46243 typ host",
            "candidate:1 1 udp 2122260223 192.168.0.196 0 typ host",
            "candidate:1 1 udp 2122260223 192.168.0.196 46243 typ peer",
            "candidate:1 1 udp 2122260223 192.168.0.196 46243 host",
            "candidate:1 1 udp 2122260223 192.168.0.196 46243 typ host raddr",
            "candidate:1 1 tcp 2122260223 192.168.0.196 46243 typ host",
            "candidate:1 1 tcp 2122260223 192.168.0.196 46243 typ host tcptype sideways",
            "candidate:has-dash 1 udp 2122260223 192.168.0.196 46243 typ host",
            "",
            "candidate:",
        ] {
            assert_eq!(Candidate::parse(none), None, "{none}");
        }
    }

    #[test]
    fn lines_round_trip() {
        let candidate = Candidate {
            kind: Kind::Relay,
            transport: Transport::Udp,
            address: "[2001:db8::7]:50000".parse().unwrap(),
            priority: priority(RELAY, 65535),
            foundation: foundation(Kind::Relay, "2001:db8::7".parse().unwrap(), None, Transport::Udp),
            related: Some("198.51.100.4:3478".parse().unwrap()),
            direction: None,
            ufrag: None,
        };
        let line = candidate.to_line("Ab12", false);
        assert!(line.ends_with("typ relay raddr 198.51.100.4 rport 3478 generation 0 ufrag Ab12"), "{line}");
        assert_eq!(Candidate::parse(&line), Some(Candidate { ufrag: Some("Ab12".into()), ..candidate.clone() }));
        let hidden = candidate.to_line("Ab12", true);
        assert!(hidden.contains("raddr 0.0.0.0 rport 0") && !hidden.contains("198.51.100.4"), "{hidden}");
    }
}
