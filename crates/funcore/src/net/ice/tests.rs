use super::agent::{MOST_CANDIDATES, MOST_CHECKS, MOST_PAIRS, MOST_SIDES, Pair, Status};
use super::*;
use crate::base::math::xorshift64;
use crate::crypto::Primitives;
use crate::net::stun::tests::Exact;
use crate::net::stun::{
    Attribute, BINDING, Class, FINGERPRINT, Frames, Framing, Integrity, MESSAGE_INTEGRITY, Message, ROLE_CONFLICT,
    Writer, is_message, prefixed,
};
use crate::net::turn::tests::{Out, Relay};
use alloc::string::ToString;
use alloc::vec;
use core::cell::Cell;
use core::time::Duration;

const RELAY: &str = "192.0.2.15:3478";
const RELAY_V4: &str = "192.0.2.15";
const RELAY_V6: &str = "2001:db8::15";
const MEDIA: [u8; 12] = [0x80, 0x6F, 0, 1, 0, 0, 0, 2, 0, 0, 0x03, 0xE9];
// The number the calling server is known by as a far side.
const SFU_SIDE: u32 = 7;

#[derive(Clone, Debug)]
enum Signal {
    Credentials(String, String),
    Candidate(String),
    Removed(SocketAddr),
}

#[derive(Debug)]
enum Event {
    Datagram { from: SocketAddr, to: SocketAddr, bytes: Vec<u8> },
    // Stream bytes toward the stream's server, and toward the agent.
    Up { device: usize, socket: Socket, bytes: Vec<u8> },
    Down { device: usize, socket: Socket, bytes: Vec<u8> },
    // What a device signals another, which knows it as the far side `side`.
    Signal { device: usize, side: u32, signal: Signal },
}

// A translator in front of a device: one outside port for each inside address whoever it sends to, and in only what comes from where the inside sent (RFC 4787: endpoint-independent mapping, address- and port-dependent filtering).
struct Nat {
    inside: IpAddr,
    outside: IpAddr,
    mappings: Vec<(SocketAddr, u16)>,
    sent: Vec<(u16, SocketAddr)>,
    next_port: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Server {
    Relay,
    Sfu,
}

struct Link {
    socket: Socket,
    seen: SocketAddr,
    server: Server,
    frames: Frames,
    // A stream closed after a write still carries that write to its server.
    is_open: bool,
}

// A device in the world: its agent and what it was told; the others know it as the far side of its index.
struct Device {
    agent: Agent,
    crypto: Exact,
    ips: Vec<IpAddr>,
    nat: Option<Nat>,
    bound: Vec<(Socket, SocketAddr)>,
    links: Vec<Link>,
    next_port: u16,
    states: Vec<(Duration, u32, State)>,
    paths: Vec<(u32, Option<Path>)>,
    delivered: Vec<(u32, Vec<u8>)>,
    told: Vec<String>,
    lines_delay: Duration,
    credentials_delay: Duration,
}

impl Device {
    fn new(role: Role, seed: u64, ips: &[&str], is_relayed: bool) -> Device {
        let crypto = Exact(Cell::new(seed));
        Device {
            agent: Agent::new(&crypto, role, is_relayed).unwrap(),
            crypto,
            ips: ips.iter().map(|ip| ip.parse().unwrap()).collect(),
            nat: None,
            bound: Vec::new(),
            links: Vec::new(),
            next_port: 40_000,
            states: Vec::new(),
            paths: Vec::new(),
            delivered: Vec::new(),
            told: Vec::new(),
            lines_delay: Duration::from_millis(30),
            credentials_delay: Duration::from_millis(30),
        }
    }

    fn behind(mut self, outside: &str) -> Device {
        self.nat = Some(Nat {
            inside: self.ips[0],
            outside: outside.parse().unwrap(),
            mappings: Vec::new(),
            sent: Vec::new(),
            next_port: 60_000,
        });
        self
    }

    fn path(&self, side: u32) -> Option<Path> {
        self.paths.iter().rev().find(|(of, _)| *of == side).and_then(|(_, path)| *path)
    }

    fn paths_of(&self, side: u32) -> Vec<Option<Path>> {
        self.paths.iter().filter(|(of, _)| *of == side).map(|(_, path)| *path).collect()
    }

    fn states_of(&self, side: u32) -> Vec<State> {
        self.states.iter().filter(|(_, of, _)| *of == side).map(|(_, _, state)| *state).collect()
    }

    // When the far side first stood connected.
    fn connected_at(&self, side: u32) -> Option<Duration> {
        self.states.iter().find(|(_, of, state)| *of == side && *state == State::Connected).map(|(at, ..)| *at)
    }
}

struct World {
    now: Duration,
    seed: u64,
    // Datagrams lost in a thousand, and the most they are held back beyond the least delay, which reorders them.
    loss: u64,
    jitter_ms: u64,
    devices: Vec<Device>,
    relay: Relay,
    sfu: Option<Sfu>,
    events: Vec<(Duration, u64, Event)>,
    order: u64,
    // The Binding requests each device sent, when and where to.
    requests: Vec<(Duration, usize, SocketAddr)>,
    cut: Vec<(IpAddr, IpAddr)>,
    is_sfu_udp_blocked: bool,
    is_sfu_tcp_blocked: bool,
    recorded: Vec<Vec<u8>>,
}

const LEAST_DELAY: Duration = Duration::from_millis(5);
// The messages of a call kept for the fuzzing to mangle: enough of each kind a call sends in its first seconds.
const RECORDED_MOST: usize = 400;

impl World {
    fn new(seed: u64, devices: Vec<Device>) -> World {
        let relay = Relay::new(RELAY.parse().unwrap(), RELAY_V4.parse().unwrap(), RELAY_V6.parse().unwrap());
        World {
            now: Duration::ZERO,
            seed,
            loss: 0,
            jitter_ms: 1,
            devices,
            relay,
            sfu: None,
            events: Vec::new(),
            order: 0,
            requests: Vec::new(),
            cut: Vec::new(),
            is_sfu_udp_blocked: false,
            is_sfu_tcp_blocked: false,
            recorded: Vec::new(),
        }
    }

    fn draw(&mut self) -> u64 {
        self.seed = xorshift64(self.seed);
        self.seed
    }

    fn push(&mut self, at: Duration, event: Event) {
        self.order += 1;
        self.events.push((at, self.order, event));
    }

    // Who a device signals: the first device every other, the others the first alone (a caller and the devices of its callee).
    fn partners(&self, device: usize) -> Vec<usize> {
        if device == 0 { (1..self.devices.len()).collect() } else { vec![0] }
    }

    // A device's credentials to the first device, and the first's to it, each after its delay.
    fn introduce(&mut self, device: usize) {
        let caller = &self.devices[0].agent;
        let offer = Signal::Credentials(caller.ufrag().to_string(), caller.pwd().to_string());
        let at = self.now + self.devices[0].credentials_delay;
        self.push(at, Event::Signal { device, side: 0, signal: offer });
        let callee = &self.devices[device].agent;
        let answer = Signal::Credentials(callee.ufrag().to_string(), callee.pwd().to_string());
        let at = self.now + self.devices[device].credentials_delay;
        self.push(at, Event::Signal { device: 0, side: device as u32, signal: answer });
    }

    // The devices' addresses taken, the relays handed over where wanted, and every pair of partners introduced.
    fn start(&mut self, relays: &[(usize, Transport)]) {
        for device in 0..self.devices.len() {
            let ips = self.devices[device].ips.clone();
            for ip in ips {
                self.devices[device].agent.local(ip);
            }
        }
        for &(device, transport) in relays {
            let server = self.relay.server(transport);
            self.devices[device].agent.add_relay(server);
        }
        for device in 1..self.devices.len() {
            self.introduce(device);
        }
    }

    // A device that answers later: its addresses taken, the first device's credentials and lines told it, and its own credentials to the first.
    fn join(&mut self, device: Device) {
        self.devices.push(device);
        let index = self.devices.len() - 1;
        let ips = self.devices[index].ips.clone();
        for ip in ips {
            self.devices[index].agent.local(ip);
        }
        self.introduce(index);
        let at = self.now + self.devices[0].lines_delay;
        for line in self.devices[0].told.clone() {
            self.push(at, Event::Signal { device: index, side: 0, signal: Signal::Candidate(line) });
        }
    }

    // Has every device do its steps until none has more at this time.
    fn settle(&mut self) {
        loop {
            let mut is_busy = false;
            for device in 0..self.devices.len() {
                let now = self.now;
                loop {
                    let Device { agent, crypto, .. } = &mut self.devices[device];
                    let Some(step) = agent.next(crypto, now) else { break };
                    is_busy = true;
                    self.perform(device, step);
                }
            }
            if !is_busy {
                return;
            }
        }
    }

    fn perform(&mut self, device: usize, step: Step) {
        let now = self.now;
        match step {
            Step::Bind { socket, local } => {
                let port = self.devices[device].next_port;
                self.devices[device].next_port += 1;
                let address = SocketAddr::new(local, port);
                self.devices[device].bound.push((socket, address));
                let Device { agent, crypto, .. } = &mut self.devices[device];
                agent.take_bound(crypto, socket, Some(address), now);
            }
            Step::Connect { socket, local, far, name } => {
                let sfu = self.sfu.as_ref();
                let server = if far == self.relay.address {
                    Some(Server::Relay)
                } else if let Some(sfu) = sfu {
                    if sfu.tls.contains(&far) {
                        assert_eq!(name.as_deref(), Some(SFU_NAME), "a TLS stream checked by the server's name");
                        Some(Server::Sfu)
                    } else {
                        (sfu.tcp.contains(&far) && !self.is_sfu_tcp_blocked).then_some(Server::Sfu)
                    }
                } else {
                    None
                };
                let port = self.devices[device].next_port;
                self.devices[device].next_port += 1;
                let seen = SocketAddr::new(local, port);
                let Some(server) = server else {
                    let Device { agent, crypto, .. } = &mut self.devices[device];
                    return agent.take_connected(crypto, socket, None, now);
                };
                let framing = if server == Server::Relay { Framing::Messages } else { Framing::Prefixed };
                self.devices[device].links.push(Link {
                    socket,
                    seen,
                    server,
                    frames: Frames::new(framing),
                    is_open: true,
                });
                let Device { agent, crypto, .. } = &mut self.devices[device];
                agent.take_connected(crypto, socket, Some(seen), now);
            }
            Step::Send { socket, to, datagram } => {
                let Some(&(_, from)) = self.devices[device].bound.iter().find(|(bound, _)| *bound == socket) else {
                    return;
                };
                if Message::parse(&datagram)
                    .is_some_and(|message| message.class == Class::Request && message.method == BINDING)
                {
                    self.requests.push((now, device, to));
                }
                self.route(Some(device), from, to, datagram);
            }
            Step::Write { socket, bytes } => {
                for piece in self.split(&bytes) {
                    let at = now + LEAST_DELAY;
                    self.push(at, Event::Up { device, socket, bytes: piece });
                }
            }
            Step::Close(socket) => {
                self.devices[device].bound.retain(|(bound, _)| *bound != socket);
                self.devices[device]
                    .links
                    .iter_mut()
                    .filter(|link| link.socket == socket)
                    .for_each(|link| link.is_open = false);
            }
            Step::Candidate(line) => {
                self.devices[device].told.push(line.clone());
                let at = now + self.devices[device].lines_delay;
                for partner in self.partners(device) {
                    let signal = Signal::Candidate(line.clone());
                    self.push(at, Event::Signal { device: partner, side: device as u32, signal });
                }
            }
            Step::Removed(address) => {
                let at = now + self.devices[device].lines_delay;
                for partner in self.partners(device) {
                    let signal = Signal::Removed(address);
                    self.push(at, Event::Signal { device: partner, side: device as u32, signal });
                }
            }
            Step::Selected { side, path } => self.devices[device].paths.push((side, path)),
            Step::State { side, state } => self.devices[device].states.push((now, side, state)),
            Step::Deliver { side, packet } => self.devices[device].delivered.push((side, packet)),
            Step::Log(line) => {
                let is_address = |word: &str| {
                    let word = word.trim_end_matches(',');
                    word.parse::<IpAddr>().is_ok() || word.parse::<SocketAddr>().is_ok()
                };
                assert!(
                    line.starts_with("ice: ") && !line.split(' ').any(is_address),
                    "no address in a log line: {line}"
                )
            }
        }
    }

    // Pieces of a stream's bytes, cut anywhere.
    fn split(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut pieces = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let length = 1 + (self.draw() % 13) as usize;
            let end = (at + length).min(bytes.len());
            pieces.push(bytes[at..end].to_vec());
            at = end;
        }
        pieces
    }

    // Sends a datagram: through the sender's translator, lost or held back as the network is.
    fn route(&mut self, device: Option<usize>, mut from: SocketAddr, to: SocketAddr, bytes: Vec<u8>) {
        if let Some(nat) =
            device.and_then(|device| self.devices[device].nat.as_mut()).filter(|nat| nat.inside == from.ip())
        {
            let port = match nat.mappings.iter().find(|(inside, _)| *inside == from) {
                Some((_, port)) => *port,
                None => {
                    nat.next_port += 1;
                    nat.mappings.push((from, nat.next_port));
                    nat.next_port
                }
            };
            if !nat.sent.contains(&(port, to)) {
                nat.sent.push((port, to));
            }
            from = SocketAddr::new(nat.outside, port);
        }
        let is_cut = self
            .cut
            .iter()
            .any(|(one, other)| (*one, *other) == (from.ip(), to.ip()) || (*one, *other) == (to.ip(), from.ip()));
        let sfu_udp = self.sfu.as_ref().is_some_and(|sfu| sfu.udp.contains(&to) || sfu.udp.contains(&from));
        if is_cut || (sfu_udp && self.is_sfu_udp_blocked) || self.draw() % 1000 < self.loss {
            return;
        }
        let at = self.now + LEAST_DELAY + Duration::from_millis(self.draw() % self.jitter_ms.max(1));
        self.push(at, Event::Datagram { from, to, bytes });
    }

    fn deliver(&mut self, event: Event) {
        let now = self.now;
        match event {
            Event::Datagram { from, to, bytes } => {
                if is_message(&bytes) && self.recorded.len() < RECORDED_MOST {
                    self.recorded.push(bytes.clone());
                }
                for device in 0..self.devices.len() {
                    let mut to = to;
                    if let Some(nat) = &self.devices[device].nat
                        && nat.outside == to.ip()
                    {
                        let Some((inside, port)) = nat.mappings.iter().find(|(_, port)| *port == to.port()) else {
                            return;
                        };
                        if !nat.sent.contains(&(*port, from)) {
                            return;
                        }
                        to = *inside;
                    }
                    if let Some(&(socket, _)) = self.devices[device].bound.iter().find(|(_, address)| *address == to) {
                        let Device { agent, crypto, .. } = &mut self.devices[device];
                        return agent.take_datagram(crypto, socket, from, &bytes, now);
                    }
                }
                if to == self.relay.address {
                    let outs = self.relay.take(from, false, &bytes, now);
                    return self.relay_out(outs);
                }
                if self.relay.is_relayed(to) {
                    let outs = self.relay.take_peer(to, from, &bytes, now);
                    return self.relay_out(outs);
                }
                if let Some(sfu) = self.sfu.as_mut()
                    && sfu.udp.contains(&to)
                {
                    let replies = sfu.take(Locator::Udp { local: to, peer: from }, &bytes, now);
                    self.sfu_out(replies);
                }
            }
            Event::Up { device, socket, bytes } => {
                let Some(link) = self.devices[device].links.iter_mut().find(|link| link.socket == socket) else {
                    return;
                };
                let (seen, server) = (link.seen, link.server);
                let frames = link.frames.take(&bytes).expect("this side frames its stream");
                for frame in frames {
                    match server {
                        Server::Relay => {
                            let outs = self.relay.take(seen, true, &frame, now);
                            self.relay_out(outs);
                        }
                        Server::Sfu => {
                            let sfu = self.sfu.as_mut().unwrap();
                            let replies = sfu.take(Locator::Tcp { device, socket }, &frame, now);
                            self.sfu_out(replies);
                        }
                    }
                }
            }
            Event::Down { device, socket, bytes } => {
                if self.devices[device].links.iter().any(|link| link.socket == socket && link.is_open) {
                    let Device { agent, crypto, .. } = &mut self.devices[device];
                    agent.take_stream(crypto, socket, &bytes, now);
                }
            }
            Event::Signal { device, side, signal } => {
                let agent = &mut self.devices[device].agent;
                match signal {
                    Signal::Credentials(ufrag, pwd) => agent.far_credentials(side, &ufrag, &pwd),
                    Signal::Candidate(line) => agent.far_candidate(side, &line),
                    Signal::Removed(address) => agent.drop_far(side, address),
                }
            }
        }
    }

    fn relay_out(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Client(client, bytes) => {
                    let link = self.devices.iter().enumerate().find_map(|(device, one)| {
                        one.links
                            .iter()
                            .find(|link| link.seen == client && link.server == Server::Relay)
                            .map(|link| (device, link.socket))
                    });
                    match link {
                        Some((device, socket)) => {
                            for piece in self.split(&bytes) {
                                let at = self.now + LEAST_DELAY;
                                self.push(at, Event::Down { device, socket, bytes: piece });
                            }
                        }
                        None => {
                            let from = self.relay.address;
                            self.route(None, from, client, bytes);
                        }
                    }
                }
                Out::Peer { from, to, data } => self.route(None, from, to, data),
            }
        }
    }

    fn sfu_out(&mut self, replies: Vec<(Locator, Vec<u8>)>) {
        for (locator, bytes) in replies {
            match locator {
                Locator::Udp { local, peer } => self.route(None, local, peer, bytes),
                Locator::Tcp { device, socket } => {
                    for piece in self.split(&prefixed(&bytes).unwrap()) {
                        let at = self.now + LEAST_DELAY;
                        self.push(at, Event::Down { device, socket, bytes: piece });
                    }
                }
            }
        }
    }

    // Runs the network and the devices until `until`.
    fn run(&mut self, until: Duration) {
        let mut stalls = 0;
        loop {
            self.settle();
            let now = self.now;
            let event = self.events.iter().map(|(at, ..)| *at).min();
            let waits = self.devices.iter().filter_map(|device| device.agent.next_wait(now).map(|wait| now + wait));
            let sfu = self.sfu.as_ref().and_then(|sfu| sfu.next_due());
            let Some(next) = event.into_iter().chain(waits).chain(sfu).min() else { break };
            if next > until {
                break;
            }
            stalls = if next <= now && event.is_none_or(|event| event > now) { stalls + 1 } else { 0 };
            assert!(stalls < 1000, "something is due at {now:?} and is never done");
            self.now = next.max(now);
            loop {
                let due = self
                    .events
                    .iter()
                    .enumerate()
                    .filter(|(_, (at, ..))| *at <= self.now)
                    .min_by_key(|(_, (at, order, _))| (*at, *order));
                let Some((at, _)) = due else { break };
                let (_, _, event) = self.events.remove(at);
                self.deliver(event);
            }
            if let Some(sfu) = self.sfu.as_mut() {
                let pings = sfu.tick(self.now);
                self.sfu_out(pings);
            }
        }
        self.now = until;
    }

    // Has a device send media to one of its far sides over the pair selected for it.
    fn media(&mut self, device: usize, side: u32, packet: &[u8]) {
        let now = self.now;
        let Device { agent, crypto, .. } = &mut self.devices[device];
        agent.send(crypto, side, packet.to_vec(), now);
        self.settle();
    }

    fn assert_bounded(&self) {
        for device in &self.devices {
            let agent = &device.agent;
            assert!(
                agent.pairs.len() <= MOST_PAIRS
                    && agent.fars.len() <= MOST_CANDIDATES
                    && agent.owns.len() <= MOST_CANDIDATES
                    && agent.checks.len() <= MOST_CHECKS
                    && agent.sides.len() <= MOST_SIDES
                    && agent.sides.iter().all(|side| side.triggered.len() <= MOST_PAIRS),
                "tables within their bounds"
            );
        }
    }
}

fn second(count: u64) -> Duration {
    Duration::from_secs(count)
}

fn pair_of(world: &mut World) {
    world.start(&[]);
    world.run(second(5));
}

// True where every device stands connected to each of its partners.
fn connected(world: &World) -> bool {
    (0..world.devices.len()).all(|device| {
        world
            .partners(device)
            .iter()
            .all(|partner| world.devices[device].agent.state(*partner as u32) == Some(State::Connected))
    })
}

// Media both ways between the first device and the second.
fn assert_media(world: &mut World) {
    let (before_a, before_b) = (world.devices[0].delivered.len(), world.devices[1].delivered.len());
    world.media(0, 1, &MEDIA);
    world.media(1, 0, &[0x81; 40]);
    let until = world.now + second(1);
    world.run(until);
    assert!(world.devices[1].delivered[before_b..].contains(&(0, MEDIA.to_vec())), "media from the first device");
    assert!(world.devices[0].delivered[before_a..].contains(&(1, vec![0x81; 40])), "and from the second");
}

// A packet of media a device sends, which tells who sent it to whom.
fn tagged(from: usize, to: u32) -> Vec<u8> {
    vec![0x80, 0x60, from as u8, to as u8, 0, 0, 0, 1]
}

// Media between the first device and each of the others that `sides` names, each packet at the side it was sent to alone.
fn assert_forked_media(world: &mut World, sides: &[u32]) {
    let before: Vec<usize> = world.devices.iter().map(|device| device.delivered.len()).collect();
    for &side in sides {
        world.media(0, side, &tagged(0, side));
        world.media(side as usize, 0, &tagged(side as usize, 0));
    }
    let until = world.now + second(1);
    world.run(until);
    for &side in sides {
        let got = &world.devices[side as usize].delivered[before[side as usize]..];
        assert_eq!(got, [(0, tagged(0, side))], "device {side} has the packet sent to it and no other");
    }
    let got = &world.devices[0].delivered[before[0]..];
    for &side in sides {
        assert!(got.contains(&(side, tagged(side as usize, 0))), "device {side}'s packet at its side: {got:?}");
    }
    assert!(
        got.iter().all(|(side, packet)| *packet == tagged(*side as usize, 0)),
        "every packet at the side that sent it: {got:?}"
    );
}

#[test]
fn host_pairs() {
    for (first, second_role) in [(Role::Controlling, Role::Controlled), (Role::Controlled, Role::Controlling)] {
        let mut world = World::new(
            11,
            vec![
                Device::new(first, 1, &["198.51.100.10", "2001:db8:a::10"], false),
                Device::new(second_role, 2, &["203.0.113.20", "2001:db8:b::20"], false),
            ],
        );
        pair_of(&mut world);
        assert!(
            connected(&world),
            "both devices connected: {:?} {:?}",
            world.devices[0].states,
            world.devices[1].states
        );
        for (device, side) in [(0, 1), (1, 0)] {
            let path = world.devices[device].path(side).expect("a selected pair");
            assert_eq!((path.local, path.far, path.transport), (Kind::Host, Kind::Host, Transport::Udp));
            assert!(!path.is_relayed());
        }
        assert_media(&mut world);
        let start = world.now;
        world.run(start + second(30));
        let sent = world.requests.iter().filter(|(at, device, _)| *at > start && *device == 0).count();
        assert!(sent <= 8, "once connected only checks of consent go, every 4 to 6 s: {sent}");
        world.assert_bounded();
    }
}

const ROLES: [(Role, Role); 2] = [(Role::Controlling, Role::Controlled), (Role::Controlled, Role::Controlling)];

#[test]
fn trickle_orders() {
    let ms = Duration::from_millis;
    // How long each device's credentials and its lines take to the other: credentials first, lines first, one device long after.
    let orders =
        [(ms(10), ms(400), ms(10), ms(400)), (ms(600), ms(5), ms(600), ms(5)), (ms(20), ms(30), ms(1500), ms(2500))];
    for (first, second_role) in ROLES {
        for (seed, (credentials_a, lines_a, credentials_b, lines_b)) in orders.into_iter().enumerate() {
            let mut a = Device::new(first, 3 + seed as u64, &["198.51.100.10"], false);
            let mut b = Device::new(second_role, 30 + seed as u64, &["203.0.113.20"], false);
            (a.credentials_delay, a.lines_delay, b.credentials_delay, b.lines_delay) =
                (credentials_a, lines_a, credentials_b, lines_b);
            let mut world = World::new(17 + seed as u64, vec![a, b]);
            pair_of(&mut world);
            assert!(
                connected(&world),
                "order {seed}, {first:?} first: {:?} {:?}",
                world.devices[0].states,
                world.devices[1].states
            );
            assert_media(&mut world);
        }
        let mut world = World::new(
            5,
            vec![Device::new(first, 5, &["198.51.100.10"], false), Device::new(second_role, 6, &[], false)],
        );
        world.start(&[]);
        world.run(second(2));
        assert_eq!(world.devices[0].agent.state(1), Some(State::Connecting), "nothing to pair yet");
        world.devices[1].agent.local("203.0.113.20".parse().unwrap());
        world.run(second(6));
        assert!(connected(&world), "an address of the far device taken late");
    }
}

#[test]
fn nat_srflx() {
    for (first, second_role) in ROLES {
        let mut world = World::new(
            23,
            vec![
                Device::new(first, 7, &["198.51.100.10"], false),
                Device::new(second_role, 8, &["10.0.0.2"], false).behind("198.51.100.99"),
            ],
        );
        world.devices[1].agent.add_stun(RELAY.parse().unwrap());
        pair_of(&mut world);
        assert!(connected(&world), "{:?} {:?}", world.devices[0].states, world.devices[1].states);
        assert!(
            world.devices[1].told.iter().any(|line| line.contains("typ srflx raddr 10.0.0.2")),
            "its reflexive candidate told"
        );
        assert_eq!(
            world.devices[0].path(1).map(|path| path.far),
            Some(Kind::Srflx),
            "reached at its reflexive address"
        );
        assert_media(&mut world);
    }
}

#[test]
fn nat_prflx() {
    for (first, second_role) in ROLES {
        let mut world = World::new(
            29,
            vec![
                Device::new(first, 9, &["198.51.100.10"], false),
                Device::new(second_role, 10, &["10.0.0.2"], false).behind("198.51.100.99"),
            ],
        );
        pair_of(&mut world);
        assert!(connected(&world), "{:?} {:?}", world.devices[0].states, world.devices[1].states);
        assert_eq!(world.devices[0].path(1).map(|path| path.far), Some(Kind::Prflx), "learned from the far check");
        let learned = world.devices[1].agent.owns.iter().any(|own| own.candidate.kind == Kind::Prflx && !own.is_told);
        assert!(learned, "and the translated device learned its own address from an answer, told to nobody");
        assert_media(&mut world);
    }
}

#[test]
fn relayed_lossy() {
    for (first, second_role) in ROLES {
        for (transport, seed) in [(Transport::Udp, 41), (Transport::Tcp, 43), (Transport::Tls, 47)] {
            let mut world = World::new(
                seed,
                vec![
                    Device::new(first, seed + 1, &["198.51.100.10"], true),
                    Device::new(second_role, seed + 2, &["203.0.113.20"], true),
                ],
            );
            (world.loss, world.jitter_ms) = (100, 60);
            world.start(&[(0, transport), (1, Transport::Udp)]);
            world.run(second(20));
            assert!(connected(&world), "{transport:?}: {:?} {:?}", world.devices[0].states, world.devices[1].states);
            for (device, side) in [(0, 1), (1, 0)] {
                assert!(
                    world.devices[device].told.iter().all(|line| line.contains("typ relay raddr 0.0.0.0 rport 0")),
                    "relayed candidates alone, telling nothing of this device"
                );
                let path = world.devices[device].path(side);
                assert!(path.is_some_and(|path| path.is_relayed() && path.local == Kind::Relay), "{path:?}");
            }
            world.loss = 0;
            assert_media(&mut world);
            world.assert_bounded();
        }
    }
}

#[test]
fn loss_reorder() {
    for (first, second_role) in ROLES {
        let mut world = World::new(
            53,
            vec![
                Device::new(first, 11, &["198.51.100.10", "2001:db8:a::10"], false),
                Device::new(second_role, 12, &["203.0.113.20", "2001:db8:b::20"], false),
            ],
        );
        (world.loss, world.jitter_ms) = (250, 120);
        world.start(&[(0, Transport::Udp), (1, Transport::Udp)]);
        world.run(second(30));
        assert!(connected(&world), "{:?} {:?}", world.devices[0].states, world.devices[1].states);
        world.loss = 0;
        assert_media(&mut world);
        world.assert_bounded();
    }
}

#[test]
fn consent_cadence() {
    let mut world = World::new(
        57,
        vec![
            Device::new(Role::Controlling, 31, &["198.51.100.10"], false),
            Device::new(Role::Controlled, 32, &["203.0.113.20"], false),
        ],
    );
    pair_of(&mut world);
    let far = world.devices[1].bound[0].1;
    let start = world.now;
    world.run(second(125));
    let times: Vec<Duration> = world
        .requests
        .iter()
        .filter(|(at, device, to)| *at > start && *device == 0 && *to == far)
        .map(|(at, ..)| *at)
        .collect();
    assert!(times.len() >= 20, "a check of consent every few seconds: {}", times.len());
    for gap in times.windows(2).map(|pair| pair[1] - pair[0]) {
        assert!(
            gap >= second(4) && gap <= second(6),
            "0.8 to 1.2 of five seconds apart: {gap:?} {times:?} {:?}",
            world.requests
        );
    }
    assert!(connected(&world), "consent kept");
}

// Drains an agent's steps at `now`: the sockets it opens bound at once, the rest kept.
fn drain(agent: &mut Agent, crypto: &Exact, now: Duration) -> Vec<Step> {
    let mut steps = Vec::new();
    while let Some(step) = agent.next(crypto, now) {
        if let Step::Bind { socket, local } = step {
            agent.take_bound(crypto, socket, Some(SocketAddr::new(local, 40_000 + socket.0 as u16)), now);
        } else {
            steps.push(step);
        }
    }
    steps
}

#[test]
fn server_answers() {
    let crypto = Exact(Cell::new(37));
    let mut agent = Agent::new(&crypto, Role::Controlled, false).unwrap();
    agent.local("198.51.100.10".parse().unwrap());
    drain(&mut agent, &crypto, Duration::ZERO);
    let socket = agent.locals[0].socket.unwrap();
    let from: SocketAddr = "203.0.113.20:5000".parse().unwrap();
    let ours = format!("{}:Far1", agent.ufrag());
    let pwd = agent.pwd().to_string();
    let ask = |agent: &mut Agent, user: &str, extra: &[Attribute<'_>], key: Option<&str>, has_fingerprint: bool| {
        let mut writer = Writer::new(BINDING, Class::Request, &[4; 12]);
        writer.push(Attribute::Username(user.as_bytes())).push(Attribute::Priority(1_862_270_975));
        for attribute in extra {
            writer.push(*attribute);
        }
        if let Some(key) = key {
            writer.seal(&crypto, Integrity::Sha1(key.as_bytes()));
        }
        agent.take_datagram(&crypto, socket, from, &writer.finish(has_fingerprint), Duration::ZERO);
        let steps = drain(agent, &crypto, Duration::ZERO);
        steps.into_iter().find_map(|step| match step {
            Step::Send { to, datagram, .. }
                if to == from && Message::parse(&datagram).is_some_and(|message| message.class != Class::Request) =>
            {
                Some(datagram)
            }
            _ => None,
        })
    };
    let read = |bytes: &Option<Vec<u8>>| {
        bytes.as_deref().and_then(Message::parse).map(|message| (message.class, message.error, message.is_sealed()))
    };
    assert_eq!(
        read(&ask(&mut agent, &ours, &[], None, true)),
        Some((Class::Error, Some(400), false)),
        "no integrity: 400, unsealed"
    );
    assert_eq!(
        read(&ask(&mut agent, "xxxx:Far1", &[], Some(&pwd), true)),
        Some((Class::Error, Some(401), false)),
        "another user: 401"
    );
    assert_eq!(
        read(&ask(&mut agent, &ours, &[], Some("wrong password of 22 b"), true)),
        Some((Class::Error, Some(401), false))
    );
    assert_eq!(ask(&mut agent, &ours, &[], Some(&pwd), false), None, "no fingerprint: silence (RFC 8445, 7.3)");
    let unknown = ask(&mut agent, &ours, &[Attribute::Raw(0x0031, &[1])], Some(&pwd), true).unwrap();
    let message = Message::parse(&unknown).unwrap();
    assert_eq!(
        (message.error, message.unknown.clone()),
        (Some(420), vec![0x0031]),
        "an attribute it must understand and does not: 420"
    );
    assert!(message.verify(&crypto, Integrity::Sha1(pwd.as_bytes())) && message.has_fingerprint);
    let conflict = ask(&mut agent, &ours, &[Attribute::Controlled(u64::MAX)], Some(&pwd), true);
    assert_eq!(
        read(&conflict),
        Some((Class::Error, Some(487), true)),
        "both controlled, the far side's tie breaker the larger: 487"
    );
    assert_eq!(agent.role, Role::Controlled);
    let good = ask(&mut agent, &ours, &[Attribute::Controlling(1), Attribute::UseCandidate], Some(&pwd), true).unwrap();
    let message = Message::parse(&good).unwrap();
    assert_eq!(
        (message.class, message.mapped, message.username),
        (Class::Success, Some(from), None),
        "the source mapped, no USERNAME (RFC 8489, 9.1.3)"
    );
    assert!(message.verify(&crypto, Integrity::Sha1(pwd.as_bytes())) && message.has_fingerprint);
    assert!(
        agent.fars.iter().any(|far| far.candidate.address == from && far.candidate.kind == Kind::Prflx),
        "a peer-reflexive candidate learned"
    );
    let named = &agent.sides[0];
    assert_eq!(
        (agent.sides.len(), named.number, named.ufrag.as_deref(), named.triggered.len()),
        (1, None, Some("Far1"), 1),
        "a side known by the fragment alone, its check back waiting for its password"
    );
}

#[test]
fn check_pacing() {
    let crypto = Exact(Cell::new(41));
    let mut agent = Agent::new(&crypto, Role::Controlling, false).unwrap();
    agent.local("198.51.100.10".parse().unwrap());
    agent.far_credentials(1, "Far1", "aFarPasswordOf22Chars0");
    for port in 0..4 {
        agent.far_candidate(
            1,
            &format!("candidate:{port} 1 udp {} 203.0.113.20 {} typ host", 2_000_000_000 - port, 5000 + port),
        );
    }
    let mut now = Duration::ZERO;
    let mut sends: Vec<(Duration, [u8; 12])> = Vec::new();
    let mut states = Vec::new();
    let mut stalls = 0;
    while now < second(200) {
        for step in drain(&mut agent, &crypto, now) {
            match step {
                Step::Send { datagram, .. } => sends.push((now, Message::parse(&datagram).unwrap().transaction)),
                Step::State { side, state } => states.push((side, state, now)),
                _ => {}
            }
        }
        let Some(wait) = agent.next_wait(now) else { break };
        stalls = if wait.is_zero() { stalls + 1 } else { 0 };
        assert!(stalls < 100, "something is due at {now:?} and is never done");
        now += wait;
    }
    let mut firsts: Vec<Duration> = Vec::new();
    let mut transactions: Vec<[u8; 12]> = Vec::new();
    for (at, transaction) in &sends {
        if !transactions.contains(transaction) {
            transactions.push(*transaction);
            firsts.push(*at);
        }
    }
    assert_eq!(transactions.len(), 4, "a check of each pair");
    assert!(firsts.windows(2).all(|pair| pair[1] - pair[0] >= Duration::from_millis(50)), "paced at Ta: {firsts:?}");
    for transaction in &transactions {
        let times: Vec<Duration> = sends.iter().filter(|(_, sent)| sent == transaction).map(|(at, _)| *at).collect();
        assert_eq!(times.len(), 7, "seven requests of a check (RFC 8489, 6.2.1)");
        let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(
            gaps[0] >= Duration::from_millis(500) && gaps.windows(2).all(|pair| pair[1] == 2 * pair[0]),
            "doubling from at least 500 ms: {gaps:?}"
        );
    }
    assert!(agent.pairs.iter().all(|pair| pair.status == super::agent::Status::Failed), "each pair failed");
    let (side, state, at) = states.last().copied().unwrap();
    assert_eq!((side, state), (1, State::Failed), "and the far side, nothing left to check");
    let last_fail = sends.last().unwrap().0;
    assert!(at > last_fail + second(30), "for as long as consent lasts after the last check failed");
}

#[test]
fn consent_loss_relay() {
    let mut world = World::new(
        59,
        vec![
            Device::new(Role::Controlling, 13, &["198.51.100.10"], false),
            Device::new(Role::Controlled, 14, &["203.0.113.20"], false),
        ],
    );
    world.start(&[(0, Transport::Udp), (1, Transport::Udp)]);
    world.run(second(10));
    assert!(connected(&world));
    let paths = [world.devices[0].path(1), world.devices[1].path(0)];
    assert!(
        paths.iter().all(|path| path.is_some_and(|path| path.local == Kind::Host && path.far == Kind::Host)),
        "host to host while the way between the hosts is open: {paths:?}"
    );
    world.cut.push(("198.51.100.10".parse().unwrap(), "203.0.113.20".parse().unwrap()));
    world.run(second(39));
    assert_eq!(world.devices[0].agent.state(1), Some(State::Connected), "consent lasts thirty seconds");
    world.run(second(50));
    let controlling = &world.devices[0];
    assert!(
        controlling.states_of(1).contains(&State::Disconnected) && controlling.paths_of(1).contains(&None),
        "consent lost, and no pair between: {:?} {:?}",
        controlling.states,
        controlling.paths
    );
    // The controlled device follows the new nomination, which may come before its own consent ran out.
    for (device, side) in [(0, 1), (1, 0)] {
        let one = &world.devices[device];
        assert_eq!(one.agent.state(side), Some(State::Connected), "a relayed pair selected after: {:?}", one.states);
        assert!(one.path(side).is_some_and(|path| path.is_relayed()), "{:?}", one.paths);
    }
    assert_media(&mut world);
}

#[test]
fn consent_loss_failure() {
    let mut world = World::new(
        61,
        vec![
            Device::new(Role::Controlling, 15, &["198.51.100.10"], false),
            Device::new(Role::Controlled, 16, &["203.0.113.20"], false),
        ],
    );
    pair_of(&mut world);
    assert!(connected(&world));
    world.loss = 1000;
    world.run(second(40));
    assert_eq!(world.devices[0].agent.state(1), Some(State::Disconnected), "consent lost, still trying");
    world.media(0, 1, &MEDIA);
    world.run(second(150));
    for (device, side) in [(0, 1), (1, 0)] {
        assert_eq!(
            world.devices[device].states_of(side),
            [State::Connected, State::Disconnected, State::Failed],
            "then failed"
        );
    }
    world.loss = 0;
    let before = world.devices[1].delivered.len();
    world.media(0, 1, &MEDIA);
    world.run(second(151));
    assert_eq!(world.devices[1].delivered.len(), before, "no media without consent");
}

#[test]
fn role_conflicts() {
    for role in [Role::Controlling, Role::Controlled] {
        let mut world = World::new(
            67,
            vec![Device::new(role, 17, &["198.51.100.10"], false), Device::new(role, 18, &["203.0.113.20"], false)],
        );
        pair_of(&mut world);
        assert!(connected(&world), "both {role:?}: {:?} {:?}", world.devices[0].states, world.devices[1].states);
        assert_ne!(world.devices[0].agent.role, world.devices[1].agent.role, "one switched");
        assert_media(&mut world);
    }
}

#[test]
fn conflict_leaves_the_role_sent() {
    const PWD: &str = "aFarPasswordOf22Chars0";
    for (sent, is_switched_meanwhile) in
        [(Role::Controlling, false), (Role::Controlling, true), (Role::Controlled, false), (Role::Controlled, true)]
    {
        let other = if sent == Role::Controlling { Role::Controlled } else { Role::Controlling };
        let crypto = Exact(Cell::new(43));
        let mut agent = Agent::new(&crypto, sent, false).unwrap();
        agent.local("198.51.100.10".parse().unwrap());
        agent.far_credentials(1, "Far1", PWD);
        agent.far_candidate(1, "candidate:1 1 udp 2000000000 203.0.113.20 5000 typ host");
        let check = drain(&mut agent, &crypto, Duration::ZERO).into_iter().find_map(|step| match step {
            Step::Send { socket, to, datagram } => Some((socket, to, Message::parse(&datagram)?.transaction)),
            _ => None,
        });
        let (socket, far, transaction) = check.expect("a check of the pair");
        let tiebreaker = agent.tiebreaker;
        // A check of the far side's that came before the answer has had this side switch already.
        if is_switched_meanwhile {
            agent.role = other;
        }
        let mut writer = Writer::new(BINDING, Class::Error, &transaction);
        writer.push(Attribute::Error(ROLE_CONFLICT)).seal(&crypto, Integrity::Sha1(PWD.as_bytes()));
        agent.take_datagram(&crypto, socket, far, &writer.finish(true), Duration::ZERO);
        let pair = &agent.pairs[0];
        assert_eq!(
            (agent.role, pair.status, agent.sides[0].triggered.contains(&pair.id)),
            (other, Status::Waiting, true),
            "sent {sent:?}, switched meanwhile {is_switched_meanwhile}: the other role than the check carried, and the pair checked again"
        );
        assert_ne!(agent.tiebreaker, tiebreaker, "with a new tie breaker");
    }
}

#[test]
fn closed_stream_ends_its_nomination() {
    let crypto = Exact(Cell::new(47));
    let mut agent = Agent::new(&crypto, Role::Controlling, false).unwrap();
    let (side, socket) = (agent.side_of(1).unwrap(), agent.socket());
    let pair = |id: u32, status: Status| Pair {
        id,
        side,
        local: 1,
        far: id,
        priority: u64::from(id),
        foundation: ("1".into(), id.to_string()),
        status,
        valid: Some(1),
        nominate: true,
        heard: None,
        seen: 0,
        consent: None,
        consent_next: Duration::ZERO,
        is_spent: false,
        stream: Some(socket),
    };
    agent.pairs.extend([pair(8, Status::Succeeded), pair(9, Status::Progress)]);
    agent.stream_closed(socket);
    let left: Vec<(Status, bool, Option<Socket>)> =
        agent.pairs.iter().map(|pair| (pair.status, pair.nominate, pair.stream)).collect();
    assert_eq!(
        left,
        [(Status::Failed, false, None); 2],
        "a pair its stream's end failed is nominated no more, so that the side's next best pair is"
    );
}

// A check as libwebrtc sends it when it nominates anew: USE-CANDIDATE where `is_used`, and its own count of nominations.
fn libwebrtc_check(from: &Agent, to: &Agent, nomination: u32, is_used: bool, crypto: &Exact) -> Vec<u8> {
    let username = format!("{}:{}", to.ufrag(), from.ufrag());
    let mut transaction = [0u8; 12];
    crypto.random(&mut transaction);
    let mut writer = Writer::new(BINDING, Class::Request, &transaction);
    writer
        .push(Attribute::Username(username.as_bytes()))
        .push(Attribute::Raw(0xC057, &[0, 1, 0, 0x32]))
        .push(Attribute::Controlling(from.tiebreaker))
        .push(Attribute::Priority(1_845_501_695))
        .push(Attribute::Nomination(nomination));
    if is_used {
        writer.push(Attribute::UseCandidate);
    }
    writer.seal(crypto, Integrity::Sha1(to.pwd().as_bytes()));
    writer.finish(true)
}

// The address of the far candidate of the pair a device selected for a far side.
fn selected_far(world: &World, device: usize, side: u32) -> Option<SocketAddr> {
    let agent = &world.devices[device].agent;
    let selected = agent.sides.iter().find(|entry| entry.number == Some(side))?.selected?;
    Some(agent.far(agent.pair(selected)?.far)?.candidate.address)
}

#[test]
fn renomination() {
    let mut world = World::new(
        71,
        vec![
            Device::new(Role::Controlling, 19, &["198.51.100.10", "198.51.100.11"], false),
            Device::new(Role::Controlled, 20, &["203.0.113.20"], false),
        ],
    );
    pair_of(&mut world);
    assert!(connected(&world));
    let first = selected_far(&world, 1, 0).unwrap();
    let other =
        world.devices[0].bound.iter().map(|(_, address)| *address).find(|address| address.ip() != first.ip()).unwrap();
    let target = world.devices[1].bound[0].1;
    let crypto = Exact(Cell::new(99));
    let send = |world: &mut World, from: SocketAddr, nomination: u32, is_used: bool| {
        let bytes = libwebrtc_check(&world.devices[0].agent, &world.devices[1].agent, nomination, is_used, &crypto);
        world.route(None, from, target, bytes);
        let until = world.now + second(1);
        world.run(until);
    };
    send(&mut world, other, 7, false);
    assert_eq!(selected_far(&world, 1, 0), Some(other), "a renomination by libwebrtc's count alone moves the pair");
    send(&mut world, first, 1, false);
    assert_eq!(selected_far(&world, 1, 0), Some(other), "a count already heard on that pair is no new nomination");
    send(&mut world, first, 8, true);
    assert_eq!(selected_far(&world, 1, 0), Some(first), "the last nomination wins");
    assert!(world.devices[1].paths_of(0).len() >= 3 && world.devices[1].agent.state(0) == Some(State::Connected));
    assert_media(&mut world);
}

#[test]
fn removal_signals() {
    let mut world = World::new(
        73,
        vec![
            Device::new(Role::Controlling, 21, &["198.51.100.10", "2001:db8:a::10"], false),
            Device::new(Role::Controlled, 22, &["203.0.113.20", "2001:db8:b::20"], false),
        ],
    );
    pair_of(&mut world);
    let crypto = Exact(Cell::new(5));
    let now = world.now;
    world.devices[1].agent.drop_local(&crypto, "2001:db8:b::20".parse().unwrap(), now);
    world.run(second(8));
    let agent = &world.devices[0].agent;
    assert!(!agent.fars.iter().any(|far| far.candidate.address.is_ipv6()), "the far device let its address go");
    assert!(!agent.pairs.iter().any(|pair| agent.far(pair.far).is_none()), "and its pairs went with it");
    assert!(connected(&world), "over what is left");
    assert_media(&mut world);
}

#[test]
fn close_deallocates() {
    let mut world = World::new(
        79,
        vec![
            Device::new(Role::Controlling, 23, &["198.51.100.10"], true),
            Device::new(Role::Controlled, 24, &["203.0.113.20"], true),
        ],
    );
    world.start(&[(0, Transport::Udp), (1, Transport::Tcp)]);
    world.run(second(10));
    assert!(connected(&world));
    assert_eq!(world.relay.allocation_count(), 4, "an allocation of each family on each device");
    for device in 0..2 {
        let now = world.now;
        let Device { agent, crypto, .. } = &mut world.devices[device];
        agent.close(crypto, now);
    }
    world.run(second(11));
    assert_eq!(world.relay.allocation_count(), 0, "each let go at the relay");
    assert!(
        world.devices.iter().all(|device| device.bound.is_empty() && device.links.iter().all(|link| !link.is_open)),
        "every socket closed"
    );
    assert!(world.devices.iter().all(|device| device.agent.next_wait(world.now).is_none()), "and nothing due after");
}

#[test]
fn relay_lines() {
    use crate::net::turn::{Discard, Failure};
    assert_eq!(
        [
            to_relay_line(Transport::Tls, Failure::Refused(401)),
            to_discard_line(Transport::Tcp, Some(400), Discard::Unsealed),
            to_discard_line(Transport::Udp, None, Discard::Unverified),
            to_discard_line(Transport::Udp, Some(300), Discard::Downgraded),
        ],
        [
            "ice: relay over tls failed: refused 401",
            "ice: relay over tcp answered 400, passed over: not sealed",
            "ice: relay over udp answered a success, passed over: integrity broken",
            "ice: relay over udp answered 300, passed over: a downgrade",
        ],
        "a relay's end, and an answer passed over by its code and why"
    );
}

// A caller and callees at 203.0.113.21 on, each answering after the delay `answers` gives it.
fn forked_world(seed: u64, answers: &[Duration]) -> World {
    let mut devices = vec![Device::new(Role::Controlling, seed, &["198.51.100.10"], false)];
    for (at, delay) in answers.iter().enumerate() {
        let ip = format!("203.0.113.{}", 21 + at);
        let mut callee = Device::new(Role::Controlled, seed + 1 + at as u64, &[ip.as_str()], false);
        (callee.credentials_delay, callee.lines_delay) = (*delay, *delay);
        devices.push(callee);
    }
    World::new(seed, devices)
}

#[test]
fn forked_answers() {
    let ms = Duration::from_millis;
    for answers in [vec![ms(30), ms(2500)], vec![ms(30), ms(1500), ms(4000)]] {
        let mut world = forked_world(107, &answers);
        world.start(&[]);
        world.run(second(12));
        assert!(connected(&world), "every device connected: {:?}", world.devices[0].states);
        for (at, answer) in answers.iter().enumerate() {
            let side = 1 + at as u32;
            let connected_at = world.devices[0].connected_at(side).unwrap();
            assert!(
                connected_at > *answer && connected_at < *answer + second(1),
                "device {side} connected once it answered: {connected_at:?}"
            );
            assert_eq!(world.devices[0].states_of(side), [State::Connected], "and stayed so");
        }
        let sides: Vec<u32> = (1..=answers.len() as u32).collect();
        assert_forked_media(&mut world, &sides);
        assert_eq!(world.devices[0].agent.sides.len(), answers.len(), "a far side for each device and no other");
        world.assert_bounded();
    }
}

#[test]
fn side_dropped() {
    let ms = Duration::from_millis;
    let mut world = forked_world(109, &[ms(30), ms(600)]);
    world.start(&[]);
    world.run(second(5));
    assert!(connected(&world));
    let before = (world.devices[0].states.len(), world.devices[0].paths.len(), world.devices[0].delivered.len());
    world.devices[0].agent.drop_side(1);
    let dropped_at = world.now;
    let gone = world.devices[1].bound[0].1;
    for _ in 0..10 {
        world.media(1, 0, &tagged(1, 0));
        world.media(0, 2, &tagged(0, 2));
        world.media(2, 0, &tagged(2, 0));
        let until = world.now + second(1);
        world.run(until);
    }
    let caller = &world.devices[0];
    assert_eq!(caller.agent.state(1), None, "the side is gone");
    let told = (&caller.states[before.0..], &caller.paths[before.1..]);
    assert!(
        told.0.iter().all(|(_, side, _)| *side != 1) && told.1.iter().all(|(side, _)| *side != 1),
        "nothing told of it after: {told:?}"
    );
    let delivered = &caller.delivered[before.2..];
    assert!(delivered.iter().all(|(side, _)| *side == 2), "its media goes nowhere: {delivered:?}");
    assert_eq!(delivered.len(), 10, "while the other side's comes");
    assert_eq!(caller.agent.state(2), Some(State::Connected));
    let checks = world.requests.iter().filter(|(at, device, to)| *at > dropped_at && *device == 0 && *to == gone);
    assert_eq!(checks.count(), 0, "no check goes to the side let go");
    assert!(world.devices[2].delivered.iter().filter(|(side, _)| *side == 0).count() >= 10, "media to the other");
    world.run(dropped_at + second(40));
    assert!(
        world.devices[1].states_of(0).contains(&State::Disconnected),
        "the device let go loses consent, its checks unanswered: {:?}",
        world.devices[1].states
    );
    assert_eq!(world.devices[0].agent.sides.len(), 1, "and its checks make no side anew");
    assert_eq!(world.devices[0].agent.state(2), Some(State::Connected), "while the other stays");
}

#[test]
fn side_dropped_streams() {
    let crypto = Exact(Cell::new(113));
    let mut agent = Agent::new(&crypto, Role::Controlling, false).unwrap();
    agent.local("198.51.100.10".parse().unwrap());
    let servers: [SocketAddr; 2] = ["192.0.2.80:443".parse().unwrap(), "192.0.2.81:443".parse().unwrap()];
    for (side, server) in [1, 2].into_iter().zip(servers) {
        agent.far_credentials(side, &format!("Srv{side}"), "aFarPasswordOf22Chars0");
        agent.far_server(side, server, Transport::Tcp, None);
    }
    let mut streams = Vec::new();
    let mut now = Duration::ZERO;
    while streams.len() < 2 && now < second(1) {
        for step in drain(&mut agent, &crypto, now) {
            if let Step::Connect { socket, far, .. } = step {
                streams.push((socket, far));
                agent.take_connected(&crypto, socket, Some("198.51.100.10:50000".parse().unwrap()), now);
            }
        }
        now += Duration::from_millis(50);
    }
    assert_eq!(streams.iter().map(|(_, far)| *far).collect::<Vec<_>>(), servers, "a stream to each server");
    agent.drop_side(1);
    let steps = drain(&mut agent, &crypto, now);
    assert!(steps.contains(&Step::Close(streams[0].0)), "the stream of the side let go closed: {steps:?}");
    assert!(!steps.contains(&Step::Close(streams[1].0)), "the other side's kept");
    assert!(
        !steps.iter().any(|step| matches!(step, Step::State { side: 1, .. } | Step::Selected { side: 1, .. })),
        "nothing told of the side let go"
    );
}

#[test]
fn checks_before_credentials() {
    let ms = Duration::from_millis;
    let mut world = forked_world(127, &[ms(3000)]);
    world.devices[1].lines_delay = ms(3500);
    world.start(&[]);
    world.run(ms(2900));
    let callee = world.devices[1].agent.ufrag().to_string();
    let caller = &world.devices[0].agent;
    let named = caller.sides.iter().find(|side| side.number.is_none());
    assert!(
        named.is_some_and(|side| side.ufrag.as_deref() == Some(callee.as_str())),
        "the checks that came first make a side known by the device's fragment alone"
    );
    assert_eq!(caller.state(1), None, "before its answer the device has no number");
    let held = caller.fars.iter().filter(|far| Some(far.side) == named.map(|side| side.id)).count();
    assert!(held > 0, "the device's address learned from its checks");
    world.run(second(8));
    let caller = &world.devices[0].agent;
    assert_eq!(caller.sides.len(), 1, "the side its checks made known is the answering side's own");
    assert_eq!(caller.sides[0].number, Some(1));
    assert!(connected(&world), "{:?} {:?}", world.devices[0].states, world.devices[1].states);
    assert_forked_media(&mut world, &[1]);
}

#[test]
fn relay_shared() {
    let ms = Duration::from_millis;
    let mut world = forked_world(131, &[ms(30), ms(800)]);
    world.devices[0] = Device::new(Role::Controlling, 131, &["198.51.100.10"], true);
    world.start(&[(0, Transport::Udp)]);
    world.run(second(10));
    assert!(connected(&world), "{:?}", world.devices[0].states);
    for side in [1, 2] {
        let path = world.devices[0].path(side);
        assert!(path.is_some_and(|path| path.local == Kind::Relay), "device {side} through the relay: {path:?}");
    }
    let callees: Vec<SocketAddr> = (1..=2).map(|device| world.devices[device].bound[0].1).collect();
    let agent = &world.devices[0].agent;
    let relay = agent.relays.iter().find(|relay| relay.bound.len() == 2).expect("one allocation serves both");
    assert!(
        callees.iter().all(|callee| relay.permitted.contains(&callee.ip()) && relay.bound.contains(callee)),
        "a permission and a channel for each device: {:?} {:?}",
        relay.permitted,
        relay.bound
    );
    assert_eq!(world.relay.allocation_count(), 2, "one allocation of each family, shared by both devices");
    assert_forked_media(&mut world, &[1, 2]);
}

#[test]
fn consent_split() {
    let ms = Duration::from_millis;
    let mut world = forked_world(137, &[ms(30), ms(500)]);
    world.start(&[]);
    world.run(second(5));
    assert!(connected(&world));
    world.cut.push(("198.51.100.10".parse().unwrap(), "203.0.113.21".parse().unwrap()));
    for _ in 0..50 {
        world.media(0, 2, &tagged(0, 2));
        world.media(2, 0, &tagged(2, 0));
        let until = world.now + second(1);
        world.run(until);
    }
    let caller = &world.devices[0];
    assert!(caller.states_of(1).contains(&State::Disconnected), "consent lost toward the cut device");
    assert_eq!(caller.states_of(2), [State::Connected], "and kept toward the other throughout");
    let from_two = caller.delivered.iter().filter(|(side, _)| *side == 2).count();
    assert!(from_two >= 50, "whose media went on: {from_two}");
}

#[test]
fn address_reused() {
    let ms = Duration::from_millis;
    let mut world = forked_world(139, &[ms(30)]);
    world.start(&[]);
    world.run(second(5));
    assert!(connected(&world));
    let reused = world.devices[1].bound[0].1;
    let now = world.now;
    let Device { agent, crypto, .. } = &mut world.devices[1];
    agent.close(crypto, now);
    world.run(second(6));
    let mut other = Device::new(Role::Controlled, 141, &["203.0.113.21"], false);
    other.next_port = reused.port();
    world.join(other);
    world.run(second(12));
    assert_eq!(world.devices[2].bound[0].1, reused, "the new device at the address the first had");
    let caller = &world.devices[0];
    assert_eq!(caller.agent.state(2), Some(State::Connected), "its checks went to its own side: {:?}", caller.states);
    assert_eq!(selected_far(&world, 0, 2), Some(reused));
    let before = world.devices[0].delivered.len();
    world.media(2, 0, &tagged(2, 0));
    let until = world.now + second(1);
    world.run(until);
    assert_eq!(world.devices[0].delivered[before..], [(2, tagged(2, 0))], "its media at its side, not the first's");
    world.run(second(60));
    let states = world.devices[0].states_of(1);
    assert!(states.contains(&State::Disconnected), "the first side's consent ran out: {states:?}");
    assert_eq!(world.devices[0].agent.state(2), Some(State::Connected));
}

#[test]
fn sides_bounded() {
    let crypto = Exact(Cell::new(149));
    let mut agent = Agent::new(&crypto, Role::Controlling, false).unwrap();
    agent.local("198.51.100.10".parse().unwrap());
    drain(&mut agent, &crypto, Duration::ZERO);
    let sides = 5;
    for side in 0..sides {
        for at in 0..MOST_CANDIDATES as u32 {
            let line =
                format!("candidate:{at} 1 udp {} 203.0.{side}.{} {} typ host", 2_000_000_000 - at, at, 1000 + at);
            agent.far_candidate(side, &line);
        }
    }
    let counts: Vec<usize> =
        agent.sides.iter().map(|side| agent.fars.iter().filter(|far| far.side == side.id).count()).collect();
    let share = MOST_CANDIDATES / sides as usize;
    assert!(counts.iter().all(|count| *count + 1 >= share), "each side keeps its share of the candidates: {counts:?}");
    let pairs: Vec<usize> =
        agent.sides.iter().map(|side| agent.pairs.iter().filter(|pair| pair.side == side.id).count()).collect();
    assert!(pairs.iter().all(|count| *count + 1 >= share), "and of the pairs: {pairs:?}");
    for side in 0..3 * MOST_SIDES as u32 {
        agent.far_candidate(side, "candidate:1 1 udp 2000000000 203.0.113.9 9000 typ host");
    }
    let socket = agent.locals[0].socket.unwrap();
    for at in 0..3 * MOST_SIDES as u16 {
        let username = format!("{}:n{at:03}", agent.ufrag());
        let mut writer = Writer::new(BINDING, Class::Request, &[at as u8; 12]);
        writer.push(Attribute::Username(username.as_bytes())).push(Attribute::Priority(2_000_000_000));
        writer.seal(&crypto, Integrity::Sha1(agent.pwd().as_bytes()));
        let from = SocketAddr::new("198.18.0.1".parse().unwrap(), 1024 + at);
        agent.take_datagram(&crypto, socket, from, &writer.finish(true), Duration::ZERO);
    }
    drain(&mut agent, &crypto, Duration::ZERO);
    assert!(
        agent.sides.len() == MOST_SIDES
            && agent.fars.len() <= MOST_CANDIDATES
            && agent.pairs.len() <= MOST_PAIRS
            && agent.checks.len() <= MOST_CHECKS,
        "as many sides as the bound, and the tables within theirs: {} {} {}",
        agent.sides.len(),
        agent.fars.len(),
        agent.pairs.len()
    );
}

fn sfu_world(seed: u64) -> World {
    let device = Device::new(Role::Controlling, seed, &["198.51.100.10", "2001:db8:a::10"], false);
    let sfu = Sfu::new(&device.agent);
    let mut world = World::new(seed, vec![device]);
    let agent = &mut world.devices[0].agent;
    agent.far_credentials(SFU_SIDE, &sfu.ufrag, &sfu.pwd);
    for address in &sfu.udp {
        agent.far_server(SFU_SIDE, *address, Transport::Udp, None);
    }
    for address in &sfu.tcp {
        agent.far_server(SFU_SIDE, *address, Transport::Tcp, None);
    }
    for address in &sfu.tls {
        agent.far_server(SFU_SIDE, *address, Transport::Tls, Some(SFU_NAME));
    }
    world.sfu = Some(sfu);
    world.start(&[]);
    world
}

fn assert_sfu(world: &mut World, transport: Transport) {
    let device = &world.devices[0];
    assert_eq!(device.agent.state(SFU_SIDE), Some(State::Connected), "{:?}", device.states);
    assert_eq!(
        device.path(SFU_SIDE).map(|path| (path.local, path.far, path.transport)),
        Some((Kind::Host, Kind::Host, transport))
    );
    let sfu = world.sfu.as_ref().unwrap();
    assert!(sfu.refusals.is_empty(), "nothing the server refuses: {:?}", sfu.refusals);
    let nominated = sfu.nominated.expect("nominated by USE-CANDIDATE");
    assert_eq!(matches!(nominated, Locator::Udp { .. }), transport == Transport::Udp);
    let until = world.now + second(60);
    world.run(until);
    let sfu = world.sfu.as_ref().unwrap();
    assert!(sfu.inactive_at.is_some_and(|at| at > world.now), "every ping of the server answered");
    let state = world.devices[0].agent.state(SFU_SIDE);
    assert!(sfu.refusals.is_empty() && state == Some(State::Connected), "{:?}", sfu.refusals);
    world.media(0, SFU_SIDE, &MEDIA);
    let until = world.now + second(1);
    world.run(until);
    let sfu = world.sfu.as_ref().unwrap();
    assert_eq!(sfu.media.last(), Some(&nominated), "media over the nominated way");
    let delivered = world.devices[0].delivered.len();
    let back = match nominated {
        Locator::Udp { .. } => vec![(nominated, MEDIA.to_vec())],
        Locator::Tcp { .. } => vec![(nominated, [0x90; 1200].to_vec())],
    };
    world.sfu_out(back);
    let until = world.now + second(1);
    world.run(until);
    let got = &world.devices[0].delivered[delivered..];
    assert!(got.len() == 1 && got[0].0 == SFU_SIDE, "and from the server, at its side: {got:?}");
}

#[test]
fn sfu_udp() {
    let mut world = sfu_world(83);
    world.run(second(5));
    assert_sfu(&mut world, Transport::Udp);
    let sfu = world.sfu.as_ref().unwrap();
    let nominated_v6 = matches!(sfu.nominated, Some(Locator::Udp { local, .. }) if local.is_ipv6());
    assert!(nominated_v6, "IPv6 first, as the server scores it higher too");
}

#[test]
fn sfu_tcp_tls() {
    let mut world = sfu_world(89);
    world.is_sfu_udp_blocked = true;
    world.run(second(10));
    assert_sfu(&mut world, Transport::Tcp);
    let mut world = sfu_world(97);
    (world.is_sfu_udp_blocked, world.is_sfu_tcp_blocked) = (true, true);
    world.run(second(10));
    assert_sfu(&mut world, Transport::Tls);
}

// What a test throws at an entry point: a datagram seen in a call with bits flipped, cut or lengthened, or random bytes.
fn mangled(world: &mut World, recorded: &[Vec<u8>]) -> Vec<u8> {
    let draw = world.draw();
    let mut bytes = match draw % 4 {
        0 => (0..(world.draw() % 200)).map(|_| world.draw() as u8).collect(),
        _ => recorded[(world.draw() % recorded.len() as u64) as usize].clone(),
    };
    match draw % 4 {
        1 if !bytes.is_empty() => {
            for _ in 0..1 + world.draw() % 4 {
                let at = (world.draw() % bytes.len() as u64) as usize;
                bytes[at] ^= 1 << (world.draw() % 8);
            }
        }
        2 => bytes.truncate((world.draw() % (bytes.len() as u64 + 1)) as usize),
        3 => bytes.extend((0..world.draw() % 40).map(|_| world.draw() as u8)),
        _ => {}
    }
    if draw.is_multiple_of(7) && bytes.len() >= 8 {
        bytes[0] &= 0x01;
        bytes[4..8].copy_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
    }
    bytes
}

#[test]
fn fuzz_entry_points() {
    let ms = Duration::from_millis;
    let mut world = forked_world(101, &[ms(30), ms(300)]);
    world.devices[2] = Device::new(Role::Controlled, 26, &["203.0.113.22"], false).behind("198.51.100.99");
    world.devices[2].agent.add_stun(RELAY.parse().unwrap());
    world.start(&[(0, Transport::Udp), (2, Transport::Tcp)]);
    world.run(second(10));
    let recorded = world.recorded.clone();
    assert!(recorded.len() > 20, "messages of a call to mangle");
    let lines: Vec<String> = world.devices.iter().flat_map(|device| device.told.clone()).collect();
    for round in 0..6000 {
        let device = round % 3;
        let bytes = mangled(&mut world, &recorded);
        let now = world.now;
        let from = match world.draw() % 3 {
            0 => SocketAddr::new("198.51.100.10".parse().unwrap(), 40_000 + (world.draw() % 4) as u16),
            1 => RELAY.parse().unwrap(),
            _ => SocketAddr::new(IpAddr::from([(world.draw() >> 8) as u8, 1, 2, 3]), world.draw() as u16),
        };
        let mut sockets: Vec<Socket> = world.devices[device].bound.iter().map(|(socket, _)| *socket).collect();
        sockets.extend(world.devices[device].links.iter().map(|link| link.socket));
        sockets.push(Socket(u32::MAX));
        let socket = sockets[(world.draw() % sockets.len() as u64) as usize];
        let side = match world.draw() % 4 {
            0 => world.draw() as u32,
            draw => draw as u32 - 1,
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let stranger = Socket(1 + (bytes.len() % 40) as u32);
        let transport = [Transport::Udp, Transport::Tcp, Transport::Tls][bytes.len() % 3];
        let Device { agent, crypto, .. } = &mut world.devices[device];
        match round % 10 {
            0..=2 => agent.take_datagram(crypto, socket, from, &bytes, now),
            3 => agent.take_stream(crypto, socket, &bytes, now),
            5 => {
                agent.far_credentials(side, &text, &text);
                // The relay's own address stays the relay's: the world takes every stream to it for TURN.
                if from != RELAY.parse().unwrap() {
                    agent.far_server(side, from, transport, Some(&text));
                }
                let _ = crate::net::stun::Uri::parse(&text);
            }
            6 => match bytes.len() % 3 {
                0 => agent.take_bound(crypto, stranger, Some(from), now),
                1 => agent.take_connected(crypto, stranger, Some(from), now),
                _ => agent.take_closed(stranger),
            },
            7 => agent.take_datagram(crypto, stranger, from, &bytes, now),
            8 => {
                agent.send(crypto, side, bytes.clone(), now);
                agent.drop_far(side, from);
                let _ = agent.state(side);
                if bytes.len() % 50 == 1 {
                    agent.drop_side(side);
                }
            }
            _ => {
                let line = &lines[(bytes.len() + round) % lines.len()];
                let mut mangled_line: Vec<u8> = line.as_bytes().to_vec();
                if !bytes.is_empty() {
                    let at = bytes[0] as usize % mangled_line.len();
                    mangled_line[at] = bytes[bytes.len() - 1];
                }
                agent.far_candidate(side, &String::from_utf8_lossy(&mangled_line));
                agent.far_candidate(side, &String::from_utf8_lossy(&bytes));
            }
        }
        let _ = Message::parse(&bytes);
        if round % 100 == 0 {
            let until = world.now + Duration::from_millis(300);
            world.run(until);
        }
        world.assert_bounded();
        let _ = world.devices[device].agent.next_wait(world.now);
    }
    let crypto = Exact(Cell::new(3));
    let relay = Relay::new(RELAY.parse().unwrap(), RELAY_V4.parse().unwrap(), RELAY_V6.parse().unwrap());
    let mut client = crate::net::turn::Client::new(relay.server(Transport::Udp), crate::net::stun::IPV4);
    let mut frames = Frames::new(Framing::Messages);
    client.start(&crypto, Duration::ZERO);
    for round in 0..3000 {
        let bytes = mangled(&mut world, &recorded);
        let now = Duration::from_millis(round);
        let _ = client.take(&crypto, &bytes, now);
        let _ = client.tick(&crypto, now);
        if frames.take(&bytes).is_none() {
            frames = Frames::new(Framing::Messages);
        }
    }
    let _ = client.next_wait(Duration::ZERO);
}

#[test]
fn candidate_flood() {
    let mut world = World::new(
        103,
        vec![
            Device::new(Role::Controlled, 27, &["198.51.100.10", "2001:db8:a::10"], false),
            Device::new(Role::Controlling, 28, &[], false),
        ],
    );
    pair_of(&mut world);
    let agent = &mut world.devices[0].agent;
    for at in 0..1000u32 {
        let line = format!(
            "candidate:{at} 1 udp {} 203.0.{}.{} {} typ host",
            2_000_000_000 - at,
            at / 250,
            at % 250,
            1000 + at
        );
        agent.far_candidate(1, &line);
    }
    let crypto = Exact(Cell::new(77));
    for at in 0..1000u16 {
        let request = {
            let username = format!("{}:abcd", world.devices[0].agent.ufrag());
            let mut writer = Writer::new(BINDING, Class::Request, &[at as u8; 12]);
            writer.push(Attribute::Username(username.as_bytes())).push(Attribute::Priority(1_000 + u32::from(at)));
            writer.seal(&crypto, Integrity::Sha1(world.devices[0].agent.pwd().as_bytes()));
            writer.finish(true)
        };
        let socket = world.devices[0].bound[0].0;
        let now = world.now;
        let from = SocketAddr::new("198.18.0.1".parse().unwrap(), 1024 + at);
        let Device { agent, crypto, .. } = &mut world.devices[0];
        agent.take_datagram(crypto, socket, from, &request, now);
    }
    world.run(second(60));
    world.assert_bounded();
    let agent = &world.devices[0].agent;
    assert!(
        agent.fars.len() == MOST_CANDIDATES && agent.pairs.len() <= MOST_PAIRS,
        "full, shared, and no further: {} {}",
        agent.fars.len(),
        agent.pairs.len()
    );
}

const SFU_NAME: &str = "sfu.example.org";
// Signal's calling server: its ping period by default, its retransmissions of a ping, how long a client may go without answering, and the longest frame its streams take (`config.rs`, `candidate_selector.rs`, `packet_server/epoll.rs`).
const SFU_PING_PERIOD: Duration = Duration::from_millis(1000);
const SFU_RTO: Duration = Duration::from_millis(500);
const SFU_RETRANSMITS: u32 = 6;
const SFU_RTO_LAST: u32 = 16;
const SFU_INACTIVITY: Duration = Duration::from_secs(30);
const SFU_FRAME_MOST: usize = 1500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Locator {
    Udp { local: SocketAddr, peer: SocketAddr },
    Tcp { device: usize, socket: Socket },
}

struct Ping {
    locator: Locator,
    transaction: Option<[u8; 12]>,
    previous: Option<[u8; 12]>,
    retransmits: u32,
    next: Duration,
}

// Signal's calling server as its source has it take a client's checks, answer them and ping back (`ice.rs`, `candidate_selector.rs`, `connection.rs`, `sfu.rs`): what it would refuse is noted.
struct Sfu {
    ufrag: String,
    pwd: String,
    client_ufrag: String,
    client_pwd: String,
    udp: Vec<SocketAddr>,
    tcp: Vec<SocketAddr>,
    tls: Vec<SocketAddr>,
    pings: Vec<Ping>,
    nominated: Option<Locator>,
    inactive_at: Option<Duration>,
    refusals: Vec<&'static str>,
    media: Vec<Locator>,
    crypto: Exact,
}

impl Sfu {
    fn new(client: &Agent) -> Sfu {
        Sfu {
            ufrag: "Sf4u".to_string(),
            pwd: "SfuPasswordOf32CharactersInBase6".to_string(),
            client_ufrag: client.ufrag().to_string(),
            client_pwd: client.pwd().to_string(),
            udp: vec!["192.0.2.80:10000".parse().unwrap(), "[2001:db8::80]:10000".parse().unwrap()],
            tcp: vec!["192.0.2.80:10000".parse().unwrap(), "[2001:db8::80]:10000".parse().unwrap()],
            tls: vec!["192.0.2.80:443".parse().unwrap()],
            pings: Vec::new(),
            nominated: None,
            inactive_at: None,
            refusals: Vec::new(),
            media: Vec::new(),
            crypto: Exact(Cell::new(0x5F0)),
        }
    }

    // `join_username`: the receiver's user fragment, then the sender's.
    fn server_username(&self) -> String {
        format!("{}:{}", self.ufrag, self.client_ufrag)
    }

    fn client_username(&self) -> String {
        format!("{}:{}", self.client_ufrag, self.ufrag)
    }

    // `StunPacket::sanity_check`: after MESSAGE-INTEGRITY only FINGERPRINT, nothing after FINGERPRINT, not both roles.
    fn is_sane(bytes: &[u8]) -> bool {
        let mut at = 20;
        let mut is_sealed = false;
        while at + 4 <= bytes.len() {
            let kind = u16::from_be_bytes([bytes[at], bytes[at + 1]]);
            let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            if is_sealed && kind != FINGERPRINT {
                return false;
            }
            is_sealed |= kind == MESSAGE_INTEGRITY;
            at += 4 + length.next_multiple_of(4);
        }
        at == bytes.len()
    }

    fn ping(&mut self, at: usize, now: Duration) -> (Locator, Vec<u8>) {
        let transaction = match self.pings[at].transaction {
            Some(transaction) => transaction,
            None => {
                let mut transaction = [0u8; 12];
                self.crypto.random(&mut transaction);
                self.pings[at].transaction = Some(transaction);
                transaction
            }
        };
        let mut writer = Writer::new(BINDING, Class::Request, &transaction);
        let username = self.client_username();
        writer
            .push(Attribute::Username(username.as_bytes()))
            .seal(&self.crypto, Integrity::Sha1(self.client_pwd.as_bytes()));
        self.pings[at].next = now + SFU_RTO;
        (self.pings[at].locator, writer.finish(true))
    }

    fn take(&mut self, locator: Locator, bytes: &[u8], now: Duration) -> Vec<(Locator, Vec<u8>)> {
        if matches!(locator, Locator::Tcp { .. }) && bytes.len() > SFU_FRAME_MOST {
            self.refusals.push("a frame longer than the stream takes");
        }
        if !is_message(bytes) {
            self.media.push(locator);
            return vec![];
        }
        let Some(message) = Message::parse(bytes).filter(|_| Self::is_sane(bytes)) else {
            self.refusals.push("a message it does not parse");
            return vec![];
        };
        if message.controlling.is_some() && message.controlled.is_some() {
            self.refusals.push("both roles");
        }
        match message.class {
            Class::Request => {
                if message.username != Some(self.server_username().as_bytes()) {
                    self.refusals.push("an unknown username");
                    return vec![];
                }
                if !message.verify(&self.crypto, Integrity::Sha1(self.pwd.as_bytes())) || !message.has_fingerprint {
                    self.refusals.push("an integrity that does not hold");
                    return vec![];
                }
                let mut writer = Writer::new(BINDING, Class::Success, &message.transaction);
                let username = self.client_username();
                writer.push(Attribute::Username(username.as_bytes()));
                if let Locator::Udp { peer, .. } = locator {
                    writer.push(Attribute::XorMapped(peer));
                }
                writer.seal(&self.crypto, Integrity::Sha1(self.pwd.as_bytes()));
                let mut out = vec![(locator, writer.finish(true))];
                if !self.pings.iter().any(|ping| ping.locator == locator) {
                    self.pings.push(Ping { locator, transaction: None, previous: None, retransmits: 0, next: now });
                    let at = self.pings.len() - 1;
                    out.push(self.ping(at, now));
                }
                if message.use_candidate {
                    self.nominated = Some(locator);
                }
                out
            }
            Class::Success => {
                if !message.verify(&self.crypto, Integrity::Sha1(self.client_pwd.as_bytes())) {
                    self.refusals.push("an answer whose integrity does not hold");
                } else if message.mapped.is_none() {
                    self.refusals.push("an answer without a mapped address");
                } else if let Some(ping) = self.pings.iter_mut().find(|ping| ping.locator == locator) {
                    if ping.transaction == Some(message.transaction) {
                        (ping.previous, ping.transaction, ping.retransmits) = (ping.transaction, None, 0);
                        ping.next = now + SFU_PING_PERIOD;
                        self.inactive_at = Some(now + SFU_INACTIVITY);
                    } else if ping.previous != Some(message.transaction) {
                        self.refusals.push("an answer to no ping");
                    }
                }
                vec![]
            }
            _ => {
                self.refusals.push("an error or an indication");
                vec![]
            }
        }
    }

    fn next_due(&self) -> Option<Duration> {
        self.pings.iter().map(|ping| ping.next).min()
    }

    fn tick(&mut self, now: Duration) -> Vec<(Locator, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.pings.len() {
            if self.pings[at].next > now {
                at += 1;
                continue;
            }
            if self.pings[at].transaction.is_some() && self.pings[at].retransmits == SFU_RETRANSMITS {
                self.pings.remove(at);
                continue;
            }
            if self.pings[at].transaction.is_some() {
                let retransmits = self.pings[at].retransmits;
                self.pings[at].retransmits += 1;
                let (locator, bytes) = self.ping(at, now);
                let wait = if retransmits < SFU_RETRANSMITS - 1 {
                    SFU_RTO * (2 << retransmits)
                } else {
                    SFU_RTO * SFU_RTO_LAST
                };
                self.pings[at].next = now + wait;
                out.push((locator, bytes));
            } else {
                out.push(self.ping(at, now));
            }
            at += 1;
        }
        out
    }
}
