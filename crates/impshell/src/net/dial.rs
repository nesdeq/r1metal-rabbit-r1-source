use crate::sys::{self, Wake, iface, lock};
use crate::wifi::radio::NetworkDown;
use crate::{crypto, log};
use r1_funcore::net::dns::nat64::{self, ID_BYTES, Prefix, Reach, Search};
use r1_funcore::net::race::{NOTHING_TO_ASK, NOTHING_TO_CONNECT, Race, Step, TIMEOUT};
use r1_funcore::net::{MTU, ipv6};
use std::ffi::CString;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const ADDRESSES: &str = "/proc/net/if_inet6";

fn silence_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "the other side is silent")
}

// Waits until one of `sockets` is ready or `deadline` comes, true where one is; fails at once where the stay `down` has ended.
pub(super) fn wait(sockets: &mut Vec<libc::pollfd>, down: &NetworkDown, deadline: Option<Instant>) -> io::Result<bool> {
    sockets.push(sys::readable(down.fd()));
    let result = sys::poll(sockets, deadline).map(|count| count > 0);
    let is_down = sockets.pop().is_some_and(|down| down.revents != 0);
    if is_down { Err(io::Error::new(io::ErrorKind::NetworkDown, "the network is gone")) } else { result }
}

// Waits for the asking another has under way, which wakes the waiter `add` hands `state` before its lock is let go; fails where the stay `down` ends first.
pub(crate) fn await_turn<T>(
    mut state: MutexGuard<'_, T>,
    add: impl FnOnce(&mut T, Arc<Wake>),
    down: &NetworkDown,
) -> io::Result<()> {
    let waiter = Arc::new(Wake::new()?);
    add(&mut state, Arc::clone(&waiter));
    drop(state);
    wait(&mut vec![sys::readable(waiter.fd())], down, None).map(drop)
}

pub(super) struct Wire {
    tcp: TcpStream,
    pub(super) down: NetworkDown,
    pub(super) timeout: Duration,
}

impl Wire {
    pub(super) fn peer(&self) -> String {
        self.tcp.peer_addr().map_or_else(|e| format!("an address unknown ({e})"), |peer| peer.ip().to_string())
    }

    pub(super) fn fd(&self) -> RawFd {
        self.tcp.as_raw_fd()
    }

    fn wait(&self, events: libc::c_short) -> io::Result<()> {
        let mut socket = vec![libc::pollfd { fd: self.tcp.as_raw_fd(), events, revents: 0 }];
        if wait(&mut socket, &self.down, Some(Instant::now() + self.timeout))? { Ok(()) } else { Err(silence_error()) }
    }

    pub(super) fn is_readable(&self) -> io::Result<bool> {
        sys::poll(&mut [sys::readable(self.tcp.as_raw_fd())], Some(Instant::now())).map(|ready| ready > 0)
    }
}

impl Read for Wire {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.tcp.read(buffer) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(libc::POLLIN)?,
                result => return result,
            }
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            match self.tcp.write(buffer) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(libc::POLLOUT)?,
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn to_wire(socket: OwnedFd, down: &NetworkDown) -> io::Result<Wire> {
    let tcp = TcpStream::from(socket);
    // What the device sends is short and waits for an answer: it goes out at once.
    tcp.set_nodelay(true)?;
    Ok(Wire { tcp, down: down.clone(), timeout: TIMEOUT })
}

// A question sent to one address over UDP, and when.
struct Query {
    socket: UdpSocket,
    address: SocketAddr,
    sent: Instant,
}

impl AsRawFd for Query {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

/// A socket for datagrams to and from `address` alone, on a port of the kernel's choice, whose calls never wait.
pub fn open_datagrams(address: &SocketAddr) -> io::Result<UdpSocket> {
    let local: SocketAddr =
        if address.is_ipv4() { (Ipv4Addr::UNSPECIFIED, 0).into() } else { (Ipv6Addr::UNSPECIFIED, 0).into() };
    let socket = UdpSocket::bind(local)?;
    socket.set_nonblocking(true)?;
    socket.connect(address)?;
    Ok(socket)
}

fn begin_query(address: &SocketAddr, question: &[u8]) -> io::Result<Query> {
    let socket = open_datagrams(address)?;
    socket.send(question)?;
    Ok(Query { socket, address: *address, sent: Instant::now() })
}

fn global_address(interface: &str) -> Option<Ipv6Addr> {
    std::fs::read_to_string(ADDRESSES).ok().and_then(|text| ipv6::find_usable_global(&text, interface))
}

/// The addresses the device holds on the stay's interface for a far side to reach it at: its IPv4 address and a global IPv6 one, each where it has one.
pub fn local_addresses(down: &NetworkDown) -> Vec<IpAddr> {
    let ipv4 = CString::new(down.interface()).ok().and_then(|name| iface::ipv4(&name).ok().flatten());
    let ipv6 = global_address(down.interface());
    ipv4.map(IpAddr::from).into_iter().chain(ipv6.map(IpAddr::from)).collect()
}

// Waits until the stay's interface has a global IPv6 address, or until `deadline`: the address once it has one.
fn await_routes(down: &NetworkDown, deadline: Option<Instant>) -> io::Result<Option<Ipv6Addr>> {
    // The news is asked for before the look so that no change between the two is missed; an announcement brings the way first, the address after.
    let events = iface::route_events(libc::RTMGRP_IPV6_IFADDR | libc::RTMGRP_IPV6_ROUTE)?;
    loop {
        let address = global_address(down.interface());
        if address.is_some() || !wait(&mut vec![sys::readable(events.as_raw_fd())], down, deadline)? {
            return Ok(address);
        }
        iface::drain(&events);
    }
}

// Races attempts at the addresses as `Race` has them (RFC 8305): `begin` starts one, which the wait looks at for `events`, and `take` reads one it found ready, which has done, has failed, or goes on (none); returns the first that has done, with what was read of it, or why none did, `nothing` where no address was given.
fn race<A: AsRawFd, T>(
    addresses: &[SocketAddr],
    events: libc::c_short,
    nothing: &str,
    down: &NetworkDown,
    mut begin: impl FnMut(&SocketAddr) -> io::Result<A>,
    mut take: impl FnMut(&A) -> Option<io::Result<T>>,
) -> io::Result<(A, T)> {
    let (start, mut race) = (Instant::now(), Race::new(addresses));
    let (mut attempts, mut last_error): (Vec<A>, _) = (Vec::new(), io::Error::other(nothing));
    loop {
        let end = match race.next_step(start.elapsed()) {
            Step::Begin(address) => {
                match begin(&address) {
                    Ok(attempt) => {
                        attempts.push(attempt);
                        race.note_begun(start.elapsed());
                    }
                    Err(e) => {
                        if e.raw_os_error() == Some(libc::ENETUNREACH) {
                            race.add_unrouted(address);
                        }
                        last_error = e;
                    }
                }
                continue;
            }
            Step::AwaitRoutes(limit) => {
                if await_routes(down, Some(start + limit))?.is_none() {
                    return Err(last_error);
                }
                race.note_rerouted();
                continue;
            }
            Step::Failed => return Err(last_error),
            Step::Silent => return Err(silence_error()),
            Step::Wait(end) => end,
        };
        let mut sockets: Vec<libc::pollfd> =
            attempts.iter().map(|attempt| libc::pollfd { fd: attempt.as_raw_fd(), events, revents: 0 }).collect();
        if !wait(&mut sockets, down, Some(start + end))? {
            race.note_quiet(start.elapsed());
            continue;
        }
        let mut going = Vec::with_capacity(attempts.len());
        for (attempt, socket) in attempts.drain(..).zip(&sockets) {
            let outcome = if socket.revents != 0 { take(&attempt) } else { None };
            match outcome {
                Some(Ok(value)) => return Ok((attempt, value)),
                Some(Err(e)) => {
                    last_error = e;
                    race.note_failed(start.elapsed());
                }
                None => going.push(attempt),
            }
        }
        attempts = going;
    }
}

pub(super) fn connect(addresses: &[SocketAddr], down: &NetworkDown) -> io::Result<Wire> {
    let take = |socket: &OwnedFd| Some(sys::connect_result(socket.as_raw_fd()));
    let (socket, ()) =
        race(&reachable(addresses, down)?, libc::POLLOUT, NOTHING_TO_CONNECT, down, sys::begin_connect, take)?;
    to_wire(socket, down)
}

/// Sends `question` to those of `addresses` the stay reaches in turn until one answers; returns the value `parse` takes from the answer, its sender and the round-trip time. Every address that refused it ends it at once.
pub fn exchange<T>(
    addresses: &[SocketAddr],
    question: &[u8],
    parse: impl Fn(&[u8]) -> Option<T>,
    down: &NetworkDown,
) -> io::Result<(T, SocketAddr, Duration)> {
    ask(&reachable(addresses, down)?, question, parse, down)
}

fn ask<T>(
    addresses: &[SocketAddr],
    question: &[u8],
    parse: impl Fn(&[u8]) -> Option<T>,
    down: &NetworkDown,
) -> io::Result<(T, SocketAddr, Duration)> {
    let mut answer = [0u8; MTU];
    // What is no answer to the question is passed over; an address that refuses it has failed.
    let take = |query: &Query| match query.socket.recv(&mut answer) {
        Ok(count) => parse(&answer[..count]).map(Ok),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
        Err(e) => Some(Err(e)),
    };
    let begin = |address: &SocketAddr| begin_query(address, question);
    let (query, value) = race(addresses, libc::POLLIN, NOTHING_TO_ASK, down, begin, take)?;
    Ok((value, query.address, query.sent.elapsed()))
}

// What the network's resolvers tell of its NAT64's prefix, asked under a number drawn for the question; none where no answer came.
fn discover(resolvers: &[IpAddr], down: &NetworkDown) -> Option<Option<Prefix>> {
    let servers: Vec<SocketAddr> = resolvers.iter().map(|resolver| SocketAddr::new(*resolver, nat64::PORT)).collect();
    let answer = crypto::random::<ID_BYTES>()
        .ok_or_else(|| io::Error::other("no random bytes for the question's number"))
        .and_then(|id| ask(&servers, &nat64::to_question(id), |message| nat64::parse_prefix(message, id), down));
    match answer {
        Ok((word, resolver, time)) => {
            log!("{}", nat64::to_told_line(word, &resolver.ip().to_string(), time.as_millis()));
            Some(word)
        }
        Err(e) => {
            log!("{}", nat64::to_unfound_line(&e.to_string()));
            None
        }
    }
}

// The prefix of the NAT64 of a network of IPv6 alone as far as this asker goes: searched for, or waited for while another searches.
fn find_prefix(search: &Mutex<Search<Arc<Wake>>>, down: &NetworkDown) -> io::Result<Option<Prefix>> {
    let mut noted = None;
    loop {
        let mut state = lock(search);
        let step = state.next_step(&mut noted);
        match step {
            nat64::Step::Known(prefix) => return Ok(prefix),
            nat64::Step::Ask(resolvers) => {
                drop(state);
                let word = discover(&resolvers, down);
                let waiters = lock(search).end_asking(word);
                waiters.iter().for_each(|waiter| waiter.raise());
            }
            nat64::Step::Await => await_turn(state, Search::add_waiter, down)?,
        }
    }
}

/// What a routed stay in a network of IPv6 alone translates the host's packets by: the global address the kernel holds on the stay's interface, waited for until the stay ends, and the NAT64's prefix; none in a network that tells no NAT64, or one of IPv4.
pub fn find_translation(down: &NetworkDown) -> io::Result<Option<(Ipv6Addr, Prefix)>> {
    let Some(search) = down.nat64() else {
        return Ok(None);
    };
    let address = await_routes(down, None)?;
    let prefix = find_prefix(search, down)?;
    Ok(address.zip(prefix))
}

// The addresses the stay reaches of those given, an IPv4 one through the NAT64 of a network of IPv6 alone.
fn reachable(addresses: &[SocketAddr], down: &NetworkDown) -> io::Result<Vec<SocketAddr>> {
    let reach = match down.nat64() {
        Some(search) => Reach::Ipv6Alone(find_prefix(search, down)?),
        None => Reach::Direct,
    };
    Ok(nat64::to_reachable(addresses, reach))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::tests::{up, up_ipv6_alone};
    use r1_funcore::net::race::NEXT_ATTEMPT;
    use std::net::TcpListener;

    #[test]
    fn first_answer_wins() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let open = listener.local_addr().unwrap();
        // A port nothing listens on: bound and let go of.
        let closed = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .map(|listener| listener.local_addr().unwrap())
            .unwrap_or_else(|_| SocketAddr::from((Ipv4Addr::LOCALHOST, 1)));
        let start = Instant::now();
        let wire = connect(&[closed, open], &up()).unwrap();
        assert_eq!(wire.tcp.peer_addr().unwrap(), open);
        assert!(
            start.elapsed() < NEXT_ATTEMPT,
            "the refusal is news at once, and the next address is tried at once: {:?}",
            start.elapsed()
        );
        assert!(wire.tcp.nodelay().unwrap());
        assert_eq!(
            connect(&[open, closed], &up()).unwrap().tcp.peer_addr().unwrap(),
            open,
            "an address that answers first is taken whatever follows"
        );
        assert!(connect(&[closed], &up()).is_err() && connect(&[], &up()).is_err());
    }

    #[test]
    fn ipv4_through_nat64_alone() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let ipv4 = listener.local_addr().unwrap();
        assert!(connect(&[ipv4], &up()).is_ok(), "a network of IPv4 reaches it");
        assert_eq!(
            connect(&[ipv4], &up_ipv6_alone(Vec::new())).map(|_| ()).map_err(|e| e.to_string()),
            Err(String::from(NOTHING_TO_CONNECT)),
            "one of IPv6 alone whose NAT64 no resolver told passes it over"
        );
    }

    #[test]
    fn nat64_wait_ends_with_stay() {
        let down = up_ipv6_alone(Vec::new());
        let search = down.nat64().unwrap();
        assert_eq!(lock(search).next_step(&mut None), nat64::Step::Ask(Vec::new()), "another asker searches on");
        let waiting = std::thread::spawn({
            let down = down.clone();
            move || connect(&[SocketAddr::from((Ipv4Addr::LOCALHOST, 1))], &down).map(|_| ()).map_err(|e| e.kind())
        });
        down.raise();
        assert_eq!(
            waiting.join().unwrap(),
            Err(io::ErrorKind::NetworkDown),
            "a connection that waited for the search of a stay that ended waits no more"
        );
    }

    #[test]
    fn wait_ends_on_silence_or_down() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let down = up();
        let mut wire = connect(&[listener.local_addr().unwrap()], &down).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"word").unwrap();
        let mut buffer = [0u8; 8];
        assert_eq!(wire.read(&mut buffer).unwrap(), 4, "what has come is read");
        wire.timeout = Duration::from_millis(20);
        let start = Instant::now();
        assert_eq!(wire.read(&mut buffer).map_err(|e| e.kind()), Err(io::ErrorKind::TimedOut));
        assert!(start.elapsed() >= Duration::from_millis(20));
        down.raise();
        assert_eq!(
            wire.read(&mut buffer).map_err(|e| e.kind()),
            Err(io::ErrorKind::NetworkDown),
            "without the network no wait is begun"
        );
        drop(peer);
    }

    #[test]
    fn exchange_takes_first_answer() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        // A port nothing listens on: bound and let go of.
        let closed = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap();
        let server_thread = std::thread::spawn({
            let server = server.try_clone().unwrap();
            move || {
                let mut question = [0u8; 16];
                let (count, sender) = server.recv_from(&mut question).unwrap();
                server.send_to(b"no", sender).unwrap();
                server.send_to(&[&question[..count], b" yes"].concat(), sender).unwrap();
            }
        });
        let parse = |answer: &[u8]| answer.ends_with(b" yes").then(|| answer.to_vec());
        let (answer, sender, elapsed) =
            exchange(&[closed, server.local_addr().unwrap()], b"what time", parse, &up()).unwrap();
        assert_eq!(
            (answer.as_slice(), sender),
            (&b"what time yes"[..], server.local_addr().unwrap()),
            "what is no answer is passed over, and an address that refuses costs a moment"
        );
        assert!(elapsed < TIMEOUT);
        server_thread.join().unwrap();
        let start = Instant::now();
        assert_eq!(
            exchange(&[closed, closed], b"what time", parse, &up()).map_err(|e| e.kind()).err(),
            Some(io::ErrorKind::ConnectionRefused),
            "where every address refuses, the exchange ends"
        );
        assert!(start.elapsed() < NEXT_ATTEMPT, "at once, not at the end of the silence: {:?}", start.elapsed());
        let down = up();
        down.raise();
        assert_eq!(
            exchange(&[server.local_addr().unwrap()], b"what time", parse, &down).map_err(|e| e.kind()).err(),
            Some(io::ErrorKind::NetworkDown)
        );
        assert!(exchange(&[], b"what time", parse, &up()).is_err());
    }
}
