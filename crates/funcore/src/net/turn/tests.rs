use super::*;
use crate::base::bytes::be16;
use crate::net::stun::tests::Exact;
use crate::net::stun::{
    ALLOCATE, ALLOCATION_MISMATCH, Attribute, BAD_REQUEST, CHANNEL_BIND, CREATE_PERMISSION, Class, Crypto, DATA_METHOD,
    DONT_FRAGMENT, FAMILY_UNSUPPORTED, FORBIDDEN, Frames, Framing, INSUFFICIENT_CAPACITY, IPV4, IPV6, Integrity,
    Message, PEER_FAMILY_MISMATCH, QUOTA_REACHED, REFRESH, REQUESTED_ADDRESS_FAMILY, REQUESTED_TRANSPORT, SEND,
    SERVER_ERROR, STALE_NONCE, STREAM_WAIT, TRANSPORT_UNSUPPORTED, TRY_ALTERNATE, Transport, UDP, UNAUTHENTICATED,
    UNKNOWN_ATTRIBUTE, WRONG_CREDENTIALS, Writer, family_of,
};
use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::net::{IpAddr, SocketAddr};

const REALM: &[u8] = b"relay.example.org";
const USER: &str = "1800000000:caller";
const PASSWORD: &str = "c2VjcmV0";
const PERMISSION_LIFE: Duration = Duration::from_secs(300);
const CHANNEL_LIFE: Duration = Duration::from_secs(600);
const COOKIE: &str = "obMatJos2gAAA";

// What the relay sends: a message to a client's transport address, or a datagram from a relayed address to a peer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Out {
    Client(SocketAddr, Vec<u8>),
    Peer { from: SocketAddr, to: SocketAddr, data: Vec<u8> },
}

struct Allocation {
    client: SocketAddr,
    is_stream: bool,
    transaction: [u8; 12],
    relayed: SocketAddr,
    expires: Duration,
    permissions: Vec<(IpAddr, Duration)>,
    channels: Vec<(u16, SocketAddr, Duration)>,
}

// A relay as RFC 8656 has a server be, for the tests: a long-term credential with a nonce it can let go stale, allocations by the client's transport address, permissions, channels, and the data it relays; and the refusals a test asks of it.
pub(crate) struct Relay {
    pub(crate) address: SocketAddr,
    ipv4: IpAddr,
    ipv6: IpAddr,
    nonce: u32,
    // Whether its nonce carries the cookie that asks for password algorithms, of which it lists SHA-256 and MD5.
    has_algorithms: bool,
    knows_fragment: bool,
    // Whether it knows REQUESTED-ADDRESS-FAMILY, which a server without RFC 6156 does not.
    knows_family: bool,
    // Whether its error responses to a signed request carry MESSAGE-INTEGRITY.
    seals_errors: bool,
    offers_ipv6: bool,
    alternate: Option<SocketAddr>,
    lifetime: u32,
    // Error codes the next signed requests of a method are answered with, in turn.
    refusals: Vec<(u16, u16)>,
    allocations: Vec<Allocation>,
    next_port: u16,
    crypto: Exact,
}

impl Relay {
    pub(crate) fn new(address: SocketAddr, ipv4: IpAddr, ipv6: IpAddr) -> Relay {
        Relay {
            address,
            ipv4,
            ipv6,
            nonce: 1,
            has_algorithms: false,
            knows_fragment: true,
            knows_family: true,
            seals_errors: true,
            offers_ipv6: true,
            alternate: None,
            lifetime: 600,
            refusals: Vec::new(),
            allocations: Vec::new(),
            next_port: 50_000,
            crypto: Exact(Cell::new(0x5EED)),
        }
    }

    // The credential a client of this relay is handed.
    pub(crate) fn server(&self, transport: Transport) -> Server {
        Server {
            address: self.address,
            transport,
            name: (transport == Transport::Tls).then(|| "relay.example.org".to_string()),
            username: USER.to_string(),
            password: PASSWORD.to_string(),
            expires: Duration::from_secs(86_400),
        }
    }

    // Lets the nonce go stale: the next signed request is answered 438.
    fn renew_nonce(&mut self) {
        self.nonce += 1;
    }

    fn nonce(&self) -> Vec<u8> {
        if self.has_algorithms { format!("{COOKIE}{}", self.nonce) } else { format!("nonce{}", self.nonce) }
            .into_bytes()
    }

    fn key(&self, sha256: bool) -> Vec<u8> {
        let parts: [&[u8]; 5] = [USER.as_bytes(), b":", REALM, b":", PASSWORD.as_bytes()];
        if sha256 { self.crypto.sha256(&parts).to_vec() } else { self.crypto.md5(&parts).to_vec() }
    }

    fn relayed_of(&self, client: SocketAddr) -> Option<SocketAddr> {
        self.allocations.iter().find(|allocation| allocation.client == client).map(|allocation| allocation.relayed)
    }

    pub(crate) fn allocation_count(&self) -> usize {
        self.allocations.len()
    }

    // True for an address the relay relays at.
    pub(crate) fn is_relayed(&self, address: SocketAddr) -> bool {
        self.allocations.iter().any(|allocation| allocation.relayed == address)
    }

    fn reply(&self, message: &Message<'_>, class: Class, attributes: &[Attribute<'_>], key: Option<&[u8]>) -> Vec<u8> {
        let mut writer = Writer::new(message.method, class, &message.transaction);
        for attribute in attributes {
            writer.push(*attribute);
        }
        if let Some(key) = key.filter(|_| class != Class::Error || self.seals_errors) {
            writer.seal(&self.crypto, if self.has_algorithms { Integrity::Sha256(key) } else { Integrity::Sha1(key) });
        }
        writer.finish(true)
    }

    fn challenge(&self, message: &Message<'_>, code: u16) -> Vec<u8> {
        let nonce = self.nonce();
        let mut attributes = vec![Attribute::Error(code), Attribute::Realm(REALM), Attribute::Nonce(&nonce)];
        let algorithms = [0, 2, 0, 0, 0, 1, 0, 0];
        if self.has_algorithms {
            attributes.push(Attribute::Algorithms(&algorithms));
        }
        self.reply(message, Class::Error, &attributes, None)
    }

    // Takes a message from a client's transport address, over a stream or in a datagram.
    pub(crate) fn take(&mut self, client: SocketAddr, is_stream: bool, bytes: &[u8], now: Duration) -> Vec<Out> {
        self.allocations.retain(|allocation| allocation.expires > now);
        if bytes.first().is_some_and(|first| (0x40..=0x4F).contains(first)) {
            let (Some(number), Some(length)) = (be16(bytes, 0), be16(bytes, 2)) else { return vec![] };
            let Some(allocation) = self.allocations.iter().find(|allocation| allocation.client == client) else {
                return vec![];
            };
            let channel = allocation.channels.iter().find(|(bound, _, until)| *bound == number && *until > now);
            let data = bytes.get(4..4 + usize::from(length));
            return match (channel, data) {
                (Some((_, peer, _)), Some(data)) => {
                    vec![Out::Peer { from: allocation.relayed, to: *peer, data: data.to_vec() }]
                }
                _ => vec![],
            };
        }
        let Some(message) = Message::parse(bytes) else { return vec![] };
        // A relay answers Binding as a STUN server does (RFC 8656, 3).
        if message.method == crate::net::stun::BINDING && message.class == Class::Request {
            return vec![Out::Client(
                client,
                self.reply(&message, Class::Success, &[Attribute::XorMapped(client)], None),
            )];
        }
        if message.class == Class::Indication {
            if message.method == SEND
                && let Some(allocation) = self.allocations.iter().find(|allocation| allocation.client == client)
                && let (Some(peer), Some(data)) = (message.peers.first(), message.data)
                && allocation.permissions.iter().any(|(ip, until)| *ip == peer.ip() && *until > now)
            {
                return vec![Out::Peer { from: allocation.relayed, to: *peer, data: data.to_vec() }];
            }
            return vec![];
        }
        if message.class != Class::Request {
            return vec![];
        }
        let out = |bytes: Vec<u8>| vec![Out::Client(client, bytes)];
        let sha256 = self.has_algorithms;
        let key = self.key(sha256);
        let integrity = if sha256 { Integrity::Sha256(&key) } else { Integrity::Sha1(&key) };
        let username = message.username.or(message.find(crate::net::stun::USERHASH));
        if username.is_none() || !message.verify(&self.crypto, integrity) {
            return out(self.challenge(&message, UNAUTHENTICATED));
        }
        if sha256 && message.find(crate::net::stun::PASSWORD_ALGORITHM) != Some(&[0, 2, 0, 0][..]) {
            return out(self.reply(&message, Class::Error, &[Attribute::Error(BAD_REQUEST)], None));
        }
        if message.nonce != Some(&self.nonce()[..]) {
            return out(self.challenge(&message, STALE_NONCE));
        }
        if let Some(at) = self.refusals.iter().position(|(method, _)| *method == message.method) {
            let (_, code) = self.refusals.remove(at);
            let unknown = [DONT_FRAGMENT];
            let alternate = self.alternate.unwrap_or(self.address);
            let mut attributes = vec![Attribute::Error(code)];
            match code {
                UNKNOWN_ATTRIBUTE => attributes.push(Attribute::Unknown(&unknown)),
                TRY_ALTERNATE => {
                    attributes.push(Attribute::Alternate(alternate));
                    attributes.push(Attribute::Raw(crate::net::stun::ALTERNATE_DOMAIN, b"other.example.org"));
                }
                _ => {}
            }
            return out(self.reply(&message, Class::Error, &attributes, Some(&key)));
        }
        let fail =
            |relay: &Relay, code: u16| out(relay.reply(&message, Class::Error, &[Attribute::Error(code)], Some(&key)));
        match message.method {
            ALLOCATE => {
                if let Some(allocation) = self.allocations.iter().find(|allocation| allocation.client == client)
                    && allocation.transaction != message.transaction
                {
                    return fail(self, ALLOCATION_MISMATCH);
                }
                if message.find(REQUESTED_TRANSPORT).and_then(|value| value.first().copied()) != Some(UDP) {
                    return fail(self, TRANSPORT_UNSUPPORTED);
                }
                let unknown: Vec<u16> =
                    [(DONT_FRAGMENT, self.knows_fragment), (REQUESTED_ADDRESS_FAMILY, self.knows_family)]
                        .into_iter()
                        .filter(|(kind, is_known)| !is_known && message.find(*kind).is_some())
                        .map(|(kind, _)| kind)
                        .collect();
                if !unknown.is_empty() {
                    let attributes = [Attribute::Error(UNKNOWN_ATTRIBUTE), Attribute::Unknown(&unknown)];
                    return out(self.reply(&message, Class::Error, &attributes, Some(&key)));
                }
                let family =
                    message.find(REQUESTED_ADDRESS_FAMILY).and_then(|value| value.first().copied()).unwrap_or(IPV4);
                let ip = match family {
                    IPV4 => self.ipv4,
                    IPV6 if self.offers_ipv6 => self.ipv6,
                    _ => return fail(self, FAMILY_UNSUPPORTED),
                };
                let relayed = match self.allocations.iter().find(|allocation| allocation.client == client) {
                    Some(allocation) => allocation.relayed,
                    None => {
                        self.next_port += 1;
                        let relayed = SocketAddr::new(ip, self.next_port);
                        self.allocations.push(Allocation {
                            client,
                            is_stream,
                            transaction: message.transaction,
                            relayed,
                            expires: now + Duration::from_secs(u64::from(self.lifetime)),
                            permissions: Vec::new(),
                            channels: Vec::new(),
                        });
                        relayed
                    }
                };
                let attributes =
                    [Attribute::Relayed(relayed), Attribute::XorMapped(client), Attribute::Lifetime(self.lifetime)];
                out(self.reply(&message, Class::Success, &attributes, Some(&key)))
            }
            REFRESH => {
                let lifetime = message.lifetime.unwrap_or(600);
                let Some(at) = self.allocations.iter().position(|allocation| allocation.client == client) else {
                    return fail(self, ALLOCATION_MISMATCH);
                };
                if lifetime == 0 {
                    self.allocations.remove(at);
                } else {
                    self.allocations[at].expires = now + Duration::from_secs(u64::from(lifetime));
                }
                out(self.reply(&message, Class::Success, &[Attribute::Lifetime(lifetime)], Some(&key)))
            }
            CREATE_PERMISSION | CHANNEL_BIND => {
                let Some(at) = self.allocations.iter().position(|allocation| allocation.client == client) else {
                    return fail(self, ALLOCATION_MISMATCH);
                };
                let family = family_of(self.allocations[at].relayed.ip());
                if message.peers.is_empty() {
                    return fail(self, BAD_REQUEST);
                }
                if message.peers.iter().any(|peer| family_of(peer.ip()) != family) {
                    return fail(self, PEER_FAMILY_MISMATCH);
                }
                let allocation = &mut self.allocations[at];
                if message.method == CHANNEL_BIND {
                    let (Some(number), peer) = (message.channel, message.peers[0]) else {
                        return fail(self, BAD_REQUEST);
                    };
                    let taken = allocation
                        .channels
                        .iter()
                        .any(|(bound, address, until)| *until > now && ((*bound == number) != (*address == peer)));
                    if !(0x4000..=0x4FFF).contains(&number) || taken {
                        return fail(self, BAD_REQUEST);
                    }
                    allocation.channels.retain(|(bound, _, _)| *bound != number);
                    allocation.channels.push((number, peer, now + CHANNEL_LIFE));
                }
                for peer in &message.peers {
                    allocation.permissions.retain(|(ip, _)| *ip != peer.ip());
                    allocation.permissions.push((peer.ip(), now + PERMISSION_LIFE));
                }
                out(self.reply(&message, Class::Success, &[], Some(&key)))
            }
            _ => fail(self, BAD_REQUEST),
        }
    }

    // Takes a datagram a peer sent to a relayed address: to the client on its channel or in a Data indication, where a permission stands.
    pub(crate) fn take_peer(&mut self, relayed: SocketAddr, from: SocketAddr, data: &[u8], now: Duration) -> Vec<Out> {
        let Some(allocation) =
            self.allocations.iter().find(|allocation| allocation.relayed == relayed && allocation.expires > now)
        else {
            return vec![];
        };
        if !allocation.permissions.iter().any(|(ip, until)| *ip == from.ip() && *until > now) {
            return vec![];
        }
        if let Some((number, ..)) = allocation.channels.iter().find(|(_, peer, until)| *peer == from && *until > now) {
            let mut bytes = number.to_be_bytes().to_vec();
            bytes.extend((data.len() as u16).to_be_bytes());
            bytes.extend(data);
            if allocation.is_stream {
                bytes.resize(bytes.len().next_multiple_of(4), 0);
            }
            return vec![Out::Client(allocation.client, bytes)];
        }
        let transaction = [6u8; 12];
        let mut writer = Writer::new(DATA_METHOD, Class::Indication, &transaction);
        writer.push(Attribute::Peer(from)).push(Attribute::Data(data));
        vec![Out::Client(allocation.client, writer.finish(true))]
    }
}

// A client and the relay joined: by datagrams, or over a stream whose bytes are cut into pieces of `cut`.
struct Link {
    crypto: Exact,
    client: Client,
    relay: Relay,
    seen: SocketAddr,
    stream: Option<(Frames, Frames)>,
    cut: usize,
    events: Vec<Event>,
    sent: usize,
    to_peers: Vec<(SocketAddr, SocketAddr, Vec<u8>)>,
}

const SEEN: &str = "198.51.100.20:40000";
const RELAY: &str = "192.0.2.15:3478";
const PEER: &str = "203.0.113.50:6000";

fn link(transport: Transport, cut: usize) -> Link {
    let relay = Relay::new(RELAY.parse().unwrap(), "192.0.2.15".parse().unwrap(), "2001:db8::15".parse().unwrap());
    let client = Client::new(relay.server(transport), IPV4);
    let stream =
        (transport != Transport::Udp).then(|| (Frames::new(Framing::Messages), Frames::new(Framing::Messages)));
    Link {
        crypto: Exact(Cell::new(7)),
        client,
        relay,
        seen: SEEN.parse().unwrap(),
        stream,
        cut,
        events: Vec::new(),
        sent: 0,
        to_peers: Vec::new(),
    }
}

impl Link {
    // Carries what the client and the relay send each other until neither sends more; keeps every other event.
    fn carry(&mut self, events: Vec<Event>, now: Duration) {
        let mut queue: alloc::collections::VecDeque<Event> = events.into();
        while let Some(event) = queue.pop_front() {
            let Event::Send(bytes) = event else {
                self.events.push(event);
                continue;
            };
            self.sent += 1;
            let frames = match &mut self.stream {
                Some((up, _)) => bytes.chunks(self.cut).flat_map(|piece| up.take(piece).unwrap()).collect(),
                None => vec![bytes],
            };
            for frame in frames {
                for out in self.relay.take(self.seen, self.stream.is_some(), &frame, now) {
                    match out {
                        Out::Client(_, bytes) => {
                            let frames = match &mut self.stream {
                                Some((_, down)) => {
                                    bytes.chunks(self.cut).flat_map(|piece| down.take(piece).unwrap()).collect()
                                }
                                None => vec![bytes],
                            };
                            for frame in frames {
                                queue.extend(self.client.take(&self.crypto, &frame, now));
                            }
                        }
                        Out::Peer { from, to, data } => self.to_peers.push((from, to, data)),
                    }
                }
            }
        }
    }

    fn start(&mut self, now: Duration) {
        let events = self.client.start(&self.crypto, now);
        self.carry(events, now);
    }

    fn tick(&mut self, now: Duration) {
        let events = self.client.tick(&self.crypto, now);
        self.carry(events, now);
    }

    // A peer's datagram to the relayed address, carried to the client.
    fn peer_sends(&mut self, from: SocketAddr, data: &[u8], now: Duration) {
        let relayed = self.client.relayed().unwrap();
        for out in self.relay.take_peer(relayed, from, data, now) {
            if let Out::Client(_, bytes) = out {
                let frames = match &mut self.stream {
                    Some((_, down)) => bytes.chunks(self.cut).flat_map(|piece| down.take(piece).unwrap()).collect(),
                    None => vec![bytes],
                };
                for frame in frames {
                    let events = self.client.take(&self.crypto, &frame, now);
                    self.carry(events, now);
                }
            }
        }
    }

    fn take_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    // The client's Allocate under the credential the relay's challenge of the first asked for, not yet carried to the relay.
    fn signed(&mut self) -> Vec<u8> {
        let events = self.client.start(&self.crypto, second(1));
        let Some(Event::Send(request)) = events.first() else { panic!("no request") };
        let Out::Client(_, challenge) = &self.relay.take(self.seen, self.stream.is_some(), request, second(1))[0]
        else {
            panic!("no challenge")
        };
        let events = self.client.take(&self.crypto, challenge, second(1));
        let Some(Event::Send(signed)) = events.first() else { panic!("no signed request") };
        signed.clone()
    }
}

fn second(count: u64) -> Duration {
    Duration::from_secs(count)
}

fn whole_life(transport: Transport, cut: usize) {
    let mut link = link(transport, cut);
    let peer: SocketAddr = PEER.parse().unwrap();
    link.start(second(1));
    let relayed = link.relay.relayed_of(link.seen).expect("an allocation at the relay");
    assert_eq!(
        link.take_events(),
        [Event::Allocated { relayed, mapped: Some(link.seen) }],
        "allocated after the challenge, with the address the relay saw"
    );
    assert_eq!(link.sent, 2, "the request without the credential, then the one under it");
    let events = link.client.send(&link.crypto, &peer, b"early", second(1));
    assert!(events.is_empty(), "nothing goes to a peer without a permission");
    let events = link.client.permit(&link.crypto, peer.ip(), second(1));
    link.carry(events, second(1));
    let events = link.client.send(&link.crypto, &peer, b"held", second(1));
    link.carry(events, second(1));
    assert_eq!(link.to_peers, [(relayed, peer, b"held".to_vec())], "a Send indication once the permission stands");
    link.peer_sends(peer, b"back", second(2));
    assert_eq!(link.take_events(), [Event::Data(peer, b"back".to_vec())], "a Data indication from the peer");
    let events = link.client.bind(&link.crypto, peer, second(3));
    link.carry(events, second(3));
    let events = link.client.send(&link.crypto, &peer, b"on the channel", second(3));
    assert!(matches!(&events[..], [Event::Send(bytes)] if bytes[0] == 0x40), "ChannelData once bound");
    link.carry(events, second(3));
    assert_eq!(link.to_peers.last(), Some(&(relayed, peer, b"on the channel".to_vec())));
    link.peer_sends(peer, b"back on it", second(4));
    assert_eq!(link.take_events(), [Event::Data(peer, b"back on it".to_vec())], "ChannelData from the peer");
    let other: SocketAddr = "203.0.113.51:6001".parse().unwrap();
    let events = link.client.permit(&link.crypto, other.ip(), second(5));
    link.carry(events, second(5));
    link.client.forget(other.ip());
    link.relay.renew_nonce();
    let sent = link.sent;
    for at in 6..=604 {
        link.tick(second(at));
    }
    assert!(link.client.relayed().is_some() && link.relay.allocation_count() == 1, "refreshed ahead of its lifetime");
    assert_eq!(link.take_events(), [], "the stale nonce renewed without a word to the owner");
    assert!(link.sent > sent + 4, "refreshes of the allocation, the channel and its permission went");
    link.peer_sends(peer, b"still", second(605));
    assert_eq!(link.take_events(), [Event::Data(peer, b"still".to_vec())], "the channel kept past its ten minutes");
    link.peer_sends(other, b"late", second(605));
    assert_eq!(link.take_events(), [], "the permission let go ran out at the relay and here");
    let wait = link.client.next_wait(second(605)).unwrap();
    assert!(wait > Duration::ZERO && wait <= second(240), "something is due within the permission's refresh: {wait:?}");
    let events = link.client.close(&link.crypto, second(606));
    link.carry(events, second(606));
    assert_eq!(link.relay.allocation_count(), 0, "a refresh of lifetime nought ends it at the relay");
    assert_eq!(link.client.next_wait(second(606)), None, "and nothing is due here after");
}

#[test]
fn allocation_udp() {
    whole_life(Transport::Udp, usize::MAX);
}

#[test]
fn allocation_streams() {
    for cut in [1, 3, 7, 20, 1500] {
        whole_life(Transport::Tcp, cut);
    }
    whole_life(Transport::Tls, 5);
}

#[test]
fn password_algorithms() {
    let mut link = link(Transport::Udp, usize::MAX);
    link.relay.has_algorithms = true;
    link.start(second(1));
    assert!(matches!(link.take_events()[..], [Event::Allocated { .. }]), "under SHA-256 and MESSAGE-INTEGRITY-SHA256");
}

fn refused(transport: Transport, change: impl Fn(&mut Relay)) -> Vec<Event> {
    let mut link = link(transport, usize::MAX);
    change(&mut link.relay);
    link.start(second(1));
    link.take_events()
}

#[test]
fn allocate_errors() {
    let codes = [
        BAD_REQUEST,
        FORBIDDEN,
        FAMILY_UNSUPPORTED,
        QUOTA_REACHED,
        INSUFFICIENT_CAPACITY,
        WRONG_CREDENTIALS,
        TRANSPORT_UNSUPPORTED,
        699,
    ];
    for code in codes {
        assert_eq!(
            refused(Transport::Udp, |relay| relay.refusals = vec![(ALLOCATE, code)]),
            [Event::Failed(Failure::Refused(code))],
            "{code} ends the allocation"
        );
    }
    assert_eq!(
        refused(Transport::Udp, |relay| relay.refusals = vec![(ALLOCATE, SERVER_ERROR); 5]),
        [Event::Failed(Failure::Refused(SERVER_ERROR))],
        "a server error asked again four times"
    );
    assert!(
        matches!(
            refused(Transport::Udp, |relay| relay.refusals = vec![(ALLOCATE, SERVER_ERROR); 4])[..],
            [Event::Allocated { .. }]
        ),
        "and taken on the fifth"
    );
    assert!(
        matches!(refused(Transport::Udp, |relay| relay.knows_fragment = false)[..], [Event::Allocated { .. }]),
        "a relay that does not know DONT-FRAGMENT is asked again without it"
    );
    assert_eq!(refused(Transport::Udp, |relay| relay.offers_ipv6 = false).len(), 1, "an IPv4 allocation needs no IPv6");
    let mut link = link(Transport::Udp, usize::MAX);
    link.client = Client::new(link.relay.server(Transport::Udp), IPV6);
    link.relay.offers_ipv6 = false;
    link.start(second(1));
    assert_eq!(
        link.take_events(),
        [Event::Failed(Failure::Refused(FAMILY_UNSUPPORTED))],
        "a family the relay does not offer"
    );
    let mut wrong = self::link(Transport::Udp, usize::MAX);
    let mut server = wrong.relay.server(Transport::Udp);
    server.password = "wrong".to_string();
    wrong.client = Client::new(server, IPV4);
    wrong.start(second(1));
    assert_eq!(wrong.take_events(), [Event::Failed(Failure::Refused(UNAUTHENTICATED))], "a credential refused twice");
    let mut expired = self::link(Transport::Udp, usize::MAX);
    let mut server = expired.relay.server(Transport::Udp);
    server.expires = second(1);
    expired.client = Client::new(server, IPV4);
    expired.start(second(1));
    assert_eq!(
        (expired.take_events(), expired.sent),
        (vec![Event::Failed(Failure::Expired)], 0),
        "nothing asked under a credential run out"
    );
}

#[test]
fn mismatch_and_redirect() {
    let mut link = link(Transport::Udp, usize::MAX);
    link.relay.refusals = vec![(ALLOCATE, ALLOCATION_MISMATCH); 4];
    for _ in 0..3 {
        link.start(second(1));
        assert_eq!(link.take_events(), [Event::Rebind], "another transport address of this side");
        link.seen.set_port(link.seen.port() + 1);
    }
    link.start(second(1));
    assert_eq!(link.take_events(), [Event::Failed(Failure::Refused(ALLOCATION_MISMATCH))], "three tried at the most");
    let mut moved = self::link(Transport::Tls, usize::MAX);
    let alternate: SocketAddr = "192.0.2.16:443".parse().unwrap();
    moved.relay.alternate = Some(alternate);
    moved.relay.refusals = vec![(ALLOCATE, TRY_ALTERNATE)];
    moved.start(second(1));
    assert_eq!(moved.take_events(), [Event::Moved]);
    assert_eq!(
        (moved.client.address(), moved.client.name()),
        (alternate, Some("other.example.org")),
        "the alternate and its name"
    );
    moved.start(second(2));
    assert!(matches!(moved.take_events()[..], [Event::Allocated { .. }]), "allocated there");
    let mut circle = self::link(Transport::Udp, usize::MAX);
    circle.relay.alternate = Some(circle.relay.address);
    circle.relay.refusals = vec![(ALLOCATE, TRY_ALTERNATE); 2];
    circle.start(second(1));
    assert_eq!(circle.take_events(), [Event::Moved]);
    circle.start(second(2));
    assert_eq!(circle.take_events(), [Event::Failed(Failure::Refused(TRY_ALTERNATE))], "never back to a server tried");
}

#[test]
fn timeouts() {
    let crypto = Exact(Cell::new(3));
    let relay = Relay::new(RELAY.parse().unwrap(), "192.0.2.15".parse().unwrap(), "2001:db8::15".parse().unwrap());
    let mut client = Client::new(relay.server(Transport::Udp), IPV4);
    assert_eq!(client.start(&crypto, Duration::ZERO).len(), 1);
    let mut sent = vec![Duration::ZERO];
    let mut now = Duration::ZERO;
    let mut failed = None;
    while failed.is_none() && now < second(60) {
        now = now + client.next_wait(now).unwrap();
        for event in client.tick(&crypto, now) {
            match event {
                Event::Send(_) => sent.push(now),
                Event::Failed(failure) => failed = Some((failure, now)),
                _ => {}
            }
        }
    }
    let millis: Vec<u128> = sent.iter().map(Duration::as_millis).collect();
    assert_eq!(millis, [0, 500, 1500, 3500, 7500, 15_500, 31_500], "RFC 8489, 6.2.1: seven requests");
    assert_eq!(failed, Some((Failure::Timeout, Duration::from_millis(39_500))), "and given up 39.5 s after the first");
    let mut stream = Client::new(relay.server(Transport::Tcp), IPV4);
    stream.start(&crypto, Duration::ZERO);
    assert_eq!(stream.next_wait(Duration::ZERO), Some(Duration::from_millis(39_500)), "a stream retransmits nothing");
    assert_eq!(stream.tick(&crypto, Duration::from_millis(39_500)), [Event::Failed(Failure::Timeout)]);
}

#[test]
fn refresh_integrity_failures() {
    let mut link = link(Transport::Udp, usize::MAX);
    link.start(second(1));
    link.take_events();
    link.relay.refusals = vec![(REFRESH, ALLOCATION_MISMATCH)];
    for at in 2..=541 {
        link.tick(second(at));
    }
    assert_eq!(link.take_events(), [Event::Failed(Failure::Lost)], "a refresh that finds no allocation");
    let mut silent = self::link(Transport::Udp, usize::MAX);
    silent.start(second(1));
    silent.take_events();
    let mut failed = Vec::new();
    for at in 541..=601 {
        failed.extend(
            silent.client.tick(&silent.crypto, second(at)).into_iter().filter(|event| !matches!(event, Event::Send(_))),
        );
    }
    assert_eq!(failed, [Event::Failed(Failure::Timeout)], "a refresh never answered, given up 39.5 s on");
    let mut forged = self::link(Transport::Tcp, usize::MAX);
    let signed = forged.signed();
    let mut answer = match &forged.relay.take(forged.seen, true, &signed, second(1))[0] {
        Out::Client(_, bytes) => bytes.clone(),
        other => panic!("{other:?}"),
    };
    // A byte of its integrity changed, and its fingerprint made anew so that the message reads.
    let at = answer.len() - 12;
    answer[at] ^= 1;
    assert!(Message::parse(&answer).is_none(), "the changed answer's fingerprint no longer holds");
    let mut fixed = answer[..answer.len() - 8].to_vec();
    let length = (fixed.len() + 8 - 20) as u16;
    fixed[2..4].copy_from_slice(&length.to_be_bytes());
    let sum = crate::base::checksum::crc32(&fixed) ^ 0x5354_554E;
    fixed.extend([0x80, 0x28, 0, 4]);
    fixed.extend(sum.to_be_bytes());
    assert_eq!(
        forged.client.take(&forged.crypto, &fixed, second(1)),
        [Event::Failed(Failure::Unverified)],
        "over a stream an answer whose integrity does not hold ends it"
    );
    let mut unknown = self::link(Transport::Udp, usize::MAX);
    let events = unknown.client.start(&unknown.crypto, second(1));
    let Some(Event::Send(request)) = events.first() else { panic!("no request") };
    let message = Message::parse(request).unwrap();
    let mut writer = Writer::new(ALLOCATE, Class::Error, &message.transaction);
    writer.push(Attribute::Error(UNAUTHENTICATED)).push(Attribute::Realm(REALM)).push(Attribute::Nonce(b"nonce1"));
    let events = unknown.client.take(&unknown.crypto, &writer.finish(true), second(1));
    let Some(Event::Send(signed)) = events.first() else { panic!("no signed request") };
    let message = Message::parse(signed).unwrap();
    let mut writer = Writer::new(ALLOCATE, Class::Success, &message.transaction);
    writer.push(Attribute::Raw(0x0031, &[1, 2, 3, 4]));
    let key = unknown.relay.key(false);
    writer.seal(&unknown.crypto, Integrity::Sha1(&key));
    assert_eq!(
        unknown.client.take(&unknown.crypto, &writer.finish(true), second(1)),
        [Event::Failed(Failure::Refused(BAD_REQUEST))],
        "a success that holds an attribute this side must understand and does not is no success"
    );
}

#[test]
fn discarded_answers() {
    let mut link = link(Transport::Udp, usize::MAX);
    let transaction = Message::parse(&link.signed()).unwrap().transaction;
    let key = link.relay.key(false);
    let answer = |class: Class, attributes: &[Attribute<'_>], key: Option<&[u8]>| {
        let mut writer = Writer::new(ALLOCATE, class, &transaction);
        for attribute in attributes {
            writer.push(*attribute);
        }
        if let Some(key) = key {
            writer.seal(&link.crypto, Integrity::Sha1(key));
        }
        writer.finish(true)
    };
    let relayed = Attribute::Relayed("192.0.2.15:50001".parse().unwrap());
    let downgraded = answer(Class::Success, &[relayed, Attribute::Nonce(COOKIE.as_bytes())], Some(&key));
    let unsealed = answer(Class::Success, &[relayed], None);
    let forged = answer(Class::Success, &[relayed], Some(b"wrong"));
    for (bytes, reason, case) in [
        (&downgraded, Discard::Downgraded, "an answer whose nonce asks for algorithms it does not list"),
        (&unsealed, Discard::Unsealed, "a success without integrity"),
        (&forged, Discard::Unverified, "a success whose integrity does not hold"),
    ] {
        assert_eq!(
            link.client.take(&link.crypto, bytes, second(1)),
            [Event::Discarded { code: None, reason }],
            "{case} is as if never received, and told why"
        );
    }
    assert_eq!(link.client.take(&link.crypto, &forged, second(1)), [], "an answer passed over again is told once");
    let mut told = Vec::new();
    for at in 2..=45 {
        let events = link.client.tick(&link.crypto, second(at));
        told.extend(events.into_iter().filter(|event| !matches!(event, Event::Send(_))).map(|event| (event, at)));
    }
    assert_eq!(told, [(Event::Failed(Failure::Unverified), 41)], "where no answer held, a broken integrity is told");
}

#[test]
fn unchecked_errors() {
    for transport in [Transport::Udp, Transport::Tcp, Transport::Tls] {
        let events = refused(transport, |relay| (relay.knows_fragment, relay.seals_errors) = (false, false));
        assert!(
            matches!(events[..], [Event::Allocated { .. }]),
            "a 420 without integrity asked again without DONT-FRAGMENT over {transport:?}: {events:?}"
        );
    }
    let unsealed = |transport, code| {
        refused(transport, |relay| (relay.seals_errors, relay.refusals) = (false, vec![(ALLOCATE, code)]))
    };
    assert_eq!(
        unsealed(Transport::Udp, QUOTA_REACHED),
        [Event::Failed(Failure::Refused(QUOTA_REACHED))],
        "a refusal without integrity ends the allocation by its code"
    );
    assert_eq!(
        unsealed(Transport::Tls, TRY_ALTERNATE),
        [Event::Failed(Failure::Unverified)],
        "a redirect without integrity is never followed"
    );
    let mut link = link(Transport::Tcp, usize::MAX);
    let mut writer = Writer::new(ALLOCATE, Class::Error, &Message::parse(&link.signed()).unwrap().transaction);
    writer.push(Attribute::Error(FORBIDDEN)).seal(&link.crypto, Integrity::Sha1(b"other"));
    assert_eq!(
        link.client.take(&link.crypto, &writer.finish(true), second(1)),
        [Event::Failed(Failure::Refused(FORBIDDEN))],
        "an error sealed under another key is taken as libwebrtc takes it"
    );
}

#[test]
fn unsealed_400() {
    for transport in [Transport::Udp, Transport::Tcp, Transport::Tls] {
        let mut link = link(transport, usize::MAX);
        (link.relay.seals_errors, link.relay.refusals) = (false, vec![(ALLOCATE, BAD_REQUEST)]);
        link.start(second(1));
        assert_eq!(
            (link.take_events(), link.client.next_wait(second(1))),
            (vec![Event::Failed(Failure::Refused(BAD_REQUEST))], None),
            "over {transport:?} a signed Allocate refused 400 without integrity fails at once, as libwebrtc fails it, and waits for nothing"
        );
    }
    let mut link = link(Transport::Tcp, usize::MAX);
    let events = link.client.start(&link.crypto, second(1));
    let Some(Event::Send(request)) = events.first() else { panic!("no request") };
    let mut writer = Writer::new(ALLOCATE, Class::Error, &Message::parse(request).unwrap().transaction);
    writer.push(Attribute::Error(BAD_REQUEST));
    let unsealed = writer.finish(true);
    assert_eq!(
        link.client.take(&link.crypto, &unsealed, second(1)),
        [Event::Discarded { code: Some(BAD_REQUEST), reason: Discard::Unsealed }],
        "a 400 without integrity to the Allocate without the credential is passed over, as RFC 8489 and libwebrtc pass it over"
    );
    assert_eq!(link.client.take(&link.crypto, &unsealed, second(1)), [], "and told once");
    assert_eq!(
        link.client.tick(&link.crypto, second(1) + STREAM_WAIT),
        [Event::Failed(Failure::Timeout)],
        "the stream's request then waits out its time"
    );
}

#[test]
fn stale_nonce_downgraded() {
    let mut link = link(Transport::Tcp, usize::MAX);
    let mut writer = Writer::new(ALLOCATE, Class::Error, &Message::parse(&link.signed()).unwrap().transaction);
    writer.push(Attribute::Error(STALE_NONCE)).push(Attribute::Realm(REALM)).push(Attribute::Nonce(COOKIE.as_bytes()));
    assert_eq!(
        link.client.take(&link.crypto, &writer.finish(true), second(1)),
        [Event::Failed(Failure::Refused(STALE_NONCE))],
        "a 438 whose nonce asks for algorithms it does not list is not asked again (RFC 8489, 9.2.5), as libwebrtc asks none"
    );
}

#[test]
fn requested_family() {
    let first_request = |family| {
        let mut link = link(Transport::Udp, usize::MAX);
        link.client = Client::new(link.relay.server(Transport::Udp), family);
        let events = link.client.start(&link.crypto, second(1));
        let Some(Event::Send(request)) = events.first() else { panic!("no request") };
        Message::parse(request).unwrap().find(REQUESTED_ADDRESS_FAMILY).map(<[u8]>::to_vec)
    };
    assert_eq!(
        (first_request(IPV4), first_request(IPV6)),
        (None, Some(vec![IPV6, 0, 0, 0])),
        "IPv4, a server's default, is asked without REQUESTED-ADDRESS-FAMILY, as libwebrtc asks it; IPv6 by it"
    );
    let unknowing = |family| {
        let mut link = link(Transport::Tls, usize::MAX);
        link.client = Client::new(link.relay.server(Transport::Tls), family);
        link.relay.knows_family = false;
        link.start(second(1));
        link.take_events()
    };
    assert!(
        matches!(unknowing(IPV4)[..], [Event::Allocated { relayed, .. }] if relayed.is_ipv4()),
        "a relay that does not know the attribute allocates IPv4"
    );
    assert_eq!(
        unknowing(IPV6),
        [Event::Failed(Failure::Refused(UNKNOWN_ATTRIBUTE))],
        "and refuses the IPv6 the attribute alone asks for"
    );
}

#[test]
fn permission_channel_refusals() {
    let mut link = link(Transport::Udp, usize::MAX);
    link.start(second(1));
    link.take_events();
    let v6: SocketAddr = "[2001:db8::50]:6000".parse().unwrap();
    let events = link.client.permit(&link.crypto, v6.ip(), second(2));
    link.carry(events, second(2));
    assert!(
        link.client.send(&link.crypto, &v6, b"x", second(2)).is_empty(),
        "a peer of another family is refused, nothing held"
    );
    let peer: SocketAddr = PEER.parse().unwrap();
    link.relay.refusals = vec![(CHANNEL_BIND, BAD_REQUEST)];
    let events = link.client.bind(&link.crypto, peer, second(3));
    link.carry(events, second(3));
    let events = link.client.send(&link.crypto, &peer, b"indicated", second(3));
    assert!(
        matches!(&events[..], [Event::Send(bytes)] if bytes[0] == 0x00),
        "a Send indication where the channel was refused"
    );
    link.carry(events, second(3));
    assert_eq!(link.to_peers.last().map(|(_, _, data)| data.as_slice()), Some(&b"indicated"[..]));
    let sent = link.sent;
    for at in 4..60 {
        link.tick(second(at));
    }
    assert_eq!(link.sent, sent, "neither is asked again");
}
