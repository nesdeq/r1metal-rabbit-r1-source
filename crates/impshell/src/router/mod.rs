mod relay;

use crate::crypto::Library;
use crate::net::{dial, names};
use crate::storage::Data;
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake, iface};
use crate::wifi::radio::{Hold, NetworkDown, Online};
use crate::{log, usb};
use r1_funcore::cell::link::OWN_INTERFACE;
use r1_funcore::domain::router::{Answer, DIRECTORY, Machine, Order, Report, State, Step, read_kept};
use r1_funcore::net::MTU;
use r1_funcore::net::ipv6::OFF_WRITES;
use r1_funcore::net::wireguard::config::{Config, FILE};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const NUDGE: u64 = 0;
const LINK: u64 = 1;
const HOST: u64 = 2;
const OWN: u64 = 3;
const DOWN: u64 = 4;
const LTE: u64 = 5;
const PEER: u64 = 6;
const TAG: &str = "router";

struct Shared {
    board: Board<State>,
    nudge: Wake,
}

impl Shared {
    fn post(&self, answer: Answer) {
        self.board.lock().add_answer(answer);
        self.nudge.raise();
    }
}

fn clocks() -> (Duration, Duration) {
    (Duration::from_nanos(sys::boottime_ns()), Duration::from_millis(sys::realtime_ms()))
}

// Writes a packet where it goes; one there is no room for now is dropped, as a link drops it.
fn send(fd: RawFd, packet: &[u8], target: &str) {
    let written = sys::check_count(unsafe { libc::write(fd, packet.as_ptr().cast(), packet.len()) });
    if let Err(e) = written
        && e.kind() != io::ErrorKind::WouldBlock
        && e.raw_os_error() != Some(libc::ENOBUFS)
    {
        log!("router: a packet for {target}: {e}");
    }
}

// Reads what a descriptor that never waits holds, a packet a call, and hands each on; a read that fails ends it, and is told unless nothing is left to read.
fn drain(fd: RawFd, source: &str, mut take: impl FnMut(&[u8])) {
    // Room for the largest packet of a link and its head.
    let mut packet = [0u8; MTU + sys::PAGE];
    loop {
        match sys::check_count(unsafe { libc::read(fd, packet.as_mut_ptr().cast(), packet.len()) }) {
            Ok(count) => take(&packet[..count]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) => return log!("router: reading {source}: {e}"),
        }
    }
}

fn read_config(data: &Data) -> Option<Config> {
    let text = data.text(DIRECTORY, FILE).inspect_err(|e| log!("router: reading the tunnel's config: {e}")).ok()??;
    read_kept(&text).inspect_err(|line| log!("{line}")).ok()
}

// Opens a socket toward the first of the peer's addresses the kernel has a way to.
fn connect(addresses: &[SocketAddr]) -> io::Result<UdpSocket> {
    let mut failure = io::Error::from(io::ErrorKind::NotFound);
    for address in addresses {
        match dial::open_datagrams(address) {
            Ok(socket) => return Ok(socket),
            Err(e) => failure = e,
        }
    }
    Err(failure)
}

struct Worker<'a> {
    shared: &'a Arc<Shared>,
    data: &'a Data,
    online: &'a Online,
    epoll: Epoll,
    interface: CString,
    events: OwnedFd,
    host: OwnedFd,
    own: File,
    hold: Option<Hold>,
    down: Option<NetworkDown>,
    lte: Option<OwnedFd>,
    peer: Option<UdpSocket>,
    relay: Option<Arc<Wake>>,
}

impl Worker<'_> {
    fn unwatch(&mut self, fd: RawFd, name: &str) {
        if let Err(e) = self.epoll.remove(fd) {
            log!("router: ending the watch of {name}: {e}");
        }
    }

    fn watch_down(&mut self) {
        let down = self.hold.as_ref().map(|hold| hold.down().clone());
        if let Some(Err(e)) = down.as_ref().map(|down| self.epoll.add(down.fd(), DOWN)) {
            log!("router: watching the stay's end: {e}");
        }
        self.down = down;
    }

    fn open_lte(&mut self, kind: u16) -> io::Result<()> {
        let down = self.down.as_ref().ok_or_else(|| io::Error::other("no stay"))?;
        let socket = iface::open_packets_on(&CString::new(down.interface())?, kind, libc::SOCK_DGRAM)?;
        self.epoll.add(socket.as_raw_fd(), LTE)?;
        self.lte = Some(socket);
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(peer) = self.peer.take() {
            self.unwatch(peer.as_raw_fd(), "the tunnel's socket");
        }
    }

    fn stop_relay(&mut self) {
        self.relay.take().iter().for_each(|stop| stop.raise());
    }

    fn close(&mut self) {
        self.disconnect();
        self.stop_relay();
        if let Some(lte) = self.lte.take() {
            self.unwatch(lte.as_raw_fd(), "the modem's interface");
        }
        if let Some(down) = self.down.take() {
            self.unwatch(down.fd(), "the stay's end");
        }
    }

    // Starts a thread beside the forwarder under a share of the hold, so that the stay stands while it works.
    fn spawn(&self, name: &str, work: impl FnOnce(&Arc<Shared>, &Hold) + Send + 'static) {
        let Some(hold) = self.hold.as_ref().map(Hold::share) else {
            return log!("router: no stay for the thread {name}");
        };
        let shared = Arc::clone(self.shared);
        if let Err(e) = thread::Builder::new().name(name.into()).spawn(move || work(&shared, &hold)) {
            log!("router: the thread {name}: {e}");
        }
    }

    fn start_relay(&mut self) {
        let stop = match Wake::new() {
            Ok(stop) => Arc::new(stop),
            Err(e) => return log!("names: the relay's stop: {e}"),
        };
        let stopped = Arc::clone(&stop);
        self.spawn("relay", move |_, hold| {
            if let Err(e) = relay::run(hold, &stopped) {
                log!("names: the relay ends: {e}");
            }
        });
        self.relay = Some(stop);
    }

    fn take_ready(&self, machine: &mut Machine, token: u64) {
        let (now, utc) = clocks();
        match token {
            NUDGE => {
                self.shared.nudge.clear();
                let wishes = self.shared.board.lock().take_wishes();
                machine.take_wishes(&Library, wishes, now);
            }
            LINK => {
                iface::drain(&self.events);
                match iface::is_running(&self.interface) {
                    Ok(is_up) => machine.take_link(&Library, is_up, now),
                    Err(e) => log!("router: reading the host's link: {e}"),
                }
            }
            HOST => drain(self.host.as_raw_fd(), "the host's link", |frame| {
                machine.take_frame(&Library, frame, now, utc);
            }),
            OWN => drain(self.own.as_raw_fd(), "the device's own packets", |packet| machine.take_own(packet, now)),
            DOWN => machine.take_down(&Library, now),
            LTE => {
                let lte = self.lte.as_ref().map(AsRawFd::as_raw_fd);
                lte.into_iter()
                    .for_each(|lte| drain(lte, "the modem's interface", |packet| machine.take_lte(packet, now)));
            }
            _ => {
                let peer = self.peer.as_ref().map(AsRawFd::as_raw_fd);
                peer.into_iter().for_each(|peer| {
                    drain(peer, "the tunnel's socket", |datagram| {
                        machine.take_datagram(&Library, datagram, now, utc);
                    });
                });
            }
        }
    }

    fn wait(&mut self, machine: &mut Machine, limit: Option<Duration>) -> io::Result<()> {
        if self.shared.board.lock().note(machine, clocks().0) {
            self.shared.board.raise();
        }
        for token in self.epoll.wait(limit)? {
            self.take_ready(machine, token);
        }
        Ok(())
    }

    fn act(&mut self, machine: &mut Machine, step: Step) -> io::Result<()> {
        let (now, utc) = clocks();
        match step {
            Step::Hold => {
                let held = self.online.hold().map(|hold| {
                    let stay = (hold.down().stay(), hold.down().routed());
                    self.hold = Some(hold);
                    stay
                });
                machine.take_hold(&Library, held.map_err(|fault| fault.to_string()), clocks().0);
            }
            Step::Release => self.hold = None,
            Step::Watch => self.watch_down(),
            Step::OpenLte(kind) => {
                if let Err(e) = self.open_lte(kind) {
                    log!("router: opening the modem's interface: {e}");
                }
            }
            Step::Close => self.close(),
            Step::ToHost(frame) => send(self.host.as_raw_fd(), &frame, "the host"),
            Step::ToOwn(packet) => send(self.own.as_raw_fd(), &packet, "the device itself"),
            Step::ToLte(packet) => {
                self.lte.iter().for_each(|lte| send(lte.as_raw_fd(), &packet, "the modem's interface"));
            }
            Step::ToPeer(datagram) => {
                self.peer.iter().for_each(|peer| send(peer.as_raw_fd(), &datagram, "the tunnel's peer"));
            }
            Step::Translate => self.spawn("router translation", |shared, hold| {
                let found = dial::find_translation(hold.down()).map_err(|e| e.to_string());
                shared.post(Answer::Translation(hold.down().stay(), found));
            }),
            Step::Resolve(host) => self.spawn("router names", move |shared, hold| {
                let found = names::resolve_all(&[&host], hold.down()).swap_remove(0).map_err(|e| e.to_string());
                shared.post(Answer::Endpoint(hold.down().stay(), found));
            }),
            Step::Connect(addresses) => {
                self.disconnect();
                let connected = connect(&addresses).and_then(|peer| {
                    self.epoll.add(peer.as_raw_fd(), PEER)?;
                    self.peer = Some(peer);
                    Ok(())
                });
                machine.take_connected(&Library, connected.map_err(|e| e.to_string()), now, utc);
            }
            Step::Disconnect => self.disconnect(),
            Step::Relay(true) => self.start_relay(),
            Step::Relay(false) => self.stop_relay(),
            Step::Keep(config) => {
                let kept = self.data.put(DIRECTORY, FILE, config.to_text().as_bytes());
                machine.take_kept(&Library, kept.map_err(|e| e.to_string()), now);
            }
            Step::Forget => {
                let forgotten = self.data.remove(DIRECTORY, FILE);
                if let Err(e) = &forgotten {
                    log!("router: taking the tunnel's config away: {e}");
                }
                machine.take_forgotten(&Library, forgotten.is_ok(), now);
            }
            Step::Log(line) => log!("{line}"),
            Step::Wait(limit) => self.wait(machine, limit)?,
        }
        Ok(())
    }
}

fn work(shared: &Arc<Shared>, data: &Data, online: &Online) -> io::Result<()> {
    let own_name = CString::new(OWN_INTERFACE)?;
    let own = iface::open_tun(&own_name)?;
    iface::write_ipv6(&own_name, TAG, &OFF_WRITES);
    let interface = CString::new(usb::host_interface()?)?;
    // The host's link carries IPv4 alone, and the kernel sends nothing of its own over it.
    iface::write_ipv6(&interface, TAG, &OFF_WRITES);
    let hardware = iface::hardware_address(&interface)?;
    iface::index(&interface, true)?;
    let host = iface::open_packets_on(&interface, iface::EVERY_PROTOCOL, libc::SOCK_RAW)?;
    let events = iface::route_events(libc::RTMGRP_LINK)?;
    let mut epoll = Epoll::new()?;
    epoll.add(shared.nudge.fd(), NUDGE)?;
    epoll.add(events.as_raw_fd(), LINK)?;
    epoll.add(host.as_raw_fd(), HOST)?;
    epoll.add(own.as_raw_fd(), OWN)?;
    let kept = data.wait_decided().then(|| read_config(data)).flatten();
    let mut machine = Machine::new(hardware, kept);
    machine.take_link(&Library, iface::is_running(&interface)?, clocks().0);
    let mut worker = Worker {
        shared,
        data,
        online,
        epoll,
        interface,
        events,
        host,
        own,
        hold: None,
        down: None,
        lte: None,
        peer: None,
        relay: None,
    };
    loop {
        let (now, utc) = clocks();
        let step = machine.next(&Library, now, utc);
        worker.act(&mut machine, step)?;
    }
}

/// The main loop's handle of the router's thread, which lends a host on the cable the modem's connection.
pub struct Router {
    shared: Arc<Shared>,
}

impl Router {
    /// Starts the thread, which keeps the tunnel's config on `data` and holds the network through `online`.
    pub fn start(data: Data, online: Online) -> io::Result<Router> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, nudge: Wake::new()? });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name(TAG.into()).spawn(move || {
            if let Err(e) = work(&worker, &data, &online) {
                log!("router: the thread ends: {e}");
            }
            worker.board.lock().end_thread();
            worker.board.raise();
        })?;
        Ok(Router { shared })
    }

    /// Readable when the router's report has news, or the radio is to be told anew whether stays are routed.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets whether a host on the cable is lent the modem's connection, and whether its packets go through the tunnel alone; the thread follows.
    pub fn want(&self, is_wanted: bool, uses_tunnel: bool) {
        if self.shared.board.lock().want(is_wanted, uses_tunnel) {
            self.shared.nudge.raise();
        }
    }

    /// Hands the thread an order: a config to keep, or the kept one to forget.
    pub fn ask(&self, order: Order) {
        self.shared.board.lock().order(order);
        self.shared.nudge.raise();
    }

    /// Takes the thread's report.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }

    /// True while a host's link has every stay made over the modem and routed.
    pub fn is_routing(&self) -> bool {
        self.shared.board.lock().is_routing()
    }

    /// True while no host is served and no order waits.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::router::Standing;
    use std::net::Ipv4Addr;

    #[test]
    fn no_adapter_nothing_routed() {
        let radio = without_networks();
        let router = Router::start(Data::unusable(), radio.online()).unwrap();
        let mut epoll = Epoll::with_fd(router.fd());
        router.want(true, false);
        let mut standing = None;
        while standing != Some(Standing::Absent) {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the thread tells how it stands");
            standing = router.latest().standing.or(standing);
        }
        assert!(
            !router.is_routing() && router.is_off(),
            "here without the gadget's network function, or the leave to make an interface, no stay is ever routed"
        );
    }

    #[test]
    fn peer_first_reachable_address() {
        let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let peer = connect(&[address]).unwrap();
        peer.send(b"word").unwrap();
        let mut word = [0u8; 8];
        assert_eq!(listener.recv(&mut word).unwrap(), 4, "a datagram for the peer reaches it");
        let mut count = 0;
        drain(peer.as_raw_fd(), "the peer", |_| count += 1);
        assert_eq!(count, 0, "a socket that holds nothing gives nothing, and does not wait");
        listener.send_to(b"answer", peer.local_addr().unwrap()).unwrap();
        let mut answers = Vec::new();
        while answers.is_empty() {
            drain(peer.as_raw_fd(), "the peer", |datagram| answers.push(datagram.to_vec()));
        }
        assert_eq!(answers, [b"answer".to_vec()], "what the peer sends is read whole, a datagram a call");
        assert_eq!(connect(&[]).map(drop).map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
    }
}
