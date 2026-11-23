use super::state::{Answer, Asker, Order, Wishes};
use super::{
    FORGOTTEN_LINE, GATEWAY, HOST, MASK, NO_ADDRESS, NO_NAT64_LINE, NO_NUMBER_LINE, Standing, UNFIT_LINE,
    UNROUTED_LINE, to_kept_line, to_link_line, to_refused_line, to_routed_line, to_unfound_line, to_untranslated_line,
};
use crate::cell::link::{OWN_ADDRESS, Routed};
use crate::net::arp;
use crate::net::dhcp::{FIRST_WAIT, Server};
use crate::net::dns::nat64::Prefix;
use crate::net::ethernet::{self, BROADCAST, Hardware, Kind};
use crate::net::nat::{Emit, Nat, Outside, Path, SWEEP_EVERY, Sink};
use crate::net::siit::{NUMBER_BYTES, Translator};
use crate::net::wireguard::config::Config;
use crate::net::wireguard::{self, Crypto, Peer, REKEY_TIMEOUT};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use core::time::Duration;

// A stay refused is asked for anew no sooner than a host that waits for its lease asks again (RFC 2131, 4.1).
const ASK_AGAIN: Duration = FIRST_WAIT;
// The peer's address that was not found, or not reached, is looked for anew as a handshake nobody answered is tried anew.
const FIND_AGAIN: Duration = REKEY_TIMEOUT;

/// What the forwarder's thread does next, one effect each.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // Holds the network, and waits for it; `Machine::take_hold` takes what came of it.
    Hold,
    // Lets go of the hold; what the stay still brings is forwarded until it ends.
    Release,
    // Watches for the end of the stay held.
    Watch,
    // Opens the modem's interface for packets of the kind a link names by that number.
    OpenLte(u16),
    // Lets go of what stands of a stay: its watch, its sockets, the relay; the hold goes by `Release` alone.
    Close,
    // A frame for the host's link.
    ToHost(Vec<u8>),
    // A packet for the device's own stack.
    ToOwn(Vec<u8>),
    // A packet for the modem's interface.
    ToLte(Vec<u8>),
    // A datagram for the tunnel's peer.
    ToPeer(Vec<u8>),
    // Has a thread beside the forwarder find the translation of a network of IPv6 alone; its `Answer` comes with the wishes.
    Translate,
    // Has a thread beside the forwarder look the peer's name up; its `Answer` comes with the wishes.
    Resolve(String),
    // Opens the tunnel's socket toward the first of these addresses, one at the least, that the stay reaches; `Machine::take_connected` takes what came of it.
    Connect(Vec<SocketAddr>),
    // Closes the tunnel's socket.
    Disconnect,
    // Starts the relay of the host's lookups, or stops it.
    Relay(bool),
    // Writes the config's file, its text `Config::to_text`; `Machine::take_kept` takes what came of it.
    Keep(Config),
    // Removes the config's file; `Machine::take_forgotten` takes what came of it.
    Forget,
    Log(String),
    // Waits for news, that long at the most.
    Wait(Option<Duration>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    None,
    Asked,
    Held,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tunnel {
    Off,
    // The stay's connection carries too little for it, or the library gave no key.
    Unfit,
    Finding,
    Connecting,
    Up,
    // The peer was not found or not reached at that time.
    Failed(Duration),
}

#[derive(Clone, Copy)]
struct Stay {
    number: u64,
    routed: Option<Routed>,
    is_open: bool,
}

/// The forwarder: what it is asked for, the host's link, the stay it forwards for, the flows and the tunnel.
pub struct Machine {
    hardware: Hardware,
    is_wanted: bool,
    uses_tunnel: bool,
    config: Option<Config>,
    keeping: VecDeque<(Config, Asker)>,
    stored: Option<(Asker, Result<(), String>)>,
    is_host_up: bool,
    host: Option<Hardware>,
    hold: Hold,
    refused: Option<Duration>,
    stay: Option<Stay>,
    nat: Nat,
    server: Option<Server>,
    translator: Option<Translator>,
    is_relaying: bool,
    tunnel: Tunnel,
    peer: Option<Peer>,
    has_begun: bool,
    sweep_at: Duration,
    logged: Option<Standing>,
    steps: VecDeque<Step>,
}

impl Machine {
    /// A forwarder whose hardware address on the host's link is `hardware`, with the config a run before kept.
    pub fn new(hardware: Hardware, config: Option<Config>) -> Machine {
        Machine {
            hardware,
            is_wanted: false,
            uses_tunnel: false,
            config,
            keeping: VecDeque::new(),
            stored: None,
            is_host_up: false,
            host: None,
            hold: Hold::None,
            refused: None,
            stay: None,
            nat: Nat::new(HOST, GATEWAY, OWN_ADDRESS),
            server: None,
            translator: None,
            is_relaying: false,
            tunnel: Tunnel::Off,
            peer: None,
            has_begun: false,
            sweep_at: Duration::ZERO,
            logged: None,
            steps: VecDeque::new(),
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        self.steps.push_back(Step::Log(line.into()));
    }

    fn is_open(&self) -> bool {
        self.stay.is_some_and(|stay| stay.is_open)
    }

    // True while a host is served: its link has every stay made over the modem and routed.
    pub(super) const fn is_routing(&self) -> bool {
        self.is_wanted && self.is_host_up
    }

    // True while a config of a tunnel is kept.
    pub(super) const fn has_config(&self) -> bool {
        self.config.is_some()
    }

    // Takes what keeping a config came to, with who handed it over.
    pub(super) const fn take_stored(&mut self) -> Option<(Asker, Result<(), String>)> {
        self.stored.take()
    }

    // How the router stands at `now`.
    pub(super) fn standing(&self, now: Duration) -> Standing {
        let is_up = self.tunnel == Tunnel::Up && self.peer.as_ref().is_some_and(|peer| peer.is_up(now));
        match (self.is_wanted, self.is_host_up, self.is_open() && self.hold == Hold::Held) {
            (false, ..) => Standing::Off,
            (true, false, _) => Standing::Alone,
            (true, true, false) => Standing::Joining,
            (true, true, true) if !self.uses_tunnel => Standing::Lte,
            (true, true, true) if self.config.is_none() => Standing::Unset,
            (true, true, true) if is_up => Standing::Tunnel,
            (true, true, true) => Standing::Shut,
        }
    }

    // The path the host's packets take: with the tunnel wanted the tunnel alone, whatever its state.
    fn path(&self) -> Path {
        if self.uses_tunnel {
            Path::Tunnel(self.config.as_ref().map_or(Ipv4Addr::UNSPECIFIED, |config| config.resolver))
        } else {
            Path::Lte
        }
    }

    fn drop_tunnel(&mut self) {
        if matches!(self.tunnel, Tunnel::Connecting | Tunnel::Up) {
            self.steps.push_back(Step::Disconnect);
        }
        (self.tunnel, self.peer, self.has_begun) = (Tunnel::Off, None, false);
        self.nat.set_tunnel(None);
    }

    fn find_endpoint(&mut self) {
        let Some(endpoint) = self.config.as_ref().map(|config| config.endpoint.clone()) else {
            return;
        };
        match endpoint.host.parse::<IpAddr>() {
            Ok(address) => {
                self.tunnel = Tunnel::Connecting;
                self.steps.push_back(Step::Connect(alloc::vec![SocketAddr::new(address, endpoint.port)]));
            }
            Err(_) => {
                self.tunnel = Tunnel::Finding;
                self.steps.push_back(Step::Resolve(endpoint.host));
            }
        }
    }

    fn begin_tunnel(&mut self, crypto: &impl Crypto) {
        let (Some(config), Some(routed)) = (&self.config, self.stay.and_then(|stay| stay.routed)) else {
            return;
        };
        let made = config.mtu_within(routed.mtu).and_then(|mtu| Some((mtu, config.to_keys(crypto)?)));
        let Some((mtu, keys)) = made else {
            self.tunnel = Tunnel::Unfit;
            return self.log(UNFIT_LINE);
        };
        self.peer = Some(Peer::new(keys, config.keepalive(), mtu));
        self.nat.set_tunnel(Some(Outside { address: config.address, mtu }));
        self.find_endpoint();
    }

    // The largest packet the host is told: that of the modem's way, or the tunnel's where a config is kept, whichever is less.
    fn set_server(&mut self, routed: Routed) {
        let way = routed.to_outside().mtu;
        let tunnel = self.config.as_ref().and_then(|config| config.mtu_within(routed.mtu));
        let mtu = tunnel.map_or(way, |tunnel| tunnel.min(way)).min(usize::from(u16::MAX)) as u16;
        self.server = Some(Server { address: GATEWAY.octets(), lent: HOST.octets(), mask: MASK.octets(), mtu });
    }

    fn open(&mut self, kind: u16) {
        let Some((stay, routed)) = self.stay.as_mut().and_then(|stay| stay.routed.map(|routed| (stay, routed))) else {
            return;
        };
        stay.is_open = true;
        self.nat.set_lte(Some(routed.to_outside()));
        self.set_server(routed);
        self.steps.push_back(Step::OpenLte(kind));
        self.log(to_routed_line(routed.to_outside().mtu));
    }

    fn close(&mut self) {
        self.drop_tunnel();
        if self.stay.take().is_some() {
            self.steps.push_back(Step::Close);
        }
        self.nat.set_lte(None);
        (self.server, self.translator, self.is_relaying) = (None, None, false);
    }

    // Brings what is held, relayed and tunnelled to what is wanted, and tells the log where the router's standing changed.
    fn settle(&mut self, crypto: &impl Crypto, now: Duration) {
        let may_ask = self.refused.is_none_or(|at| now.saturating_sub(at) >= ASK_AGAIN);
        match (self.is_routing(), self.hold) {
            (true, Hold::None) if may_ask => {
                self.hold = Hold::Asked;
                self.steps.push_back(Step::Hold);
            }
            (false, Hold::Held) => {
                self.hold = Hold::None;
                self.steps.push_back(Step::Release);
            }
            _ => {}
        }
        let serves = self.is_routing() && self.hold == Hold::Held && self.is_open();
        let relays = serves && !self.uses_tunnel;
        if core::mem::replace(&mut self.is_relaying, relays) != relays {
            self.steps.push_back(Step::Relay(relays));
        }
        match (serves && self.uses_tunnel && self.config.is_some(), self.tunnel) {
            (false, Tunnel::Off) => {}
            (false, _) => self.drop_tunnel(),
            (true, Tunnel::Off) => self.begin_tunnel(crypto),
            (true, Tunnel::Failed(at)) if now.saturating_sub(at) >= FIND_AGAIN => self.find_endpoint(),
            (true, _) => {}
        }
        let standing = self.standing(now);
        if self.logged.replace(standing) != Some(standing) {
            self.log(standing.to_log_line());
        }
    }

    /// Takes what the main loop asks, the orders it handed over and what the threads beside the forwarder found.
    pub fn take_wishes(&mut self, crypto: &impl Crypto, wishes: Wishes, now: Duration) {
        if (self.is_wanted, self.uses_tunnel) != (wishes.is_wanted, wishes.uses_tunnel) {
            (self.is_wanted, self.uses_tunnel) = (wishes.is_wanted, wishes.uses_tunnel);
            // None of the host's flows goes on along a way its packets take no more.
            self.nat.forget_host();
            self.refused = None;
        }
        for order in wishes.orders {
            match order {
                Order::Keep(config, asker) => {
                    self.steps.push_back(Step::Keep(config.clone()));
                    self.keeping.push_back((config, asker));
                }
                Order::Forget => self.steps.push_back(Step::Forget),
            }
        }
        for answer in wishes.answers {
            match answer {
                Answer::Endpoint(stay, found) => self.take_endpoint(stay, found, now),
                Answer::Translation(stay, found) => self.take_translation(crypto, stay, found),
            }
        }
        self.settle(crypto, now);
    }

    fn is_of_stay(&self, number: u64) -> bool {
        self.stay.is_some_and(|stay| stay.number == number)
    }

    fn take_endpoint(&mut self, stay: u64, found: Result<Vec<IpAddr>, String>, now: Duration) {
        if !self.is_of_stay(stay) || self.tunnel != Tunnel::Finding {
            return;
        }
        let port = self.config.as_ref().map_or(0, |config| config.endpoint.port);
        match found.and_then(|addresses| if addresses.is_empty() { Err(NO_ADDRESS.into()) } else { Ok(addresses) }) {
            Ok(addresses) => {
                self.tunnel = Tunnel::Connecting;
                let ends = addresses.into_iter().map(|address| SocketAddr::new(address, port)).collect();
                self.steps.push_back(Step::Connect(ends));
            }
            Err(reason) => {
                self.tunnel = Tunnel::Failed(now);
                self.log(to_unfound_line(&reason));
            }
        }
    }

    fn take_translation(&mut self, crypto: &impl Crypto, stay: u64, found: Result<Option<(Ipv6Addr, Prefix)>, String>) {
        if !self.is_of_stay(stay) || self.is_open() {
            return;
        }
        let mut number = [0u8; NUMBER_BYTES];
        match found {
            Ok(Some((kernel, prefix))) if crypto.random(&mut number) => {
                self.translator = Some(Translator::new(kernel, number, prefix));
                self.open(ethernet::IPV6);
            }
            Ok(Some(_)) => self.log(NO_NUMBER_LINE),
            Ok(None) => self.log(NO_NAT64_LINE),
            Err(reason) => self.log(to_untranslated_line(&reason)),
        }
    }

    /// Takes what writing the config's file came to.
    pub fn take_kept(&mut self, crypto: &impl Crypto, result: Result<(), String>, now: Duration) {
        let Some((config, asker)) = self.keeping.pop_front() else {
            return;
        };
        if result.is_ok() {
            self.config = Some(config);
            // A tunnel that stands is of the config before.
            self.drop_tunnel();
            self.log(to_kept_line());
        }
        self.stored = Some((asker, result));
        self.settle(crypto, now);
    }

    /// Takes whether the config's file is gone.
    pub fn take_forgotten(&mut self, crypto: &impl Crypto, is_gone: bool, now: Duration) {
        if is_gone {
            self.config = None;
            self.drop_tunnel();
            self.log(FORGOTTEN_LINE);
        }
        self.settle(crypto, now);
    }

    /// Takes whether the host's link is up.
    pub fn take_link(&mut self, crypto: &impl Crypto, is_up: bool, now: Duration) {
        if core::mem::replace(&mut self.is_host_up, is_up) != is_up {
            self.log(to_link_line(is_up));
            (self.host, self.refused) = (None, None);
            self.nat.forget_host();
        }
        self.settle(crypto, now);
    }

    /// Takes what holding the network came to: the stay's number with what a routed one took off the modem's interface, or why there is none.
    pub fn take_hold(&mut self, crypto: &impl Crypto, held: Result<(u64, Option<Routed>), String>, now: Duration) {
        match held {
            Ok((number, routed)) => {
                (self.hold, self.refused) = (Hold::Held, None);
                if !self.is_of_stay(number) {
                    self.close();
                    self.stay = Some(Stay { number, routed, is_open: false });
                    self.steps.push_back(Step::Watch);
                    match routed {
                        Some(Routed { address: Some(_), .. }) => self.open(ethernet::IPV4),
                        Some(Routed { address: None, .. }) => self.steps.push_back(Step::Translate),
                        None => self.log(UNROUTED_LINE),
                    }
                }
            }
            Err(reason) => {
                (self.hold, self.refused) = (Hold::None, Some(now));
                self.log(to_refused_line(&reason));
            }
        }
        self.settle(crypto, now);
    }

    /// Takes the end of the stay.
    pub fn take_down(&mut self, crypto: &impl Crypto, now: Duration) {
        self.close();
        if core::mem::replace(&mut self.hold, Hold::None) == Hold::Held {
            self.steps.push_back(Step::Release);
        }
        self.refused = None;
        self.settle(crypto, now);
    }

    /// Takes what opening the tunnel's socket came to.
    pub fn take_connected(&mut self, crypto: &impl Crypto, result: Result<(), String>, now: Duration, utc: Duration) {
        if self.tunnel != Tunnel::Connecting {
            return;
        }
        match result {
            Ok(()) => {
                self.tunnel = Tunnel::Up;
                // The first session is made at once; one after a peer went silent is made as packets ask for it.
                if !core::mem::replace(&mut self.has_begun, true) {
                    let steps = self.peer.as_mut().map(|peer| peer.begin(crypto, now, utc)).unwrap_or_default();
                    self.run_peer(steps, now);
                }
            }
            Err(reason) => {
                self.tunnel = Tunnel::Failed(now);
                self.log(to_unfound_line(&reason));
            }
        }
        self.settle(crypto, now);
    }

    fn run_peer(&mut self, steps: Vec<wireguard::Step>, now: Duration) {
        for step in steps {
            match step {
                wireguard::Step::Send(datagram) => self.steps.push_back(Step::ToPeer(datagram)),
                wireguard::Step::Deliver(packet) => {
                    let emits = self.nat.take_tunnel(&packet, now);
                    self.emit_plain(emits);
                }
                wireguard::Step::Lost => {
                    self.tunnel = Tunnel::Failed(now);
                    self.steps.push_back(Step::Disconnect);
                }
            }
        }
    }

    fn hand_host(&mut self, kind: Kind, payload: &[u8]) {
        let frame = ethernet::to_frame(self.host.unwrap_or(BROADCAST), self.hardware, kind, payload);
        self.steps.push_back(Step::ToHost(frame));
    }

    // Hands on a packet the translation gives for any way but the tunnel, which a packet enters through `emit` alone.
    fn hand_on(&mut self, Emit { sink, packet }: Emit) {
        match sink {
            Sink::Lte => {
                let packet = match &self.translator {
                    Some(translator) => translator.to_ipv6(&packet),
                    None => Some(packet),
                };
                self.steps.extend(packet.map(Step::ToLte));
            }
            Sink::Own => self.steps.push_back(Step::ToOwn(packet)),
            Sink::Host => self.hand_host(Kind::Ipv4, &packet),
            Sink::Tunnel => {}
        }
    }

    fn emit_plain(&mut self, emits: Vec<Emit>) {
        emits.into_iter().for_each(|emit| self.hand_on(emit));
    }

    fn emit(&mut self, crypto: &impl Crypto, emits: Vec<Emit>, now: Duration, utc: Duration) {
        for emit in emits {
            if emit.sink == Sink::Tunnel {
                // A packet for a tunnel that is not up is dropped: it leaves by no other way.
                let peer = self.peer.as_mut().filter(|_| self.tunnel == Tunnel::Up);
                let steps = peer.map(|peer| peer.send(crypto, &emit.packet, now, utc)).unwrap_or_default();
                self.run_peer(steps, now);
            } else {
                self.hand_on(emit);
            }
        }
    }

    /// Takes a frame off the host's link.
    pub fn take_frame(&mut self, crypto: &impl Crypto, frame: &[u8], now: Duration, utc: Duration) {
        let Some((sender, kind, payload)) = ethernet::parse(frame).filter(|_| self.is_wanted) else {
            return;
        };
        match kind {
            Kind::Arp => {
                if let Some((asker, reply)) = arp::answer(payload, GATEWAY.octets(), self.hardware) {
                    self.steps.push_back(Step::ToHost(ethernet::to_frame(asker, self.hardware, Kind::Arp, &reply)));
                }
            }
            Kind::Ipv4 => {
                self.host = Some(sender);
                // A host is lent its address once its packets have a way.
                match self.server.as_ref().and_then(|server| server.answer(payload)) {
                    Some(reply) => {
                        self.steps.push_back(Step::ToHost(ethernet::to_frame(BROADCAST, self.hardware, kind, &reply)));
                    }
                    None => {
                        let emits = self.nat.take_host(self.path(), payload, now);
                        self.emit(crypto, emits, now, utc);
                    }
                }
            }
        }
        self.settle(crypto, now);
    }

    /// Takes a packet the device's own stack sent.
    pub fn take_own(&mut self, packet: &[u8], now: Duration) {
        let emits = self.nat.take_own(packet, now);
        self.emit_plain(emits);
    }

    /// Takes a packet that came over the modem's interface.
    pub fn take_lte(&mut self, packet: &[u8], now: Duration) {
        let emits = match self.translator {
            Some(translator) => translator.to_ipv4(packet).map(|packet| self.nat.take_lte(&packet, now)),
            None => Some(self.nat.take_lte(packet, now)),
        };
        self.emit_plain(emits.unwrap_or_default());
    }

    /// Takes a datagram the tunnel's peer sent.
    pub fn take_datagram(&mut self, crypto: &impl Crypto, datagram: &[u8], now: Duration, utc: Duration) {
        let peer = self.peer.as_mut().filter(|_| self.tunnel == Tunnel::Up);
        let steps = peer.map(|peer| peer.take(crypto, datagram, now, utc)).unwrap_or_default();
        self.run_peer(steps, now);
        self.settle(crypto, now);
    }

    fn tick(&mut self, crypto: &impl Crypto, now: Duration, utc: Duration) {
        if now >= self.sweep_at {
            self.nat.sweep(now);
            self.sweep_at = now.saturating_add(SWEEP_EVERY);
        }
        let peer = self.peer.as_mut().filter(|_| self.tunnel == Tunnel::Up);
        let steps = peer.map(|peer| peer.tick(crypto, now, utc)).unwrap_or_default();
        self.run_peer(steps, now);
        self.settle(crypto, now);
    }

    fn next_wait(&self, now: Duration) -> Option<Duration> {
        let sweep = (self.nat.flow_count() > 0).then(|| self.sweep_at.saturating_sub(now));
        let peer = self.peer.as_ref().filter(|_| self.tunnel == Tunnel::Up).and_then(|peer| peer.next_wait(now));
        [sweep, peer].into_iter().flatten().min()
    }

    /// The thread's next step at `now`, the clock at `utc`: what is due is done first, and the wait comes last.
    pub fn next(&mut self, crypto: &impl Crypto, now: Duration, utc: Duration) -> Step {
        if self.steps.is_empty() {
            self.tick(crypto, now, utc);
        }
        self.steps.pop_front().unwrap_or_else(|| Step::Wait(self.next_wait(now)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::dns::nat64::tests::prefix;
    use crate::net::wireguard::config::tests::TEXT;
    use crate::net::wireguard::tests::Toy;
    use alloc::vec;

    const IP_HEAD: usize = 20;

    const HARDWARE: Hardware = [2, 0, 0, 0, 0, 9];
    const HOST_HARDWARE: Hardware = [2, 0, 0, 0, 0, 1];
    const LTE: Ipv4Addr = Ipv4Addr::new(10, 64, 3, 7);
    const FAR: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
    const UTC: Duration = Duration::from_secs(1_800_000_000);

    fn second(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    fn config() -> Config {
        super::super::read_config(&TEXT.replace("vpn.example.org", "203.0.113.5")).unwrap()
    }

    fn named_config() -> Config {
        super::super::read_config(TEXT).unwrap()
    }

    fn udp(source: Ipv4Addr, target: Ipv4Addr, ports: (u16, u16), data: &[u8]) -> Vec<u8> {
        let length = (8 + data.len()) as u16;
        let mut packet = vec![0x45, 0];
        packet.extend((IP_HEAD as u16 + length).to_be_bytes());
        packet.extend([0, 0, 0, 0, 64, 17, 0, 0]);
        packet.extend([source.octets(), target.octets()].concat());
        let sum: u32 = packet.chunks(2).map(|word| u32::from(u16::from_be_bytes([word[0], word[1]]))).sum();
        let folded = (sum & 0xFFFF) + (sum >> 16);
        packet[10..12].copy_from_slice(&(!((folded & 0xFFFF) + (folded >> 16)) as u16).to_be_bytes());
        packet.extend([ports.0.to_be_bytes(), ports.1.to_be_bytes(), length.to_be_bytes(), [0, 0]].concat());
        packet.extend_from_slice(data);
        packet
    }

    fn frame(packet: &[u8]) -> Vec<u8> {
        ethernet::to_frame(HARDWARE, HOST_HARDWARE, Kind::Ipv4, packet)
    }

    // The steps up to the next wait, the log's lines left out.
    fn drain(machine: &mut Machine, crypto: &Toy, now: Duration) -> Vec<Step> {
        let mut steps = Vec::new();
        loop {
            match machine.next(crypto, now, UTC + now) {
                Step::Wait(_) => return steps,
                Step::Log(_) => {}
                step => steps.push(step),
            }
        }
    }

    fn wishes(is_wanted: bool, uses_tunnel: bool) -> Wishes {
        Wishes { is_wanted, uses_tunnel, ..Wishes::default() }
    }

    // A machine wanted, its host's link up and its stay routed over a connection of IPv4.
    fn routing(crypto: &Toy, uses_tunnel: bool, config: Option<Config>) -> Machine {
        let mut machine = Machine::new(HARDWARE, config);
        machine.take_wishes(crypto, wishes(true, uses_tunnel), second(1));
        machine.take_link(crypto, true, second(1));
        assert_eq!(drain(&mut machine, crypto, second(1)), [Step::Hold]);
        machine.take_hold(crypto, Ok((7, Some(Routed { address: Some(LTE), mtu: 1500 }))), second(1));
        machine
    }

    #[test]
    fn hold_follows_host() {
        let crypto = Toy::default();
        let mut machine = Machine::new(HARDWARE, None);
        assert_eq!(machine.standing(second(0)), Standing::Off);
        machine.take_link(&crypto, true, second(0));
        assert_eq!(drain(&mut machine, &crypto, second(0)), [], "a host on a device that is no modem holds nothing");
        machine.take_wishes(&crypto, wishes(true, false), second(1));
        assert!(machine.is_routing());
        assert_eq!(drain(&mut machine, &crypto, second(1)), [Step::Hold]);
        assert_eq!(machine.standing(second(1)), Standing::Joining);
        machine.take_hold(&crypto, Ok((7, Some(Routed { address: Some(LTE), mtu: 1500 }))), second(2));
        assert_eq!(
            drain(&mut machine, &crypto, second(2)),
            [Step::Watch, Step::OpenLte(0x0800), Step::Relay(true)],
            "the stay's end watched, the modem's interface opened for IPv4, the host's lookups relayed"
        );
        assert_eq!(machine.standing(second(2)), Standing::Lte);
        machine.take_link(&crypto, false, second(3));
        assert_eq!(
            drain(&mut machine, &crypto, second(3)),
            [Step::Release, Step::Relay(false)],
            "the host gone: the hold is let go, and what the stay still brings is forwarded"
        );
        assert!(!machine.is_routing() && machine.standing(second(3)) == Standing::Alone);
        machine.take_own(&udp(OWN_ADDRESS, FAR, (5000, 443), b"own"), second(3));
        assert!(matches!(&drain(&mut machine, &crypto, second(3))[..], [Step::ToLte(_)]));
        machine.take_link(&crypto, true, second(4));
        assert_eq!(drain(&mut machine, &crypto, second(4)), [Step::Hold]);
        machine.take_hold(&crypto, Ok((7, Some(Routed { address: Some(LTE), mtu: 1500 }))), second(4));
        assert_eq!(
            drain(&mut machine, &crypto, second(4)),
            [Step::Relay(true)],
            "the same stay: nothing is opened anew"
        );
        machine.take_down(&crypto, second(5));
        assert_eq!(
            drain(&mut machine, &crypto, second(5)),
            [Step::Close, Step::Release, Step::Hold],
            "a stay that ended is let go of whole, its hold too, and asked for anew at once"
        );
        machine.take_hold(&crypto, Ok((8, None)), second(6));
        assert_eq!(drain(&mut machine, &crypto, second(6)), [Step::Watch], "a stay that is not routed is waited out");
        assert_eq!(machine.standing(second(6)), Standing::Joining);
        machine.take_wishes(&crypto, wishes(false, false), second(7));
        assert_eq!(drain(&mut machine, &crypto, second(7)), [Step::Release]);
        assert_eq!(machine.standing(second(7)), Standing::Off);
    }

    #[test]
    fn new_stay_keeps_hold() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, false, None);
        drain(&mut machine, &crypto, second(1));
        machine.take_link(&crypto, false, second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [Step::Release, Step::Relay(false)]);
        machine.take_link(&crypto, true, second(3));
        assert_eq!(drain(&mut machine, &crypto, second(3)), [Step::Hold]);
        machine.take_hold(&crypto, Ok((8, Some(Routed { address: Some(LTE), mtu: 1500 }))), second(3));
        assert_eq!(
            drain(&mut machine, &crypto, second(3)),
            [Step::Close, Step::Watch, Step::OpenLte(0x0800), Step::Relay(true)],
            "what stood of the stay before is let go of, and the hold taken for the new one stands: no release follows"
        );
    }

    #[test]
    fn endpoint_without_address() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, true, Some(named_config()));
        drain(&mut machine, &crypto, second(1));
        let none = Answer::Endpoint(7, Ok(Vec::new()));
        machine.take_wishes(&crypto, Wishes { answers: vec![none], ..wishes(true, true) }, second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [], "a name that has no address opens no socket");
        assert_eq!(machine.standing(second(2)), Standing::Shut);
    }

    #[test]
    fn refused_hold_retry() {
        let crypto = Toy::default();
        let mut machine = Machine::new(HARDWARE, None);
        machine.take_wishes(&crypto, wishes(true, false), second(1));
        machine.take_link(&crypto, true, second(1));
        assert_eq!(drain(&mut machine, &crypto, second(1)), [Step::Hold]);
        machine.take_hold(&crypto, Err("the modem is not ready".into()), second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), []);
        let asking = frame(&udp(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, (68, 67), b"lease"));
        machine.take_frame(&crypto, &asking, second(5), UTC);
        assert_eq!(drain(&mut machine, &crypto, second(5)), [], "none before the host itself would ask again");
        machine.take_frame(&crypto, &asking, second(6), UTC);
        assert_eq!(drain(&mut machine, &crypto, second(6)), [Step::Hold], "four seconds on, its next frame asks");
        machine.take_hold(&crypto, Err("the modem is not ready".into()), second(6));
        machine.take_link(&crypto, false, second(7));
        machine.take_link(&crypto, true, second(7));
        assert_eq!(drain(&mut machine, &crypto, second(7)), [Step::Hold], "a link that comes up anew asks at once");
    }

    #[test]
    fn host_served_over_lte() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, false, None);
        drain(&mut machine, &crypto, second(1));
        let mut ask = vec![0, 1, 0x08, 0x00, 6, 4, 0, 1];
        ask.extend([&HOST_HARDWARE[..], &HOST.octets(), &[0; 6], &GATEWAY.octets()].concat());
        machine.take_frame(&crypto, &ethernet::to_frame(BROADCAST, HOST_HARDWARE, Kind::Arp, &ask), second(2), UTC);
        let steps = drain(&mut machine, &crypto, second(2));
        let [Step::ToHost(answer)] = &steps[..] else { panic!("not one frame: {steps:?}") };
        assert_eq!(
            (&answer[..6], &answer[6..12], &answer[12..14], &answer[22..28]),
            (&HOST_HARDWARE[..], &HARDWARE[..], &[0x08, 0x06][..], &HARDWARE[..]),
            "the router's hardware address, told the asker"
        );
        machine.take_frame(&crypto, &frame(&udp(HOST, FAR, (5000, 443), b"out")), second(2), UTC);
        let steps = drain(&mut machine, &crypto, second(2));
        let [Step::ToLte(out)] = &steps[..] else { panic!("not one packet: {steps:?}") };
        assert_eq!(
            (&out[12..16], &out[16..20], &out[IP_HEAD..IP_HEAD + 4]),
            (&LTE.octets()[..], &FAR.octets()[..], &[0x13, 0x88, 0x01, 0xBB][..]),
            "from the connection's address, the host's port kept"
        );
        machine.take_lte(&udp(FAR, LTE, (443, 5000), b"back"), second(3));
        let steps = drain(&mut machine, &crypto, second(3));
        let [Step::ToHost(back)] = &steps[..] else { panic!("not one frame: {steps:?}") };
        assert_eq!(
            (&back[..6], &back[14 + 12..14 + 16], &back[14 + 16..14 + 20]),
            (&HOST_HARDWARE[..], &FAR.octets()[..], &HOST.octets()[..]),
            "the answer, for the host's hardware address"
        );
        machine.take_lte(&udp(FAR, LTE, (443, 5001), b"unasked"), second(3));
        assert_eq!(drain(&mut machine, &crypto, second(3)), [], "what nobody asked for is dropped");
        machine.take_frame(&crypto, &frame(&udp(HOST, GATEWAY, (5300, 53), b"lookup")), second(3), UTC);
        let steps = drain(&mut machine, &crypto, second(3));
        let [Step::ToOwn(lookup)] = &steps[..] else { panic!("not one packet: {steps:?}") };
        assert_eq!(
            &lookup[16..20],
            &OWN_ADDRESS.octets()[..],
            "the host's lookups go to the relay, on the device itself"
        );
        machine.take_own(&udp(OWN_ADDRESS, HOST, (53, 5300), b"answer"), second(3));
        assert!(matches!(&drain(&mut machine, &crypto, second(3))[..], [Step::ToHost(_)]), "and its answers back");
        let mut other = frame(&udp(HOST, FAR, (5000, 443), b"out"));
        other[12..14].copy_from_slice(&[0x86, 0xDD]);
        machine.take_frame(&crypto, &other, second(3), UTC);
        assert_eq!(drain(&mut machine, &crypto, second(3)), [], "the host's link carries no IPv6");
    }

    #[test]
    fn off_forwards_nothing() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, false, None);
        drain(&mut machine, &crypto, second(1));
        machine.take_wishes(&crypto, wishes(false, false), second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [Step::Release, Step::Relay(false)]);
        machine.take_frame(&crypto, &frame(&udp(HOST, FAR, (5000, 443), b"out")), second(2), UTC);
        assert_eq!(drain(&mut machine, &crypto, second(2)), [], "a host on a device that is no modem is not heard");
    }

    // Every step a host's frames come to while the tunnel is wanted, whatever stands of it.
    fn steps_of_host_frames(machine: &mut Machine, crypto: &Toy, now: Duration) -> Vec<Step> {
        for target in [FAR, GATEWAY, Ipv4Addr::new(1, 1, 1, 1), LTE, OWN_ADDRESS] {
            for port in [53, 443, 853] {
                machine.take_frame(crypto, &frame(&udp(HOST, target, (5000, port), b"out")), now, UTC + now);
            }
        }
        drain(machine, crypto, now)
    }

    #[test]
    fn tunnel_only_path() {
        let crypto = Toy::default();
        let no_plain_way = |steps: &[Step]| !steps.iter().any(|step| matches!(step, Step::ToLte(_) | Step::ToOwn(_)));
        let mut unset = routing(&crypto, true, None);
        assert_eq!(drain(&mut unset, &crypto, second(1)), [Step::Watch, Step::OpenLte(0x0800)], "and no relay");
        assert_eq!(unset.standing(second(1)), Standing::Unset);
        assert_eq!(steps_of_host_frames(&mut unset, &crypto, second(2)), [], "no config kept");
        let mut named = routing(&crypto, true, Some(named_config()));
        assert_eq!(
            drain(&mut named, &crypto, second(1)),
            [Step::Watch, Step::OpenLte(0x0800), Step::Resolve("vpn.example.org".into())]
        );
        assert_eq!(steps_of_host_frames(&mut named, &crypto, second(2)), [], "the peer's address not found yet");
        let unfound = Answer::Endpoint(7, Err("no such name".into()));
        named.take_wishes(&crypto, Wishes { answers: vec![unfound], ..wishes(true, true) }, second(3));
        assert_eq!(named.standing(second(3)), Standing::Shut);
        assert_eq!(steps_of_host_frames(&mut named, &crypto, second(4)), [], "nor found at all");
        assert_eq!(
            steps_of_host_frames(&mut named, &crypto, second(8)),
            [Step::Resolve("vpn.example.org".into())],
            "five seconds on, the host's packets have it looked up anew, and are dropped still"
        );
        let found = Answer::Endpoint(7, Ok(vec![IpAddr::from([203, 0, 113, 5]), "2001:db8::5".parse().unwrap()]));
        named.take_wishes(&crypto, Wishes { answers: vec![found], ..wishes(true, true) }, second(9));
        assert_eq!(
            drain(&mut named, &crypto, second(9)),
            [Step::Connect(vec!["203.0.113.5:51820".parse().unwrap(), "[2001:db8::5]:51820".parse().unwrap()])]
        );
        assert_eq!(steps_of_host_frames(&mut named, &crypto, second(9)), [], "the socket not open yet");
        named.take_connected(&crypto, Err("the network is unreachable".into()), second(9), UTC);
        assert_eq!(steps_of_host_frames(&mut named, &crypto, second(10)), [], "nor opened at all");
        let mut machine = routing(&crypto, true, Some(config()));
        assert_eq!(
            drain(&mut machine, &crypto, second(1)),
            [Step::Watch, Step::OpenLte(0x0800), Step::Connect(vec!["203.0.113.5:51820".parse().unwrap()])],
            "a peer told by its address is looked up nowhere"
        );
        machine.take_connected(&crypto, Ok(()), second(1), UTC + second(1));
        let steps = drain(&mut machine, &crypto, second(1));
        assert!(matches!(&steps[..], [Step::ToPeer(first)] if first.len() == 148), "the handshake begun at once");
        assert_eq!(machine.standing(second(1)), Standing::Shut, "no session yet");
        let steps = steps_of_host_frames(&mut machine, &crypto, second(2));
        assert!(no_plain_way(&steps), "while the handshake is under way: {steps:?}");
        machine.take_down(&crypto, second(3));
        assert_eq!(drain(&mut machine, &crypto, second(3)), [Step::Disconnect, Step::Close, Step::Release, Step::Hold]);
        assert_eq!(steps_of_host_frames(&mut machine, &crypto, second(3)), [], "nor without a stay");
    }

    #[test]
    fn tunnel_switch_drops_flows() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, false, Some(config()));
        drain(&mut machine, &crypto, second(1));
        machine.take_frame(&crypto, &frame(&udp(HOST, FAR, (5000, 443), b"out")), second(2), UTC);
        assert!(matches!(&drain(&mut machine, &crypto, second(2))[..], [Step::ToLte(_)]));
        machine.take_wishes(&crypto, wishes(true, true), second(3));
        assert_eq!(
            drain(&mut machine, &crypto, second(3)),
            [Step::Relay(false), Step::Connect(vec!["203.0.113.5:51820".parse().unwrap()])],
            "the relay serves no lookup while the tunnel is wanted"
        );
        machine.take_lte(&udp(FAR, LTE, (443, 5000), b"back"), second(3));
        assert_eq!(drain(&mut machine, &crypto, second(3)), [], "the flow that left over LTE takes nothing more");
        machine.take_wishes(&crypto, wishes(true, false), second(4));
        assert_eq!(drain(&mut machine, &crypto, second(4)), [Step::Relay(true), Step::Disconnect]);
        assert_eq!(machine.standing(second(4)), Standing::Lte);
    }

    #[test]
    fn config_kept_and_forgotten() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, true, None);
        drain(&mut machine, &crypto, second(1));
        let order = Order::Keep(config(), Asker::Console);
        machine.take_wishes(&crypto, Wishes { orders: vec![order], ..wishes(true, true) }, second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [Step::Keep(config())]);
        let shown = alloc::format!("{:?}", Step::Keep(config()));
        assert!(!shown.contains("ERER"), "a step tells no key where it is formatted: {shown}");
        assert!(!machine.has_config(), "kept once its file is written");
        machine.take_kept(&crypto, Err("no room".into()), second(2));
        assert_eq!(machine.take_stored(), Some((Asker::Console, Err("no room".into()))));
        assert!(!machine.has_config() && drain(&mut machine, &crypto, second(2)).is_empty());
        let order = Order::Keep(config(), Asker::Window);
        machine.take_wishes(&crypto, Wishes { orders: vec![order], ..wishes(true, true) }, second(3));
        drain(&mut machine, &crypto, second(3));
        machine.take_kept(&crypto, Ok(()), second(3));
        assert_eq!((machine.take_stored(), machine.take_stored()), (Some((Asker::Window, Ok(()))), None));
        assert!(machine.has_config());
        assert_eq!(
            drain(&mut machine, &crypto, second(3)),
            [Step::Connect(vec!["203.0.113.5:51820".parse().unwrap()])],
            "and the tunnel is set up with it"
        );
        machine.take_wishes(&crypto, Wishes { orders: vec![Order::Forget], ..wishes(true, true) }, second(4));
        assert_eq!(drain(&mut machine, &crypto, second(4)), [Step::Forget]);
        machine.take_forgotten(&crypto, false, second(4));
        assert!(machine.has_config(), "a file that is not gone is a config kept");
        machine.take_forgotten(&crypto, true, second(4));
        assert_eq!(drain(&mut machine, &crypto, second(4)), [Step::Disconnect]);
        assert_eq!((machine.has_config(), machine.standing(second(4))), (false, Standing::Unset));
    }

    #[test]
    fn network_of_ipv6_alone_translated() {
        let crypto = Toy::default();
        let mut machine = Machine::new(HARDWARE, None);
        machine.take_wishes(&crypto, wishes(true, false), second(1));
        machine.take_link(&crypto, true, second(1));
        drain(&mut machine, &crypto, second(1));
        machine.take_hold(&crypto, Ok((7, Some(Routed { address: None, mtu: 1500 }))), second(1));
        assert_eq!(drain(&mut machine, &crypto, second(1)), [Step::Watch, Step::Translate]);
        assert_eq!(machine.standing(second(1)), Standing::Joining, "no way before the translation is known");
        let kernel: Ipv6Addr = "2001:db8:1:2::1".parse().unwrap();
        let other = Answer::Translation(6, Ok(Some((kernel, prefix("64:ff9b::", 96)))));
        machine.take_wishes(&crypto, Wishes { answers: vec![other], ..wishes(true, false) }, second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [], "an answer for a stay before");
        let found = Answer::Translation(7, Ok(Some((kernel, prefix("64:ff9b::", 96)))));
        machine.take_wishes(&crypto, Wishes { answers: vec![found], ..wishes(true, false) }, second(2));
        assert_eq!(drain(&mut machine, &crypto, second(2)), [Step::OpenLte(0x86DD), Step::Relay(true)]);
        machine.take_frame(&crypto, &frame(&udp(HOST, FAR, (5000, 443), b"out")), second(3), UTC);
        let steps = drain(&mut machine, &crypto, second(3));
        let [Step::ToLte(out)] = &steps[..] else { panic!("not one packet: {steps:?}") };
        assert_eq!(
            (out[0] >> 4, &out[8..16], &out[24..40]),
            (6, &kernel.octets()[..8], &"64:ff9b::5db8:d822".parse::<Ipv6Addr>().unwrap().octets()[..]),
            "as IPv6, from an address of the connection's network to the far end behind the NAT64"
        );
        let mut back = out.clone();
        let (from, to) = (out[8..24].to_vec(), out[24..40].to_vec());
        back[8..24].copy_from_slice(&to);
        back[24..40].copy_from_slice(&from);
        back[40..44].copy_from_slice(&[0x01, 0xBB, 0x13, 0x88]);
        machine.take_lte(&back, second(3));
        assert!(matches!(&drain(&mut machine, &crypto, second(3))[..], [Step::ToHost(_)]), "and back as IPv4");
        let mut silent = Machine::new(HARDWARE, None);
        silent.take_wishes(&crypto, wishes(true, false), second(1));
        silent.take_link(&crypto, true, second(1));
        silent.take_hold(&crypto, Ok((7, Some(Routed { address: None, mtu: 1500 }))), second(1));
        drain(&mut silent, &crypto, second(1));
        let none = Answer::Translation(7, Ok(None));
        silent.take_wishes(&crypto, Wishes { answers: vec![none], ..wishes(true, false) }, second(2));
        assert_eq!(drain(&mut silent, &crypto, second(2)), [], "a network that tells no NAT64 gives the host no way");
    }

    #[test]
    fn waits_for_what_is_due() {
        let crypto = Toy::default();
        let mut machine = routing(&crypto, false, None);
        drain(&mut machine, &crypto, second(1));
        assert_eq!(machine.next(&crypto, second(1), UTC), Step::Wait(None), "nothing kept, nothing due");
        machine.take_frame(&crypto, &frame(&udp(HOST, FAR, (5000, 443), b"out")), second(2), UTC);
        drain(&mut machine, &crypto, second(2));
        assert_eq!(
            machine.next(&crypto, second(2), UTC),
            Step::Wait(Some(second(59))),
            "a flow kept: the next look for flows nothing passes over"
        );
        assert_eq!(machine.next(&crypto, second(400), UTC), Step::Wait(None), "the flow's time over, it is gone");
    }
}
