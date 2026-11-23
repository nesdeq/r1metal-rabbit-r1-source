mod rewrite;

use super::dns::nat64::PORT;
use super::{Address, icmp, to_whole_ipv4};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;
use rewrite::{ACK, End, FIN, Kind, Parsed, Piece, Protocol, RST, SYN};

// The ports a mapping is given where its own is taken (RFC 6056), and as many flows as a way holds of a protocol.
const FIRST_PORT: u16 = 1024;
const PORTS: usize = (u16::MAX - FIRST_PORT) as usize + 1;
// How long a flow nothing passes over is kept: UDP's (RFC 4787, REQ-5), TCP's once both sides opened it and before or after (RFC 5382, REQ-5), an echo's (RFC 5508).
const UDP_LIFE: Duration = Duration::from_secs(5 * 60);
const TCP_LIFE: Duration = Duration::from_secs(2 * 3600 + 4 * 60);
const TCP_EDGE_LIFE: Duration = Duration::from_secs(4 * 60);
const ECHO_LIFE: Duration = Duration::from_secs(60);
// How often the flows nothing passes over are looked for and dropped: as long as the shortest of them is kept.
pub(crate) const SWEEP_EVERY: Duration = ECHO_LIFE;
const OPENED_OUT: u8 = 1;
const OPENED_IN: u8 = 2;
const ENDING: u8 = 4;
const WAYS: usize = 3;

// Whose flow a packet is of, on the inner side of a router of one host, which translates addresses (RFC 4787, 5382, 5508).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Inside {
    Host,
    Own,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Way {
    Lte,
    Tunnel,
    // To the device's own resolver's relay, whose sockets the kernel keeps.
    Relay,
}

// The way the host's packets take: over the modem's connection, or into the tunnel, whose resolver its lookups go to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Path {
    Lte,
    Tunnel(Ipv4Addr),
}

// Where a translated packet goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sink {
    Lte,
    Tunnel,
    Own,
    Host,
}

// A packet to hand on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Emit {
    pub(crate) sink: Sink,
    pub(crate) packet: Vec<u8>,
}

/// A way's outer end: the address its packets leave from, and the largest packet it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outside {
    pub(crate) address: Ipv4Addr,
    pub mtu: usize,
}

// A flow as its way tells it: the port it leaves from, and the far end.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Outer {
    way: Way,
    protocol: Protocol,
    port: u16,
    remote: End,
}

// A flow as its inner side tells it: its own port, and the far end as it names it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Inner {
    inside: Inside,
    protocol: Protocol,
    port: u16,
    seen: End,
}

// The pieces of one packet, by whether they go out, the addresses they come with, their protocol and their number.
type Mark = (bool, Address, Address, u8, u16);

struct Flow {
    inner: Inner,
    state: u8,
    last: Duration,
    piece: Option<Mark>,
}

impl Flow {
    fn life(&self) -> Duration {
        match self.inner.protocol {
            Protocol::Udp => UDP_LIFE,
            Protocol::Echo => ECHO_LIFE,
            Protocol::Tcp if self.state == OPENED_OUT | OPENED_IN => TCP_LIFE,
            Protocol::Tcp => TCP_EDGE_LIFE,
        }
    }

    fn is_alive(&self, now: Duration) -> bool {
        now.saturating_sub(self.last) < self.life()
    }

    fn note(&mut self, is_out: bool, flags: u8, now: Duration) {
        self.last = now;
        if flags & SYN != 0 {
            self.state |= if is_out { OPENED_OUT } else { OPENED_IN };
        }
        if flags & (FIN | RST) != 0 {
            self.state |= ENDING;
        }
    }
}

const fn sink_of(way: Way) -> Sink {
    match way {
        Way::Lte => Sink::Lte,
        Way::Tunnel => Sink::Tunnel,
        Way::Relay => Sink::Own,
    }
}

const fn inner_sink(inside: Inside) -> Sink {
    match inside {
        Inside::Host => Sink::Host,
        Inside::Own => Sink::Own,
    }
}

// True for an address no packet is routed to: this network's own, a loop's, a group's, every station's.
const fn is_unroutable(address: Address) -> bool {
    matches!(address[0], 0 | 127 | 224..)
}

// The translation's state: the addresses of the inner side, the ways out, and the flows.
pub(crate) struct Nat {
    host: Address,
    gateway: Address,
    own: Address,
    lte: Option<Outside>,
    tunnel: Option<Outside>,
    flows: BTreeMap<Outer, Flow>,
    inward: BTreeMap<Inner, Outer>,
    pieces: BTreeMap<Mark, Outer>,
    counts: [[usize; Protocol::COUNT]; WAYS],
}

impl Nat {
    // A translation for the host at `host`, whose router and resolver is `gateway`, and the device's own stack at `own`.
    pub(crate) fn new(host: Ipv4Addr, gateway: Ipv4Addr, own: Ipv4Addr) -> Nat {
        Nat {
            host: host.octets(),
            gateway: gateway.octets(),
            own: own.octets(),
            lte: None,
            tunnel: None,
            flows: BTreeMap::new(),
            inward: BTreeMap::new(),
            pieces: BTreeMap::new(),
            counts: [[0; Protocol::COUNT]; WAYS],
        }
    }

    // Sets the outer end of the modem's way, or that there is none; a change drops the way's flows.
    pub(crate) fn set_lte(&mut self, outside: Option<Outside>) {
        if core::mem::replace(&mut self.lte, outside) != outside {
            self.drop_where(|outer, _| outer.way == Way::Lte);
        }
    }

    // Sets the outer end of the tunnel's way, or that there is none; a change drops the way's flows.
    pub(crate) fn set_tunnel(&mut self, outside: Option<Outside>) {
        if core::mem::replace(&mut self.tunnel, outside) != outside {
            self.drop_where(|outer, _| outer.way == Way::Tunnel);
        }
    }

    // Drops the host's flows, so that none goes on along a way the host's packets take no more.
    pub(crate) fn forget_host(&mut self) {
        self.drop_where(|_, flow| flow.inner.inside == Inside::Host);
    }

    // Drops the flows nothing passed over for as long as each is kept.
    pub(crate) fn sweep(&mut self, now: Duration) {
        self.drop_where(|_, flow| !flow.is_alive(now));
    }

    // How many flows are kept.
    pub(crate) fn flow_count(&self) -> usize {
        self.flows.len()
    }

    fn drop_where(&mut self, is_dropped: impl Fn(&Outer, &Flow) -> bool) {
        let dropped: Vec<Outer> =
            self.flows.iter().filter(|(outer, flow)| is_dropped(outer, flow)).map(|(outer, _)| *outer).collect();
        dropped.iter().for_each(|outer| self.remove(outer));
    }

    fn remove(&mut self, outer: &Outer) {
        if let Some(flow) = self.flows.remove(outer) {
            self.inward.remove(&flow.inner);
            if let Some(mark) = &flow.piece {
                self.pieces.remove(mark);
            }
            self.counts[outer.way as usize][outer.protocol as usize] -= 1;
        }
    }

    // The address a way's packets leave from and the largest it carries; the relay's packets keep the host's address.
    fn outside(&self, way: Way) -> Option<(Address, usize)> {
        match way {
            Way::Lte => self.lte.map(|outside| (outside.address.octets(), outside.mtu)),
            Way::Tunnel => self.tunnel.map(|outside| (outside.address.octets(), outside.mtu)),
            Way::Relay => Some((self.host, usize::MAX)),
        }
    }

    const fn inner_address(&self, inside: Inside) -> Address {
        match inside {
            Inside::Host => self.host,
            Inside::Own => self.own,
        }
    }

    // The way a new flow takes and its far end: the host's lookups at its router go to the resolver of its path, and nothing else of the host's reaches the router's address or one that is no host's.
    fn route(&self, inside: Inside, path: Path, protocol: Protocol, target: End) -> Option<(Way, End)> {
        let is_lookup = target == (self.gateway, PORT) && protocol != Protocol::Echo;
        match (inside, path) {
            (Inside::Own, _) => Some((Way::Lte, target)),
            (Inside::Host, _) if target.0 == self.gateway && !is_lookup || is_unroutable(target.0) => None,
            (Inside::Host, Path::Lte) if is_lookup => Some((Way::Relay, (self.own, PORT))),
            (Inside::Host, Path::Lte) => Some((Way::Lte, target)),
            (Inside::Host, Path::Tunnel(resolver)) if is_lookup => Some((Way::Tunnel, (resolver.octets(), PORT))),
            (Inside::Host, Path::Tunnel(_)) => Some((Way::Tunnel, target)),
        }
    }

    // The port a new flow leaves from: its own where no flow of its way to its far end has it, else the next free one.
    fn choose_port(&mut self, way: Way, protocol: Protocol, wanted: u16, remote: End, now: Duration) -> Option<u16> {
        if self.counts[way as usize][protocol as usize] >= PORTS {
            self.sweep(now);
        }
        if self.counts[way as usize][protocol as usize] >= PORTS {
            return None;
        }
        let is_free = |port: u16| !self.flows.contains_key(&Outer { way, protocol, port, remote });
        let start = usize::from(wanted.saturating_add(1).max(FIRST_PORT) - FIRST_PORT);
        let mut others = (0..PORTS).map(|step| FIRST_PORT + ((start + step) % PORTS) as u16);
        if is_free(wanted) { Some(wanted) } else { others.find(|port| is_free(*port)) }
    }

    // The flow a packet from the inner side is of, begun where it begins one.
    fn find_out(&mut self, inside: Inside, path: Path, parsed: &Parsed, now: Duration) -> Option<Outer> {
        let Kind::Flow(protocol, flags) = parsed.kind else {
            return None;
        };
        let inner = Inner { inside, protocol, port: parsed.source.1, seen: parsed.target };
        let routed = self.route(inside, path, protocol, parsed.target);
        if let Some(outer) = self.inward.get(&inner).copied() {
            // A flow goes on only by the way its packets take now: one of a path since left ends here.
            let is_on_path = routed == Some((outer.way, outer.remote));
            if let Some(flow) = self.flows.get_mut(&outer).filter(|flow| is_on_path && flow.is_alive(now)) {
                flow.note(true, flags, now);
                return Some(outer);
            }
            self.remove(&outer);
        }
        // A connection begins with the one segment that asks for it, and an echo with its question.
        let begins = match protocol {
            Protocol::Tcp => flags & (SYN | ACK | FIN | RST) == SYN,
            Protocol::Udp => true,
            Protocol::Echo => parsed.target.1 == 0,
        };
        let (way, remote) = routed.filter(|_| begins)?;
        self.outside(way)?;
        let port = self.choose_port(way, protocol, inner.port, remote, now)?;
        let outer = Outer { way, protocol, port, remote };
        let mut flow = Flow { inner, state: 0, last: now, piece: None };
        flow.note(true, flags, now);
        self.flows.insert(outer, flow);
        self.inward.insert(inner, outer);
        self.counts[way as usize][protocol as usize] += 1;
        Some(outer)
    }

    // Notes the first piece of a packet under its flow, in place of the flow's piece before.
    fn mark_piece(&mut self, mark: Mark, outer: Outer) {
        if let Some(flow) = self.flows.get_mut(&outer) {
            if let Some(old) = flow.piece.replace(mark) {
                self.pieces.remove(&old);
            }
            self.pieces.insert(mark, outer);
        }
    }

    fn to_mark(is_out: bool, packet: &[u8], parsed: &Parsed) -> Mark {
        let (protocol, number) = rewrite::piece_mark(packet);
        (is_out, parsed.source.0, parsed.target.0, protocol, number)
    }

    fn send(&mut self, inside: Inside, path: Path, read: Option<Read>, now: Duration) -> Vec<Emit> {
        let Some((mut packet, parsed)) = read else {
            return Vec::new();
        };
        if parsed.source.0 != self.inner_address(inside) {
            return Vec::new();
        }
        let mark = Nat::to_mark(true, &packet, &parsed);
        let outer = match parsed.kind {
            Kind::Flow(..) => self.find_out(inside, path, &parsed, now),
            Kind::Piece(_) => self.pieces.get(&mark).copied(),
            Kind::Error => return self.send_error(inside, packet, &parsed, now),
        };
        let Some((outer, (address, mtu))) = outer.and_then(|outer| Some((outer, self.outside(outer.way)?))) else {
            return Vec::new();
        };
        if packet.len() > mtu && rewrite::is_whole_alone(&packet) {
            // The device's own stack is told its way's largest packet as its interface is set up.
            let told = icmp::too_big(&packet, parsed.head, self.gateway, mtu.min(usize::from(u16::MAX)) as u16);
            return Vec::from_iter((inside == Inside::Host).then_some(Emit { sink: Sink::Host, packet: told }));
        }
        if parsed.piece == Piece::First {
            self.mark_piece(mark, outer);
        }
        rewrite::set_ends(&mut packet, &parsed, (address, outer.port), outer.remote);
        if matches!(parsed.kind, Kind::Flow(Protocol::Tcp, flags) if flags & SYN != 0) {
            rewrite::clamp_segment(&mut packet, parsed.head, mtu);
        }
        let sink = sink_of(outer.way);
        if packet.len() <= mtu {
            return alloc::vec![Emit { sink, packet }];
        }
        rewrite::cut(&packet, parsed.head, mtu).into_iter().map(|packet| Emit { sink, packet }).collect()
    }

    fn take(&mut self, way: Way, read: Option<Read>, now: Duration) -> Vec<Emit> {
        let Some((mut packet, parsed)) = read else {
            return Vec::new();
        };
        let Some((_, mtu)) = self.outside(way).filter(|(address, _)| parsed.target.0 == *address) else {
            return Vec::new();
        };
        let mark = Nat::to_mark(false, &packet, &parsed);
        let outer = match parsed.kind {
            Kind::Flow(protocol, _) => Some(Outer { way, protocol, port: parsed.target.1, remote: parsed.source }),
            Kind::Piece(_) => self.pieces.get(&mark).copied(),
            Kind::Error => return self.take_error(way, packet, &parsed, now),
        };
        let Some((outer, flow)) = outer.and_then(|outer| Some((outer, self.flows.get_mut(&outer)?))) else {
            return Vec::new();
        };
        if !flow.is_alive(now) {
            self.remove(&outer);
            return Vec::new();
        }
        if let Kind::Flow(_, flags) = parsed.kind {
            flow.note(false, flags, now);
        }
        let inner = flow.inner;
        if parsed.piece == Piece::First {
            self.mark_piece(mark, outer);
        }
        rewrite::set_ends(&mut packet, &parsed, inner.seen, (self.inner_address(inner.inside), inner.port));
        if matches!(parsed.kind, Kind::Flow(Protocol::Tcp, flags) if flags & SYN != 0) {
            rewrite::clamp_segment(&mut packet, parsed.head, mtu);
        }
        alloc::vec![Emit { sink: inner_sink(inner.inside), packet }]
    }

    // An error from the inner side about a packet that came to it: it goes on to the far end as the flow's way names both.
    fn send_error(&self, inside: Inside, mut packet: Vec<u8>, parsed: &Parsed, now: Duration) -> Vec<Emit> {
        let found = rewrite::parse_quoted(&packet, parsed.head).and_then(|(protocol, source, target)| {
            let outer = *self.inward.get(&Inner { inside, protocol, port: target.1, seen: source })?;
            let (address, _) = self.outside(outer.way)?;
            self.flows.get(&outer).is_some_and(|flow| flow.is_alive(now)).then_some((outer, address))
        });
        let Some((outer, address)) = found else {
            return Vec::new();
        };
        let quoted = (outer.remote, (address, outer.port));
        rewrite::set_error_ends(&mut packet, parsed.head, (address, outer.remote.0), quoted);
        alloc::vec![Emit { sink: sink_of(outer.way), packet }]
    }

    // An error that came by a way about a packet that left by it: it goes on to the inner side as that names both ends.
    fn take_error(&self, way: Way, mut packet: Vec<u8>, parsed: &Parsed, now: Duration) -> Vec<Emit> {
        let found = rewrite::parse_quoted(&packet, parsed.head).and_then(|(protocol, source, target)| {
            let flow = self.flows.get(&Outer { way, protocol, port: source.1, remote: target })?;
            let is_ours = self.outside(way).is_some_and(|(address, _)| address == source.0);
            (is_ours && flow.is_alive(now)).then_some(flow.inner)
        });
        let Some(inner) = found else {
            return Vec::new();
        };
        let address = self.inner_address(inner.inside);
        rewrite::set_error_ends(
            &mut packet,
            parsed.head,
            (parsed.source.0, address),
            ((address, inner.port), inner.seen),
        );
        alloc::vec![Emit { sink: inner_sink(inner.inside), packet }]
    }

    // Takes a packet the host sent, on the path its packets take.
    pub(crate) fn take_host(&mut self, path: Path, packet: &[u8], now: Duration) -> Vec<Emit> {
        self.send(Inside::Host, path, read(packet), now)
    }

    // Takes a packet the device's own stack sent: what it sends the host is its relay's answer, all else leaves over the modem's connection.
    pub(crate) fn take_own(&mut self, packet: &[u8], now: Duration) -> Vec<Emit> {
        let read = read(packet);
        let is_answer = read.as_ref().is_some_and(|(_, parsed)| parsed.target.0 == self.host);
        if is_answer { self.take(Way::Relay, read, now) } else { self.send(Inside::Own, Path::Lte, read, now) }
    }

    // Takes a packet that came over the modem's connection.
    pub(crate) fn take_lte(&mut self, packet: &[u8], now: Duration) -> Vec<Emit> {
        self.take(Way::Lte, read(packet), now)
    }

    // Takes a packet that came out of the tunnel.
    pub(crate) fn take_tunnel(&mut self, packet: &[u8], now: Duration) -> Vec<Emit> {
        self.take(Way::Tunnel, read(packet), now)
    }
}

// A packet cut to its own length, and what the translation reads of it.
type Read = (Vec<u8>, Parsed);

fn read(packet: &[u8]) -> Option<Read> {
    let packet = to_whole_ipv4(packet)?;
    Some((packet.to_vec(), rewrite::parse(packet)?))
}

#[cfg(test)]
mod tests;
