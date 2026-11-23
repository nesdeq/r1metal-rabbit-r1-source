//! DHCP for IPv4 (RFC 2131, 2132, 4039): a station's messages, their resending and the leases held, and a server that lends one host one address.

use super::{ADDRESSES, Address, FRAGMENT_AT, FRAGMENT_BITS, LENGTH_AT, UDP, ipv4_head, parse_ipv4_head, sum};
use crate::base::bytes::be16;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

const NO_ADDRESS: Address = [0; 4];
const BROADCAST: Address = [255; 4];
const STATION_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
// The ports a message goes between, its sender's first.
const TO_SERVER: (u16, u16) = (STATION_PORT, SERVER_PORT);
const TO_STATION: (u16, u16) = (SERVER_PORT, STATION_PORT);
const UDP_HEAD: usize = 8;
const FIXED: usize = 236;
const REQUEST: u8 = 1;
const REPLY: u8 = 2;
const ETHERNET: u8 = 1;
const ID_AT: usize = 4;
const FLAGS_AT: usize = 10;
const CIADDR_AT: usize = 12;
const YIADDR_AT: usize = 16;
const GIADDR_AT: usize = 24;
const HARDWARE_AT: usize = 28;
const HARDWARE_BYTES: usize = 6;
const COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const PAD: u8 = 0;
const MASK: u8 = 1;
const ROUTER: u8 = 3;
const RESOLVER: u8 = 6;
const INTERFACE_MTU: u8 = 26;
const REQUESTED_ADDRESS: u8 = 50;
const LEASE_SECONDS: u8 = 51;
const MESSAGE_TYPE: u8 = 53;
const SERVER: u8 = 54;
const PARAMETER_LIST: u8 = 55;
const RENEW_SECONDS: u8 = 58;
const RAPID_COMMIT: u8 = 80;
const END: u8 = 255;
const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;
const INFORM: u8 = 8;
// A lease without end (RFC 2131, 3.3).
const FOREVER: u32 = u32::MAX;
const RENEW_SHARE: u32 = 2;
pub(crate) const FIRST_WAIT: Duration = Duration::from_secs(4);

struct Resend {
    wait: Duration,
    due: Duration,
}

impl Resend {
    const fn new(now: Duration) -> Resend {
        Resend { wait: FIRST_WAIT, due: now.saturating_add(FIRST_WAIT) }
    }

    fn take_due(&mut self, now: Duration) -> bool {
        let is_due = now >= self.due;
        if is_due {
            self.wait = self.wait.saturating_mul(2);
            self.due = now.saturating_add(self.wait);
        }
        is_due
    }
}

/// An address a network has lent, and its terms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lease {
    pub address: Address,
    pub mask: Address,
    pub router: Option<Address>,
    server: Address,
    seconds: u32,
    renew_seconds: u32,
}

impl Lease {
    fn prefix(&self) -> u32 {
        u32::from_be_bytes(self.mask).leading_ones()
    }

    fn to_log_line(self) -> String {
        let router = self
            .router
            .map_or_else(|| String::from("no router"), |router| format!("router {}", Ipv4Addr::from(router)));
        format!(
            "{}/{}, {router}, lent for {} s by {}",
            Ipv4Addr::from(self.address),
            self.prefix(),
            self.seconds,
            Ipv4Addr::from(self.server)
        )
    }

    /// The log line of it on the interface `ms` after it was asked for, `is_kept` if it stood from a stay before.
    pub fn to_bound_line(&self, ms: u128, is_kept: bool) -> String {
        let suffix = if is_kept { ", kept from the stay before" } else { "" };
        format!("wifi: address {} after {ms} ms{suffix}", self.to_log_line())
    }
}

/// The leases the device holds, each by the access point's hardware address, with the time it was given at.
#[derive(Default)]
pub struct Leases(BTreeMap<[u8; 6], (Lease, Duration)>);

impl Leases {
    /// The access point's lease not yet due for renewal at `now`: it goes on the interface with nothing asked.
    pub fn find_current(&self, access: &[u8; 6], now: Duration) -> Option<Lease> {
        let (lease, start) = self.0.get(access)?;
        (now.saturating_sub(*start).as_secs() < u64::from(lease.renew_seconds)).then_some(*lease)
    }

    /// Keeps the lease a join to the access point bound at `now`, in place of any before; the current one, bound again with nothing asked, keeps the time it was given at.
    pub fn keep(&mut self, access: [u8; 6], lease: Lease, now: Duration) {
        if self.find_current(&access, now) != Some(lease) {
            self.0.insert(access, (lease, now));
        }
    }
}

fn udp_sum(addresses: &[u8], datagram: &[u8]) -> u16 {
    sum(&[addresses, &[0, UDP], &(datagram.len() as u16).to_be_bytes(), datagram])
}

fn udp_packet(source: Address, target: Address, (from, to): (u16, u16), payload: &[u8]) -> Vec<u8> {
    let length = (UDP_HEAD + payload.len()) as u16;
    let mut packet = ipv4_head(UDP, source, target, usize::from(length)).to_vec();
    let mut datagram = Vec::with_capacity(usize::from(length));
    datagram.extend(from.to_be_bytes());
    datagram.extend(to.to_be_bytes());
    datagram.extend(length.to_be_bytes());
    datagram.extend([0, 0]);
    datagram.extend(payload);
    // A sum of zero is sent as all ones.
    let checksum = match !udp_sum(&packet[ADDRESSES], &datagram) {
        0 => 0xFFFF,
        checksum => checksum,
    };
    datagram[6..8].copy_from_slice(&checksum.to_be_bytes());
    packet.extend(datagram);
    packet
}

fn udp_payload(packet: &[u8], (from, to): (u16, u16)) -> Option<&[u8]> {
    let (head, protocol) = parse_ipv4_head(packet)?;
    if protocol != UDP || sum(&[&packet[..head]]) != 0xFFFF {
        return None;
    }
    // A fragment of a larger packet is none of ours.
    if be16(packet, FRAGMENT_AT)? & FRAGMENT_BITS != 0 {
        return None;
    }
    let udp = packet.get(head..usize::from(be16(packet, LENGTH_AT)?))?;
    if be16(udp, 0)? != from || be16(udp, 2)? != to {
        return None;
    }
    let datagram = udp.get(..usize::from(be16(udp, 4)?)).filter(|datagram| datagram.len() >= UDP_HEAD)?;
    // A datagram sent without a checksum carries zero in its place.
    if be16(udp, 6)? != 0 && udp_sum(&packet[ADDRESSES], datagram) != 0xFFFF {
        return None;
    }
    Some(&datagram[UDP_HEAD..])
}

fn parse_options(mut options: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    core::iter::from_fn(move || {
        while options.first() == Some(&PAD) {
            options = &options[1..];
        }
        let (&kind, &length) = (options.first().filter(|kind| **kind != END)?, options.get(1)?);
        let body = options.get(2..2 + usize::from(length))?;
        options = &options[2 + usize::from(length)..];
        Some((kind, body))
    })
}

// A message's fixed part, of a kind, with its number and the station's hardware address, and the cookie its options follow.
fn to_fixed(kind: u8, id: [u8; 4], hardware: &[u8; HARDWARE_BYTES]) -> Vec<u8> {
    let mut message = alloc::vec![0u8; FIXED];
    (message[0], message[1], message[2]) = (kind, ETHERNET, HARDWARE_BYTES as u8);
    message[ID_AT..ID_AT + 4].copy_from_slice(&id);
    message[HARDWARE_AT..HARDWARE_AT + HARDWARE_BYTES].copy_from_slice(hardware);
    message.extend(COOKIE);
    message
}

fn add_option(message: &mut Vec<u8>, kind: u8, body: &[u8]) {
    message.extend([kind, body.len() as u8]);
    message.extend(body);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Selecting,
    Requesting { address: Address, server: Address },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Offer,
    Lend { is_rapid: bool },
    // What an asker that has its address by other means is told of the network.
    Tell,
    Refuse,
}

// A server that lends one host one address without end, and tells it the router, the resolver and the largest packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Server {
    pub(crate) address: Address,
    pub(crate) lent: Address,
    pub(crate) mask: Address,
    pub(crate) mtu: u16,
}

impl Server {
    // The reply to an IPv4 packet of the host's, a packet for every station of the link; none for what asks this server nothing.
    pub(crate) fn answer(&self, packet: &[u8]) -> Option<Vec<u8>> {
        let message = udp_payload(packet, TO_SERVER)?;
        let fixed = message.get(..FIXED)?;
        if fixed[..3] != [REQUEST, ETHERNET, HARDWARE_BYTES as u8] {
            return None;
        }
        let options = message.get(FIXED..)?.strip_prefix(&COOKIE)?;
        let find = |kind: u8| parse_options(options).find(|(option, _)| *option == kind).map(|(_, body)| body);
        let address = |kind: u8| find(kind).and_then(|body| body.first_chunk::<4>()).copied();
        let ciaddr: Address = *fixed[CIADDR_AT..].first_chunk()?;
        let answer = match *find(MESSAGE_TYPE)?.first()? {
            DISCOVER => {
                if find(RAPID_COMMIT).is_some() {
                    Answer::Lend { is_rapid: true }
                } else {
                    Answer::Offer
                }
            }
            // A request that takes another server's offer asks this one nothing.
            DHCP_REQUEST if address(SERVER).is_some_and(|server| server != self.address) => return None,
            DHCP_REQUEST if address(REQUESTED_ADDRESS).unwrap_or(ciaddr) == self.lent => {
                Answer::Lend { is_rapid: false }
            }
            DHCP_REQUEST => Answer::Refuse,
            INFORM => Answer::Tell,
            _ => return None,
        };
        let mut reply = to_fixed(REPLY, *fixed[ID_AT..].first_chunk()?, fixed[HARDWARE_AT..].first_chunk()?);
        reply[FLAGS_AT..FLAGS_AT + 2].copy_from_slice(&fixed[FLAGS_AT..FLAGS_AT + 2]);
        reply[GIADDR_AT..GIADDR_AT + 4].copy_from_slice(&fixed[GIADDR_AT..GIADDR_AT + 4]);
        let kind = match answer {
            Answer::Offer => OFFER,
            Answer::Lend { .. } | Answer::Tell => ACK,
            Answer::Refuse => NAK,
        };
        add_option(&mut reply, MESSAGE_TYPE, &[kind]);
        add_option(&mut reply, SERVER, &self.address);
        if matches!(answer, Answer::Offer | Answer::Lend { .. }) {
            reply[YIADDR_AT..YIADDR_AT + 4].copy_from_slice(&self.lent);
            add_option(&mut reply, LEASE_SECONDS, &FOREVER.to_be_bytes());
        }
        if answer != Answer::Refuse {
            reply[CIADDR_AT..CIADDR_AT + 4].copy_from_slice(&ciaddr);
            add_option(&mut reply, MASK, &self.mask);
            add_option(&mut reply, ROUTER, &self.address);
            add_option(&mut reply, RESOLVER, &self.address);
            add_option(&mut reply, INTERFACE_MTU, &self.mtu.to_be_bytes());
        }
        if answer == (Answer::Lend { is_rapid: true }) {
            add_option(&mut reply, RAPID_COMMIT, &[]);
        }
        reply.push(END);
        Some(udp_packet(self.address, BROADCAST, TO_STATION, &reply))
    }
}

/// The outcome of a reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    // The station sends its next message at once.
    Send,
    Bound(Lease),
}

/// What a station does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
    Wait(Duration),
    Send,
    // Gives up on a network that lends nothing.
    Silent,
}

/// A DHCP client that never asks for an old address; its times count from its first message.
pub struct Client {
    hardware: [u8; 6],
    id: u32,
    state: State,
    resend: Resend,
}

impl Client {
    /// A client that marks its messages with `id`.
    pub const fn new(hardware: [u8; 6], id: u32) -> Client {
        Client { hardware, id, state: State::Selecting, resend: Resend::new(Duration::ZERO) }
    }

    /// What it does `elapsed` after its first message went, with `timeout` of its time left, none once that is over.
    pub fn next(&mut self, elapsed: Duration, timeout: Option<Duration>) -> Next {
        if timeout.is_some() && self.take_resend(elapsed) {
            return Next::Send;
        }
        self.next_wait(elapsed, timeout).map_or(Next::Silent, Next::Wait)
    }

    fn next_wait(&self, elapsed: Duration, timeout: Option<Duration>) -> Option<Duration> {
        Some(timeout?.min(self.resend.due.saturating_sub(elapsed)))
    }

    fn take_resend(&mut self, now: Duration) -> bool {
        self.resend.take_due(now)
    }

    /// Takes an IPv4 packet from the interface at `now`; none if it is no reply to this station. A message the reply calls for goes at `now`.
    pub fn take_packet(&mut self, packet: &[u8], now: Duration) -> Option<Step> {
        let step = self.read_reply(packet);
        if step == Some(Step::Send) {
            self.resend = Resend::new(now);
        }
        step
    }

    /// The message to send now and until a reply is taken, as a broadcast IPv4 packet.
    pub fn message(&self) -> Vec<u8> {
        let mut message = to_fixed(REQUEST, self.id.to_be_bytes(), &self.hardware);
        match self.state {
            State::Selecting => {
                add_option(&mut message, MESSAGE_TYPE, &[DISCOVER]);
                add_option(&mut message, RAPID_COMMIT, &[]);
            }
            State::Requesting { address, server } => {
                add_option(&mut message, MESSAGE_TYPE, &[DHCP_REQUEST]);
                add_option(&mut message, REQUESTED_ADDRESS, &address);
                add_option(&mut message, SERVER, &server);
            }
        }
        add_option(&mut message, PARAMETER_LIST, &[MASK, ROUTER, LEASE_SECONDS, RENEW_SECONDS]);
        message.push(END);
        udp_packet(NO_ADDRESS, BROADCAST, TO_SERVER, &message)
    }

    fn read_reply(&mut self, packet: &[u8]) -> Option<Step> {
        let message = udp_payload(packet, TO_STATION)?;
        let fixed = message.get(..FIXED)?;
        if fixed[0] != REPLY
            || fixed[ID_AT..ID_AT + 4] != self.id.to_be_bytes()
            || fixed[HARDWARE_AT..HARDWARE_AT + self.hardware.len()] != self.hardware
        {
            return None;
        }
        let options = message.get(FIXED..)?.strip_prefix(&COOKIE)?;
        let find = |kind: u8| parse_options(options).find(|(option, _)| *option == kind).map(|(_, body)| body);
        let address = |kind: u8| find(kind).and_then(|body| body.first_chunk::<4>()).copied();
        let seconds = |kind: u8| address(kind).map(u32::from_be_bytes);
        let yiaddr: Address = fixed[YIADDR_AT..YIADDR_AT + 4].try_into().ok()?;
        let kind = *find(MESSAGE_TYPE)?.first()?;
        let lease = || {
            let lease_seconds = seconds(LEASE_SECONDS)?;
            Some(Lease {
                address: yiaddr,
                mask: address(MASK)?,
                router: address(ROUTER),
                server: address(SERVER)?,
                seconds: lease_seconds,
                // A time to renew past the lease's end is none to keep it by.
                renew_seconds: seconds(RENEW_SECONDS)
                    .map_or(lease_seconds / RENEW_SHARE, |renew| renew.min(lease_seconds)),
            })
        };
        match (self.state, kind) {
            // A server that makes the short exchange lends the address at once.
            (State::Selecting, ACK) if find(RAPID_COMMIT).is_some() => lease().map(Step::Bound),
            (State::Selecting, OFFER) => {
                self.state = State::Requesting { address: yiaddr, server: address(SERVER)? };
                Some(Step::Send)
            }
            (State::Requesting { address: requested, server }, ACK)
                if yiaddr == requested && address(SERVER) == Some(server) =>
            {
                lease().map(Step::Bound)
            }
            // A server that refuses what was asked for sends the station back to asking who lends one.
            (State::Requesting { .. }, NAK) => {
                self.state = State::Selecting;
                Some(Step::Send)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::IP_HEAD;
    use alloc::vec;

    fn time_left(elapsed: Duration) -> Option<Duration> {
        Duration::from_secs(10).checked_sub(elapsed)
    }

    const HARDWARE: [u8; 6] = [2, 0, 0, 0, 0, 1];
    const SERVER_ADDRESS: Address = [192, 168, 1, 1];
    const LENT: Address = [192, 168, 1, 57];

    fn reply(id: u32, hardware: [u8; 6], kind: u8, yiaddr: Address, options: &[u8]) -> Vec<u8> {
        let mut message = vec![0u8; FIXED];
        (message[0], message[1], message[2]) = (REPLY, ETHERNET, 6);
        message[ID_AT..ID_AT + 4].copy_from_slice(&id.to_be_bytes());
        message[YIADDR_AT..YIADDR_AT + 4].copy_from_slice(&yiaddr);
        message[HARDWARE_AT..HARDWARE_AT + 6].copy_from_slice(&hardware);
        message.extend(COOKIE);
        message.extend([MESSAGE_TYPE, 1, kind, SERVER, 4]);
        message.extend(SERVER_ADDRESS);
        message.extend(options);
        message.push(END);
        udp_packet(SERVER_ADDRESS, yiaddr, TO_STATION, &message)
    }

    fn terms(renew: Option<u32>) -> Vec<u8> {
        let mut terms = vec![MASK, 4, 255, 255, 255, 0, ROUTER, 4, 192, 168, 1, 1, LEASE_SECONDS, 4, 0, 0, 0x0E, 0x10];
        if let Some(renew) = renew {
            terms.extend([PAD, PAD, RENEW_SECONDS, 4]);
            terms.extend(renew.to_be_bytes());
        }
        terms
    }

    const LEASE: Lease = Lease {
        address: LENT,
        mask: [255, 255, 255, 0],
        router: Some(SERVER_ADDRESS),
        server: SERVER_ADDRESS,
        seconds: 3600,
        renew_seconds: 900,
    };

    fn to_options(packet: &[u8]) -> Vec<(u8, Vec<u8>)> {
        parse_options(&packet[IP_HEAD + UDP_HEAD + FIXED + 4..]).map(|(kind, body)| (kind, body.to_vec())).collect()
    }

    #[test]
    fn datagram_checksums() {
        let packet = udp_packet([10, 0, 0, 2], [10, 0, 0, 1], TO_SERVER, b"hello");
        assert_eq!(
            (sum(&[&packet[..IP_HEAD]]), packet.len(), &packet[IP_HEAD..IP_HEAD + 6]),
            (0xFFFF, 33, &[0, 68, 0, 67, 0, 13][..]),
            "a head that checks, and a datagram from the station's port to the server's"
        );
        assert_eq!(
            udp_sum(&packet[12..20], &packet[IP_HEAD..]),
            0xFFFF,
            "and a datagram that checks with the addresses it goes between"
        );
    }

    #[test]
    fn discover_request_ack() {
        let mut station = Client::new(HARDWARE, 0x1234_5678);
        let discover = station.message();
        assert_eq!(
            (&discover[12..20], discover[IP_HEAD + UDP_HEAD], &discover[IP_HEAD + UDP_HEAD + ID_AT..][..4]),
            (&[0, 0, 0, 0, 255, 255, 255, 255][..], REQUEST, &[0x12, 0x34, 0x56, 0x78][..]),
            "from no address to every station, with the number its replies carry"
        );
        assert_eq!(
            &discover[IP_HEAD + UDP_HEAD + HARDWARE_AT..][..6],
            &HARDWARE,
            "and the hardware address the reply is sent to"
        );
        assert_eq!(
            to_options(&discover),
            [
                (MESSAGE_TYPE, vec![DISCOVER]),
                (RAPID_COMMIT, vec![]),
                (PARAMETER_LIST, vec![MASK, ROUTER, LEASE_SECONDS, RENEW_SECONDS])
            ]
        );
        assert_eq!(station.message(), discover, "sent again, a message is the same");
        assert_eq!(station.read_reply(&reply(0x1234_5678, HARDWARE, OFFER, LENT, &terms(Some(900)))), Some(Step::Send));
        assert_eq!(
            to_options(&station.message())[..3],
            [(MESSAGE_TYPE, vec![DHCP_REQUEST]), (REQUESTED_ADDRESS, LENT.to_vec()), (SERVER, SERVER_ADDRESS.to_vec())],
            "it asks the server that offered for the address offered"
        );
        assert_eq!(
            station.read_reply(&reply(0x1234_5678, HARDWARE, ACK, LENT, &terms(Some(900)))),
            Some(Step::Bound(LEASE))
        );
        assert_eq!(LEASE.prefix(), 24);
        let mut plain = Client::new(HARDWARE, 7);
        plain.read_reply(&reply(7, HARDWARE, OFFER, LENT, &terms(None)));
        assert_eq!(
            plain.read_reply(&reply(7, HARDWARE, ACK, LENT, &terms(None))),
            Some(Step::Bound(Lease { renew_seconds: 1800, ..LEASE })),
            "a server that names no time to renew means half the lease"
        );
        let mut late = Client::new(HARDWARE, 8);
        late.read_reply(&reply(8, HARDWARE, OFFER, LENT, &terms(Some(0xFFFF))));
        assert_eq!(
            late.read_reply(&reply(8, HARDWARE, ACK, LENT, &terms(Some(0xFFFF)))),
            Some(Step::Bound(Lease { renew_seconds: 3600, ..LEASE })),
            "and one that names a time past the lease's end, the end"
        );
    }

    #[test]
    fn current_leases() {
        let lease = Lease { renew_seconds: 1800, ..LEASE };
        let (second, access, other) = (Duration::from_secs(1), [2, 0, 0, 0, 0, 9], [2, 0, 0, 0, 0, 8]);
        let mut leases = Leases::default();
        assert_eq!(leases.find_current(&access, second), None, "none is held");
        leases.keep(access, lease, 100 * second);
        assert_eq!(
            [
                leases.find_current(&access, 100 * second),
                leases.find_current(&access, 1899 * second),
                leases.find_current(&other, 100 * second)
            ],
            [Some(lease), Some(lease), None],
            "until its renewal is due, and only where it was given"
        );
        leases.keep(access, lease, 1000 * second);
        assert_eq!(
            leases.find_current(&access, 1899 * second),
            Some(lease),
            "bound again with nothing asked, it keeps the time it was given at"
        );
        assert_eq!(leases.find_current(&access, 1900 * second), None, "then it is asked for anew");
        leases.keep(access, lease, 1900 * second);
        assert_eq!(
            leases.find_current(&access, 3699 * second),
            Some(lease),
            "and the same terms lent anew count from then"
        );
        leases.keep(access, Lease { renew_seconds: 60, ..lease }, 1900 * second);
        assert_eq!(
            leases.find_current(&access, 1959 * second).map(|lease| lease.renew_seconds),
            Some(60),
            "the new one counts"
        );
    }

    #[test]
    fn resend_backoff() {
        let second = Duration::from_secs(1);
        let mut resend = Resend::new(second);
        assert_eq!(resend.due, 5 * second);
        assert!(!resend.take_due(4 * second), "not before its time");
        assert!(resend.take_due(5 * second));
        assert_eq!(resend.due, 13 * second, "then after eight");
        assert!(!resend.take_due(12 * second) && resend.take_due(14 * second));
        assert_eq!(resend.due, 30 * second, "counted from when it went, after sixteen");
    }

    #[test]
    fn resend_schedule() {
        let second = Duration::from_secs(1);
        let mut station = Client::new(HARDWARE, 9);
        assert_eq!(
            station.next_wait(Duration::ZERO, time_left(Duration::ZERO)),
            Some(4 * second),
            "counted from when the first message went"
        );
        assert_eq!(station.next_wait(3 * second, time_left(3 * second)), Some(second));
        assert!(!station.take_resend(3 * second) && station.take_resend(4 * second));
        assert_eq!(
            station.next_wait(4 * second, time_left(4 * second)),
            Some(6 * second),
            "the asking ends before the message would go again after eight seconds"
        );
        assert_eq!(station.take_packet(&reply(8, HARDWARE, OFFER, LENT, &terms(Some(900))), 5 * second), None);
        assert_eq!(
            station.next_wait(5 * second, time_left(5 * second)),
            Some(5 * second),
            "what is no reply changes nothing"
        );
        assert_eq!(
            station.take_packet(&reply(9, HARDWARE, OFFER, LENT, &terms(Some(900))), 5 * second),
            Some(Step::Send)
        );
        assert_eq!(
            station.next_wait(5 * second, time_left(5 * second)),
            Some(4 * second),
            "the request an offer brings goes again four seconds after it went"
        );
    }

    #[test]
    fn silent_network() {
        let (second, milli) = (Duration::from_secs(1), Duration::from_millis(1));
        let mut station = Client::new(HARDWARE, 9);
        assert!(station.take_resend(4 * second));
        assert_eq!(station.next_wait(9 * second, Some(milli)), Some(milli), "waited for up to the limit");
        assert_eq!(station.next_wait(10 * second, None), None, "and silent from it on, whatever is still to go again");
    }

    #[test]
    fn next_silent_at_limit() {
        let second = Duration::from_secs(1);
        let mut station = Client::new(HARDWARE, 9);
        assert_eq!(station.next(Duration::ZERO, time_left(Duration::ZERO)), Next::Wait(4 * second));
        assert_eq!(station.next(4 * second, time_left(4 * second)), Next::Send);
        assert_eq!(station.next(4 * second, time_left(4 * second)), Next::Wait(6 * second));
        assert_eq!(station.next(10 * second, None), Next::Silent);
        assert_eq!(station.next(12 * second, None), Next::Silent, "past the limit nothing goes again, whatever is due");
        let mut backed = Client::new(HARDWARE, 9);
        assert_eq!(
            backed.next(FIRST_WAIT, None),
            Next::Silent,
            "a limit that ends as the message would go again sends none"
        );
    }

    #[test]
    fn lease_text() {
        assert_eq!(LEASE.to_log_line(), "192.168.1.57/24, router 192.168.1.1, lent for 3600 s by 192.168.1.1");
        assert_eq!(
            Lease { router: None, ..LEASE }.to_log_line(),
            "192.168.1.57/24, no router, lent for 3600 s by 192.168.1.1"
        );
        assert_eq!(
            [LEASE.to_bound_line(350, false), LEASE.to_bound_line(2, true)],
            [
                "wifi: address 192.168.1.57/24, router 192.168.1.1, lent for 3600 s by 192.168.1.1 after 350 ms",
                "wifi: address 192.168.1.57/24, router 192.168.1.1, lent for 3600 s by 192.168.1.1 after 2 ms, kept from the stay before"
            ]
        );
    }

    #[test]
    fn nak_and_rapid_commit() {
        let mut refused = Client::new(HARDWARE, 9);
        refused.read_reply(&reply(9, HARDWARE, OFFER, LENT, &terms(Some(900))));
        assert_eq!(
            refused.read_reply(&reply(9, HARDWARE, ACK, [192, 168, 1, 58], &terms(Some(900)))),
            None,
            "an acknowledgement of another address than the one asked for is none"
        );
        assert_eq!(refused.read_reply(&reply(9, HARDWARE, NAK, [0; 4], &[])), Some(Step::Send));
        assert_eq!(to_options(&refused.message())[0], (MESSAGE_TYPE, vec![DISCOVER]), "refused, it asks who lends one");
        let mut quick = Client::new(HARDWARE, 9);
        assert_eq!(
            quick.read_reply(&reply(9, HARDWARE, ACK, LENT, &[&terms(Some(900))[..], &[RAPID_COMMIT, 0]].concat())),
            Some(Step::Bound(LEASE)),
            "a server that makes the short exchange lends at once"
        );
        assert_eq!(
            quick.read_reply(&reply(9, HARDWARE, ACK, LENT, &terms(Some(900)))),
            None,
            "an acknowledgement that does not say so answers nothing that was asked"
        );
    }

    const SERVER_TERMS: Server = Server { address: SERVER_ADDRESS, lent: LENT, mask: [255, 255, 255, 252], mtu: 1384 };

    fn request(kind: u8, ciaddr: Address, options: &[u8]) -> Vec<u8> {
        let mut message = to_fixed(REQUEST, [0, 0, 0, 9], &HARDWARE);
        message[FLAGS_AT] = 0x80;
        message[CIADDR_AT..CIADDR_AT + 4].copy_from_slice(&ciaddr);
        add_option(&mut message, MESSAGE_TYPE, &[kind]);
        message.extend(options);
        message.push(END);
        udp_packet(ciaddr, BROADCAST, TO_SERVER, &message)
    }

    fn answered(packet: &[u8]) -> (u8, Address, Vec<(u8, Vec<u8>)>) {
        let reply = SERVER_TERMS.answer(packet).expect("an answer");
        assert_eq!(
            (&reply[12..20], &reply[IP_HEAD..IP_HEAD + 4]),
            (&[192, 168, 1, 1, 255, 255, 255, 255][..], &[0, 67, 0, 68][..]),
            "from the server to every station, from its port to the station's"
        );
        let message = &reply[IP_HEAD + UDP_HEAD..];
        assert_eq!(
            (message[0], &message[ID_AT..ID_AT + 4], message[FLAGS_AT], &message[HARDWARE_AT..HARDWARE_AT + 6]),
            (REPLY, &[0, 0, 0, 9][..], 0x80, &HARDWARE[..]),
            "a reply under the question's number, with its flags and the asker's hardware address"
        );
        let options = to_options(&reply);
        (options[0].1[0], message[YIADDR_AT..YIADDR_AT + 4].try_into().unwrap(), options[1..].to_vec())
    }

    #[test]
    fn server_lends_its_one_address() {
        let network = vec![
            (SERVER, SERVER_ADDRESS.to_vec()),
            (LEASE_SECONDS, vec![255; 4]),
            (MASK, vec![255, 255, 255, 252]),
            (ROUTER, SERVER_ADDRESS.to_vec()),
            (RESOLVER, SERVER_ADDRESS.to_vec()),
            (INTERFACE_MTU, vec![0x05, 0x68]),
        ];
        assert_eq!(
            answered(&request(DISCOVER, NO_ADDRESS, &[])),
            (OFFER, LENT, network.clone()),
            "an offer of the address, lent without end, with the mask, the router, the resolver and the largest packet"
        );
        let wanted = [&[REQUESTED_ADDRESS, 4][..], &LENT, &[SERVER, 4], &SERVER_ADDRESS].concat();
        assert_eq!(answered(&request(DHCP_REQUEST, NO_ADDRESS, &wanted)), (ACK, LENT, network.clone()));
        assert_eq!(
            answered(&request(DHCP_REQUEST, NO_ADDRESS, &wanted[..6])).0,
            ACK,
            "a station that asks for the address it had, naming no server"
        );
        assert_eq!(answered(&request(DHCP_REQUEST, LENT, &[])).0, ACK, "and one that renews it");
        let mut rapid = network.clone();
        rapid.push((RAPID_COMMIT, vec![]));
        assert_eq!(answered(&request(DISCOVER, NO_ADDRESS, &[RAPID_COMMIT, 0])), (ACK, LENT, rapid));
        let other = [&[REQUESTED_ADDRESS, 4][..], &[192, 168, 1, 58]].concat();
        assert_eq!(
            answered(&request(DHCP_REQUEST, NO_ADDRESS, &other)),
            (NAK, NO_ADDRESS, vec![(SERVER, SERVER_ADDRESS.to_vec())]),
            "another address than the one it lends is refused"
        );
        assert_eq!(
            answered(&request(INFORM, [192, 168, 1, 57], &[])),
            (ACK, NO_ADDRESS, [&network[..1], &network[2..]].concat()),
            "a station that has its address is told of the network, and lent nothing"
        );
    }

    #[test]
    fn server_asked_nothing() {
        let elsewhere = [&[REQUESTED_ADDRESS, 4][..], &LENT, &[SERVER, 4], &[192, 168, 1, 9]].concat();
        assert_eq!(
            SERVER_TERMS.answer(&request(DHCP_REQUEST, NO_ADDRESS, &elsewhere)),
            None,
            "a request that takes another server's offer"
        );
        assert_eq!(SERVER_TERMS.answer(&request(7, LENT, &[])), None, "an address given back");
        assert_eq!(
            SERVER_TERMS.answer(&reply(9, HARDWARE, OFFER, LENT, &terms(None))),
            None,
            "a server's own kind of message"
        );
        let whole = request(DISCOVER, NO_ADDRESS, &[]);
        assert!((0..whole.len()).all(|length| SERVER_TERMS.answer(&whole[..length]).is_none()), "a packet cut short");
    }

    #[test]
    fn client_and_server_agree() {
        let mut station = Client::new(HARDWARE, 9);
        let lent = SERVER_TERMS.answer(&station.message()).and_then(|reply| station.read_reply(&reply));
        let lease = Lease {
            address: LENT,
            mask: [255, 255, 255, 252],
            router: Some(SERVER_ADDRESS),
            server: SERVER_ADDRESS,
            seconds: u32::MAX,
            renew_seconds: u32::MAX / 2,
        };
        assert_eq!(lent, Some(Step::Bound(lease)), "the short exchange, which the firmware's own station asks for");
        let mut plain = Client::new(HARDWARE, 9);
        plain.read_reply(&SERVER_TERMS.answer(&request(DISCOVER, NO_ADDRESS, &[])).unwrap());
        assert_eq!(
            SERVER_TERMS.answer(&plain.message()).and_then(|reply| plain.read_reply(&reply)),
            Some(Step::Bound(lease)),
            "and the long one"
        );
    }

    #[test]
    fn foreign_packets() {
        let mut station = Client::new(HARDWARE, 9);
        let offer = reply(9, HARDWARE, OFFER, LENT, &terms(Some(900)));
        assert_eq!(station.read_reply(&reply(8, HARDWARE, OFFER, LENT, &terms(Some(900)))), None, "another number");
        assert_eq!(
            station.read_reply(&reply(9, [2, 0, 0, 0, 0, 2], OFFER, LENT, &terms(Some(900)))),
            None,
            "another station's hardware address"
        );
        let mut corrupt = offer.clone();
        corrupt[100] ^= 1;
        assert_eq!(station.read_reply(&corrupt), None, "a packet that does not check");
        let mut unchecked = corrupt.clone();
        unchecked[26..28].copy_from_slice(&[0, 0]);
        assert_eq!(
            Client::new(HARDWARE, 9).read_reply(&unchecked),
            Some(Step::Send),
            "a datagram sent without a checksum is taken as it is"
        );
        let mut pieces = offer.clone();
        pieces[6] |= 0x20;
        assert_eq!(station.read_reply(&pieces), None, "nor a piece of a larger packet");
        for length in 0..offer.len() {
            assert_eq!(Client::new(HARDWARE, 9).read_reply(&offer[..length]), None, "{length} bytes");
        }
        for length in 0..UDP_HEAD as u8 {
            let mut short = offer.clone();
            short[IP_HEAD + 4..IP_HEAD + 6].copy_from_slice(&[0, length]);
            assert_eq!(
                station.read_reply(&short),
                None,
                "a datagram that says it is {length} bytes, less than its own head"
            );
        }
        let mut other_port = offer.clone();
        other_port[22..24].copy_from_slice(&[0, 69]);
        assert_eq!(station.read_reply(&other_port), None);
        let mut request = station.message();
        request[20..24].copy_from_slice(&[0, 67, 0, 68]);
        assert_eq!(station.read_reply(&request), None, "a request is no reply");
        assert_eq!(
            Client::new(HARDWARE, 9).read_reply(&reply(9, HARDWARE, ACK, LENT, &terms(Some(900)))),
            None,
            "an acknowledgement of nothing asked"
        );
        assert_eq!(
            Client::new(HARDWARE, 9).read_reply(&reply(9, HARDWARE, OFFER, LENT, &terms(Some(900)))),
            Some(Step::Send)
        );
    }
}
