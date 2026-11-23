use crate::log;
use crate::net::dial::await_turn;
use crate::net::tls::Stream;
use crate::sys::{self, Wake, lock};
use crate::wifi::radio::NetworkDown;
use r1_funcore::net::dns::{
    self, Batch, BatchError, Lookup, NO_ANSWER, NoAddress, PORT, RESOLVER_NAME, RESOLVERS, State, Step, Unresolved,
    to_reached_line, to_relay_line, to_watch_line,
};
use r1_funcore::net::tls::Purpose;
use r1_funcore::net::watch::{self, BUFFER_BYTES, HEADER_BYTES};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Answers = io::Result<Vec<Result<(Vec<IpAddr>, u32), NoAddress>>>;
type Failure = (io::ErrorKind, String);

struct Resolver(Mutex<State<Failure, Arc<Wake>>>);

static RESOLVER: Resolver = Resolver::new();

fn to_failure(e: io::Error) -> Failure {
    (e.kind(), e.to_string())
}

fn log_lines(lookup: &mut Lookup<Failure>) {
    lookup.take_lines().iter().for_each(|line| log!("{line}"));
}

impl Resolver {
    const fn new() -> Resolver {
        Resolver(Mutex::new(State::new()))
    }

    // Each name's addresses in its place, those the cache does not keep asked for by `ask` in a turn of the stay `down`, one asking at a time.
    fn resolve(
        &self,
        names: &[&str],
        down: &NetworkDown,
        mut ask: impl FnMut(&[String]) -> Answers,
    ) -> Vec<Result<Vec<IpAddr>, Unresolved<Failure>>> {
        let mut lookup = lock(&self.0).begin(names, sys::boottime_seconds());
        loop {
            let mut state = lock(&self.0);
            let step = state.next_step(&mut lookup, down.stay(), sys::boottime_seconds());
            match step {
                Step::Done => break,
                Step::Await => {
                    if let Err(e) = await_turn(state, State::add_waiter, down) {
                        lookup.fail(to_failure(e));
                    }
                }
                Step::Ask(missing) => {
                    drop(state);
                    log_lines(&mut lookup);
                    let answers = ask(&missing).map_err(to_failure);
                    let now = sys::boottime_seconds();
                    let waiters = lock(&self.0).end_asking(&mut lookup, answers, down.stay(), now);
                    waiters.iter().for_each(|waiter| waiter.raise());
                }
            }
            log_lines(&mut lookup);
        }
        log_lines(&mut lookup);
        lookup.into_results()
    }
}

fn batch_error(reason: BatchError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason.message())
}

fn read_answers(stream: &mut Stream, batch: &mut Batch) -> io::Result<()> {
    while !batch.is_answered() {
        let mut length = [0u8; 2];
        stream.read_exact(&mut length)?;
        let mut message = vec![0u8; dns::message_length(length)];
        stream.read_exact(&mut message)?;
        batch.take_message(&message).ok_or_else(|| sys::invalid(NO_ANSWER))?;
    }
    Ok(())
}

struct PacketSocket(OwnedFd);

impl PacketSocket {
    fn open() -> io::Result<PacketSocket> {
        let socket = sys::iface::open_packets(sys::iface::EVERY_PROTOCOL, libc::SOCK_DGRAM)?;
        sys::set_option(&socket, libc::SOL_SOCKET, libc::SO_RCVBUFFORCE, &BUFFER_BYTES)?;
        Ok(PacketSocket(socket))
    }

    fn finish(self) -> Option<String> {
        let (mut watch, mut packet) = (watch::Watch::new(PORT), [0u8; HEADER_BYTES]);
        loop {
            let result = unsafe { libc::recv(self.0.as_raw_fd(), packet.as_mut_ptr().cast(), packet.len(), 0) };
            let Ok(length) = sys::check_count(result) else { break };
            watch.take_packet(&packet[..length]);
        }
        watch.to_log_line()
    }
}

// The watch is taken only where its line has a reader.
fn query_with_watch(names: &[String], down: &NetworkDown) -> Answers {
    if sys::klog::is_silent() {
        return query(names, down);
    }
    let socket = PacketSocket::open().inspect_err(|e| log!("names: no watch on the resolver's port: {e}")).ok();
    let answers = query(names, down);
    if let Some(line) = socket.and_then(PacketSocket::finish) {
        log!("{}", to_watch_line(&line));
    }
    answers
}

// A connection to the firmware's resolver over TLS, with the address it reached and how long the connection and the greeting took from `start`.
fn reach(down: &NetworkDown, start: Instant) -> io::Result<(Stream, String, Duration, Duration)> {
    let resolvers = RESOLVERS.map(|resolver| SocketAddr::new(resolver, PORT));
    let (stream, peer, connect_time) = Stream::reach(&resolvers, RESOLVER_NAME, Purpose::Names, start, down)?;
    Ok((stream, peer, connect_time, start.elapsed()))
}

/// A connection to the firmware's resolver for the relay of a host's lookups, which keeps it while the resolver does; its making is told in the log.
pub fn reach_for_relay(down: &NetworkDown) -> io::Result<Stream> {
    let (stream, peer, connect_time, tls_time) = reach(down, Instant::now())?;
    log!("{}", to_relay_line(&peer, connect_time.as_millis(), &stream.to_log_text(), tls_time.as_millis()));
    Ok(stream)
}

fn query(names: &[String], down: &NetworkDown) -> Answers {
    let (questions, mut batch) = Batch::new(names).map_err(batch_error)?;
    let start = Instant::now();
    let (mut stream, peer, connect_time, tls_time) = reach(down, start)?;
    // All questions go out in one piece, so that they travel together.
    stream.write_all(&questions)?;
    stream.flush()?;
    read_answers(&mut stream, &mut batch)?;
    let tls = stream.to_log_text();
    let total_ms = start.elapsed().as_millis();
    log!("{}", to_reached_line(&peer, connect_time.as_millis(), &tls, tls_time.as_millis(), names.len(), total_ms));
    stream.close();
    Ok(batch.results())
}

/// The addresses of each name in its place, in the order to try them: an address needs no query, a name is asked for once however it is written, an answer is cached as long as it allows, and the rest go over one connection; the end of the stay `down` ends the query, and a wait for another's.
pub fn resolve_all(names: &[&str], down: &NetworkDown) -> Vec<io::Result<Vec<IpAddr>>> {
    let results = RESOLVER.resolve(names, down, |names| query_with_watch(names, down));
    let to_error = |reason: Unresolved<Failure>| match reason {
        Unresolved::Invalid => batch_error(BatchError::InvalidName),
        Unresolved::NoAddress(reason @ NoAddress::Unanswered) => sys::invalid(reason.message()),
        Unresolved::NoAddress(reason) => io::Error::new(io::ErrorKind::NotFound, reason.message()),
        Unresolved::Failed((kind, text)) => io::Error::new(kind, text),
    };
    results.into_iter().map(|result| result.map_err(to_error)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::tests::up;
    use std::sync::mpsc;
    use std::thread;

    fn address(octet: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, octet])
    }

    #[test]
    fn literals_need_no_query() {
        let down = up();
        assert_eq!(resolve_all(&["192.0.2.9"], &down).swap_remove(0).unwrap(), [address(9)]);
        let results = resolve_all(&["192.0.2.9", "a..b", "2001:db8::1", "192.0.2.9"], &down);
        assert_eq!(
            results.iter().map(|result| result.as_ref().map(Vec::len).map_err(io::Error::kind)).collect::<Vec<_>>(),
            [Ok(1), Err(io::ErrorKind::InvalidInput), Ok(1), Ok(1)],
            "what is no name is not asked for, several at once are told each in its place, and where none needs asking nobody is asked"
        );
        assert!(resolve_all(&[], &down).is_empty());
    }

    #[test]
    fn answers_cached_across_queries() {
        let (resolver, down) = (Resolver::new(), up());
        let names = ["a.example", "b.example", "c.example"];
        let first_query = |query: &[String]| -> Answers {
            assert_eq!(query, ["a.example", "b.example", "c.example"], "all that is wanted in one asking");
            Ok(vec![Ok((vec![address(1)], 60)), Err(NoAddress::Absent), Ok((vec![address(3)], 0))])
        };
        assert_eq!(
            resolver.resolve(&names, &down, first_query),
            [Ok(vec![address(1)]), Err(Unresolved::NoAddress(NoAddress::Absent)), Ok(vec![address(3)])],
            "each name is told in its place, also the one that has no address"
        );
        let second_query = |query: &[String]| -> Answers {
            assert_eq!(query, ["b.example", "c.example"], "what an answer lets nobody keep is asked for anew");
            Ok(vec![Ok((vec![address(2)], 60)), Ok((vec![address(4)], 60))])
        };
        assert_eq!(
            resolver.resolve(&names, &down, second_query),
            [Ok(vec![address(1)]), Ok(vec![address(2)]), Ok(vec![address(4)])]
        );
        let no_query = |_: &[String]| -> Answers { panic!("asked for what is kept") };
        assert_eq!(
            resolver.resolve(&["A.Example.", "b.example"], &down, no_query),
            [Ok(vec![address(1)]), Ok(vec![address(2)])]
        );
        let empty_answer = |_: &[String]| -> Answers { Ok(Vec::new()) };
        assert_eq!(
            resolver.resolve(&["d.example"], &down, empty_answer),
            [Err(Unresolved::NoAddress(NoAddress::Unanswered))],
            "a name the resolver left out has no answer"
        );
    }

    #[test]
    fn one_query_at_a_time() {
        let (resolver, down) = (Arc::new(Resolver::new()), up());
        let (start_sender, start_receiver) = mpsc::channel::<()>();
        let (resume_sender, resume_receiver) = mpsc::channel::<()>();
        let first = thread::spawn({
            let (resolver, down) = (Arc::clone(&resolver), down.clone());
            move || {
                resolver.resolve(&["a.example"], &down, |_| {
                    start_sender.send(()).unwrap();
                    resume_receiver.recv().unwrap();
                    Ok(vec![Ok((vec![address(1)], 60))])
                })
            }
        });
        start_receiver.recv().unwrap();
        let second = thread::spawn({
            let (resolver, down) = (Arc::clone(&resolver), down.clone());
            move || resolver.resolve(&["a.example"], &down, |_| panic!("asked beside the asking that is under way"))
        });
        let ended = up();
        let gone = thread::spawn({
            let (resolver, ended) = (Arc::clone(&resolver), ended.clone());
            move || resolver.resolve(&["a.example"], &ended, |_| panic!("asked in a stay that ended"))
        });
        ended.raise();
        assert_eq!(
            gone.join().unwrap(),
            [Err(Unresolved::Failed((io::ErrorKind::NetworkDown, String::from("the network is gone"))))],
            "a waiter whose stay ended waits no more"
        );
        resume_sender.send(()).unwrap();
        assert_eq!(first.join().unwrap(), [Ok(vec![address(1)])]);
        assert_eq!(
            second.join().unwrap(),
            [Ok(vec![address(1)])],
            "the second asker for a name has it from the first, over no connection of its own"
        );
    }
}
