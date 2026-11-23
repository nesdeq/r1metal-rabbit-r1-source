use super::agent::{Agent, Base, Local, MOST_CANDIDATES, Own, Relay, Via};
use super::candidate::{
    Candidate, DISCARD_PORT, Direction, HOST, Kind, SRFLX, foundation, local_preference, priority, tcp_preference,
    type_preference,
};
use super::{Socket, Step, to_discard_line, to_relay_line};
use crate::net::stun::{Crypto, Frames, Framing, IPV4, IPV6, Transport, family_of};
use crate::net::turn::{self, Client, Failure};
use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

// Host candidates' local preferences, this side's and a server's: IPv6 above IPv4 (RFC 8421, 4), each address of a family one below the one before (RFC 8445, 5.1.2.1).
pub(super) const IPV6_PREFERENCE: u32 = 0xFFFF;
pub(super) const IPV4_PREFERENCE: u32 = 0x7FFF;
// A relay's local preference: its transport, UDP above TCP above TLS, above its order (RFC 8445, 5.1.2.2; RFC 6544, 4.2).
const TRANSPORT_SHIFT: u32 = 13;
const ORDER_MOST: u32 = 8191;
// A server-reflexive binding is kept alive by a Binding request every Tr while no pair is selected (RFC 8445, 5.1.1.4, 11).
pub(super) const KEEPALIVE: Duration = Duration::from_secs(15);

const fn rank(transport: Transport) -> u32 {
    match transport {
        Transport::Udp => 3,
        Transport::Tcp => 2,
        Transport::Tls => 1,
    }
}

impl Agent {
    /// Takes an address of this side: a host candidate on a socket of its own, its reflexive ones and relayed ones through the relays of its family.
    pub fn local(&mut self, ip: IpAddr) {
        if self.is_closed || self.locals.iter().any(|local| local.ip == ip) || self.locals.len() >= MOST_CANDIDATES {
            return;
        }
        let order = self.locals.iter().filter(|local| local.ip.is_ipv6() == ip.is_ipv6()).count() as u32;
        let preference = (if ip.is_ipv6() { IPV6_PREFERENCE } else { IPV4_PREFERENCE }).saturating_sub(order);
        let socket = (!self.is_relayed).then(|| self.socket());
        if let Some(socket) = socket {
            self.steps.push_back(Step::Bind { socket, local: ip });
            // The active TCP candidate (RFC 6544, 4.1): told to nobody, it pairs with servers' passive ones.
            let candidate = Candidate {
                kind: Kind::Host,
                transport: Transport::Tcp,
                address: SocketAddr::new(ip, DISCARD_PORT),
                priority: priority(type_preference(Kind::Host, Transport::Tcp), tcp_preference(true, preference)),
                foundation: foundation(Kind::Host, ip, None, Transport::Tcp),
                related: None,
                direction: Some(Direction::Active),
                ufrag: None,
            };
            self.add_own(candidate, Base::Tcp(ip), false);
        }
        self.locals.push(Local { ip, socket, candidate: None, preference, refresh: None });
        for index in 0..self.servers.len() {
            if self.servers[index].address.is_ipv6() == ip.is_ipv6() {
                self.open_relays(index, ip);
            }
        }
    }

    // Lets an address of this side go: its sockets closed, its candidates told gone and their pairs dropped.
    pub(crate) fn drop_local(&mut self, crypto: &impl Crypto, ip: IpAddr, now: Duration) {
        let Some(at) = self.locals.iter().position(|local| local.ip == ip) else { return };
        let local = self.locals.remove(at);
        let mut sockets: Vec<Socket> = local.socket.into_iter().collect();
        let mut relays = Vec::new();
        for at in 0..self.relays.len() {
            if self.relays[at].local == ip {
                relays.push(self.relays[at].id);
                sockets.push(self.relays[at].socket);
                if self.relays[at].is_open {
                    let events = self.relays[at].client.close(crypto, now);
                    self.carry(at, events);
                }
            }
        }
        self.relays.retain(|relay| relay.local != ip);
        sockets.extend(self.streams.iter().filter(|stream| stream.local == ip).map(|stream| stream.socket));
        self.streams.retain(|stream| stream.local != ip);
        self.steps.extend(sockets.into_iter().map(Step::Close));
        self.gathers.retain(|(local, _)| *local != ip);
        self.remove_owns(|own| match own.base {
            Base::Udp(socket) => Some(socket) == local.socket,
            Base::Relay(id) => relays.contains(&id),
            Base::Tcp(base) => base == ip,
        });
    }

    // Takes a STUN server, which the host candidates of its family ask for their reflexive candidates.
    pub(crate) fn add_stun(&mut self, server: SocketAddr) {
        if self.is_closed || self.stuns.contains(&server) || self.stuns.len() >= MOST_CANDIDATES {
            return;
        }
        self.stuns.push(server);
        for local in &self.locals {
            if local.candidate.is_some() && local.ip.is_ipv6() == server.is_ipv6() {
                self.gathers.push_back((local.ip, server));
            }
        }
        self.clear_idle();
    }

    // Takes a relay: each address of its family allocates a relayed candidate of each family on it; a relay over UDP is asked for reflexive candidates too.
    pub(crate) fn add_relay(&mut self, server: turn::Server) {
        if self.is_closed || self.servers.contains(&server) || self.servers.len() >= MOST_CANDIDATES {
            return;
        }
        self.servers.push(server);
        let index = self.servers.len() - 1;
        let server = &self.servers[index];
        let (family, is_udp, address) = (server.address.is_ipv6(), server.transport == Transport::Udp, server.address);
        let ips: Vec<(IpAddr, bool)> = self
            .locals
            .iter()
            .filter(|local| local.ip.is_ipv6() == family)
            .map(|local| (local.ip, local.candidate.is_some()))
            .collect();
        for (ip, is_bound) in ips {
            self.open_relays(index, ip);
            if is_udp && is_bound && !self.is_relayed {
                self.gathers.push_back((ip, address));
            }
        }
        self.clear_idle();
    }

    // The servers a host candidate asks for its reflexive address: the STUN servers, and the relays over UDP, which answer Binding too.
    pub(super) fn stun_servers(&self) -> Vec<SocketAddr> {
        let relays =
            self.servers.iter().filter(|server| server.transport == Transport::Udp).map(|server| server.address);
        self.stuns.iter().copied().chain(relays).collect()
    }

    // Opens a relay's allocations from an address of this side: one for a relayed address of each family (RFC 8656, 7.1).
    fn open_relays(&mut self, index: usize, ip: IpAddr) {
        for family in [IPV4, IPV6] {
            if self.relays.len() >= MOST_CANDIDATES {
                return;
            }
            let server = self.servers[index].clone();
            let order = (self.relays.len() as u32).min(ORDER_MOST);
            let preference = rank(server.transport) << TRANSPORT_SHIFT | (ORDER_MOST - order);
            let (id, socket) = (self.id(), self.socket());
            let is_stream = server.transport != Transport::Udp;
            self.relays.push(Relay {
                id,
                local: ip,
                socket,
                client: Client::new(server, family),
                frames: is_stream.then(|| Frames::new(Framing::Messages)),
                is_open: false,
                candidate: None,
                preference,
                permitted: Vec::new(),
                bound: Vec::new(),
            });
            self.open_relay(self.relays.len() - 1);
        }
    }

    // Asks the owner for a relay's socket: a datagram socket, or a stream to the relay.
    fn open_relay(&mut self, at: usize) {
        let relay = &self.relays[at];
        let (socket, local) = (relay.socket, relay.local);
        self.steps.push_back(match relay.frames {
            Some(_) => Step::Connect {
                socket,
                local,
                far: relay.client.address(),
                name: relay.client.name().map(String::from),
            },
            None => Step::Bind { socket, local },
        });
    }

    // A relay's socket anew, after a redirect over a stream or an allocation the relay holds for the old one.
    fn reopen_relay(&mut self, at: usize) {
        self.steps.push_back(Step::Close(self.relays[at].socket));
        let socket = self.socket();
        let relay = &mut self.relays[at];
        (relay.socket, relay.is_open) = (socket, false);
        if relay.frames.is_some() {
            relay.frames = Some(Frames::new(Framing::Messages));
        }
        self.open_relay(at);
    }

    // Takes what opening a UDP socket came to: its bound address, or none where it failed.
    pub(crate) fn take_bound(
        &mut self,
        crypto: &impl Crypto,
        socket: Socket,
        bound: Option<SocketAddr>,
        now: Duration,
    ) {
        if self.is_closed {
            return;
        }
        if let Some(at) = self.locals.iter().position(|local| local.socket == Some(socket)) {
            let Some(address) = bound else {
                self.locals[at].socket = None;
                return self.log(String::from("ice: a host socket was not opened"));
            };
            let (ip, preference) = (self.locals[at].ip, self.locals[at].preference);
            let candidate = Candidate {
                kind: Kind::Host,
                transport: Transport::Udp,
                address,
                priority: priority(HOST, preference),
                foundation: foundation(Kind::Host, ip, None, Transport::Udp),
                related: None,
                direction: None,
                ufrag: None,
            };
            let id = self.add_own(candidate, Base::Udp(socket), true);
            if let Some(local) = self.locals.iter_mut().find(|local| local.socket == Some(socket)) {
                (local.candidate, local.refresh) = (id, Some(now + KEEPALIVE));
            }
            for server in self.stun_servers() {
                if server.is_ipv6() == ip.is_ipv6() {
                    self.gathers.push_back((ip, server));
                }
            }
        } else {
            self.take_relay_open(crypto, socket, bound.is_some(), now);
        }
    }

    // Takes what connecting a stream came to: its local address, or none where it failed.
    pub(crate) fn take_connected(
        &mut self,
        crypto: &impl Crypto,
        socket: Socket,
        connected: Option<SocketAddr>,
        now: Duration,
    ) {
        if self.is_closed {
            return;
        }
        if self.relays.iter().any(|relay| relay.socket == socket) {
            return self.take_relay_open(crypto, socket, connected.is_some(), now);
        }
        let Some(stream) = self.streams.iter_mut().find(|stream| stream.socket == socket) else { return };
        if connected.is_none() {
            return self.take_closed(socket);
        }
        stream.is_open = true;
        let waiting: Vec<u32> =
            self.pairs.iter().filter(|pair| pair.stream == Some(socket)).map(|pair| pair.id).collect();
        for id in waiting {
            self.check(crypto, id, false, now);
        }
    }

    fn take_relay_open(&mut self, crypto: &impl Crypto, socket: Socket, is_open: bool, now: Duration) {
        let Some(at) = self.relays.iter().position(|relay| relay.socket == socket) else { return };
        if !is_open {
            let transport = self.relays[at].client.transport();
            self.log(to_relay_line(transport, Failure::Lost));
            return self.drop_relay(self.relays[at].id);
        }
        self.relays[at].is_open = true;
        let events = self.relays[at].client.start(crypto, now);
        self.relayed(crypto, at, events, now);
    }

    // Takes the end of a stream, or of a socket that failed.
    pub(crate) fn take_closed(&mut self, socket: Socket) {
        if self.is_closed {
            return;
        }
        if let Some(relay) = self.relays.iter().find(|relay| relay.socket == socket) {
            let (id, transport) = (relay.id, relay.client.transport());
            self.log(to_relay_line(transport, Failure::Lost));
            return self.drop_relay(id);
        }
        if let Some(at) = self.streams.iter().position(|stream| stream.socket == socket) {
            self.streams.remove(at);
            self.steps.push_back(Step::Close(socket));
            return self.stream_closed(socket);
        }
        if let Some(at) = self.locals.iter().position(|local| local.socket == Some(socket)) {
            (self.locals[at].socket, self.locals[at].candidate) = (None, None);
            self.steps.push_back(Step::Close(socket));
            self.remove_owns(|own| own.base == Base::Udp(socket));
        }
    }

    // What a relay's client tells: bytes for the relay, its allocation, data from peers, a move, an answer passed over, or its end.
    pub(super) fn relayed(&mut self, crypto: &impl Crypto, at: usize, events: Vec<turn::Event>, now: Duration) {
        let id = self.relays[at].id;
        for event in events {
            let Some(at) = self.relays.iter().position(|relay| relay.id == id) else { return };
            match event {
                turn::Event::Send(_) => self.carry(at, alloc::vec![event]),
                turn::Event::Allocated { relayed, mapped } => self.allocated(at, relayed, mapped),
                turn::Event::Data(peer, data) => self.arrive(crypto, Via::Relay(id), peer, Cow::Owned(data), now),
                turn::Event::Moved if self.relays[at].frames.is_some() => self.reopen_relay(at),
                turn::Event::Moved => {
                    let events = self.relays[at].client.start(crypto, now);
                    self.relayed(crypto, at, events, now);
                }
                turn::Event::Rebind => self.reopen_relay(at),
                turn::Event::Discarded { code, reason } => {
                    let transport = self.relays[at].client.transport();
                    self.log(to_discard_line(transport, code, reason));
                }
                turn::Event::Failed(failure) => {
                    let transport = self.relays[at].client.transport();
                    self.log(to_relay_line(transport, failure));
                    self.drop_relay(id);
                }
            }
        }
    }

    fn allocated(&mut self, at: usize, relayed: SocketAddr, mapped: Option<SocketAddr>) {
        // A relayed candidate the same as a host candidate is none (RFC 8445, 5.1.1.2).
        if self.owns.iter().any(|own| own.candidate.address == relayed) {
            return;
        }
        let relay = &self.relays[at];
        let server = relay.client.address().ip();
        let candidate = Candidate {
            kind: Kind::Relay,
            transport: Transport::Udp,
            address: relayed,
            priority: priority(type_preference(Kind::Relay, relay.client.transport()), relay.preference),
            foundation: foundation(Kind::Relay, relayed.ip(), Some(server), relay.client.transport()),
            related: mapped,
            direction: None,
            ufrag: None,
        };
        let id = relay.id;
        let own = self.add_own(candidate, Base::Relay(id), true);
        self.relays[at].candidate = own;
    }

    // Lets a relay go: its socket closed, its candidate told gone and its pairs dropped.
    fn drop_relay(&mut self, id: u32) {
        let Some(at) = self.relays.iter().position(|relay| relay.id == id) else { return };
        let relay = self.relays.remove(at);
        self.steps.push_back(Step::Close(relay.socket));
        self.remove_owns(|own| own.base == Base::Relay(id));
    }

    // Adds a candidate of this side, tells it where it is to be told, and pairs it; none where the table is full or it is redundant (RFC 8445, 5.1.3).
    fn add_own(&mut self, candidate: Candidate, base: Base, is_told: bool) -> Option<u32> {
        let is_redundant = self.owns.iter().any(|own| own.candidate.address == candidate.address && own.base == base);
        if is_redundant || self.owns.len() >= MOST_CANDIDATES {
            return None;
        }
        let id = self.id();
        if is_told {
            self.steps.push_back(Step::Candidate(candidate.to_line(&self.ufrag, self.is_relayed)));
        }
        self.owns.push(Own { id, candidate, base, is_told });
        self.pair_own(id);
        Some(id)
    }

    // Drops the candidates of this side a test picks: the told ones told gone, their pairs dropped.
    fn remove_owns(&mut self, is_gone: impl Fn(&Own) -> bool) {
        let mut ids = Vec::new();
        for own in self.owns.iter().filter(|own| is_gone(own)) {
            ids.push(own.id);
            if own.is_told {
                self.steps.push_back(Step::Removed(own.candidate.address));
            }
        }
        self.owns.retain(|own| !is_gone(own));
        self.remove_pairs(|pair| ids.contains(&pair.local));
    }

    // Takes the answer of a STUN server: a server-reflexive candidate where the mapped address is not the host's (RFC 8445, 5.1.1.2, 5.1.3).
    pub(super) fn reflexive(&mut self, local: IpAddr, server: SocketAddr, mapped: SocketAddr) {
        let Some(host) = self.locals.iter().find(|candidate| candidate.ip == local).and_then(|local| local.candidate)
        else {
            return;
        };
        let Some(own) = self.own(host) else { return };
        let (base, preference) = (own.base, local_preference(own.candidate.priority));
        let candidate = Candidate {
            kind: Kind::Srflx,
            transport: Transport::Udp,
            address: mapped,
            priority: priority(SRFLX, preference),
            foundation: foundation(Kind::Srflx, local, Some(server.ip()), Transport::Udp),
            related: Some(own.candidate.address),
            direction: None,
            ufrag: None,
        };
        if family_of(mapped.ip()) == family_of(local) {
            self.add_own(candidate, base, true);
        }
    }
}
