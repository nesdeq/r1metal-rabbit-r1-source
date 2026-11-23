use crate::log;
use crate::net::names;
use crate::net::tls::Stream;
use crate::sys::{Epoll, Wake};
use crate::wifi::radio::Hold;
use r1_funcore::cell::link::OWN_ADDRESS;
use r1_funcore::net::MTU;
use r1_funcore::net::dns::nat64::PORT;
use r1_funcore::net::dns::{self, Relay, Streams};
use r1_funcore::net::race::TIMEOUT;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;

const STOP: u64 = 0;
const DOWN: u64 = 1;
const DATAGRAMS: u64 = 2;
const LISTENER: u64 = 3;
const RESOLVER: u64 = 4;
// The token of a host's connection is this and its number.
const CONNECTIONS: u64 = 5;

// Who asked: by a datagram from that address, or over the connection of that number.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asker {
    Datagram(SocketAddr),
    Connection(u64),
}

struct Worker<'a> {
    hold: &'a Hold,
    epoll: Epoll,
    datagrams: UdpSocket,
    listener: TcpListener,
    connections: HashMap<u64, TcpStream>,
    streams: Streams,
    resolver: Option<Stream>,
    relay: Relay<Asker>,
}

fn read_message(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 2];
    stream.read_exact(&mut length)?;
    let mut message = vec![0u8; dns::message_length(length)];
    stream.read_exact(&mut message)?;
    Ok(message)
}

impl Worker<'_> {
    // Lets go of the resolver's connection, where one stands, and of the questions that wait for its answers.
    fn drop_resolver(&mut self) {
        let line = self.relay.end();
        if let Some(resolver) = self.resolver.take() {
            if let Err(e) = self.epoll.remove(resolver.fd()) {
                log!("names: ending the watch of the relay's connection: {e}");
            }
            log!("{line}");
        }
    }

    // Sends a question to the resolver, over a connection made for it where none stands.
    fn send(&mut self, framed: &[u8]) -> io::Result<()> {
        if self.resolver.is_none() {
            let stream = names::reach_for_relay(self.hold.down())?;
            self.epoll.add(stream.fd(), RESOLVER)?;
            self.resolver = Some(stream);
        }
        let resolver = self.resolver.as_mut().ok_or_else(|| io::Error::other("no connection"))?;
        resolver.write_all(framed)?;
        resolver.flush()
    }

    fn ask(&mut self, asker: Asker, is_stream: bool, question: &[u8]) {
        if let Some(framed) = self.relay.ask(asker, is_stream, question)
            && let Err(e) = self.send(&framed)
        {
            log!("names: the relay could not ask the resolver: {e}");
            self.drop_resolver();
        }
    }

    fn take_datagrams(&mut self) {
        let mut question = [0u8; MTU];
        loop {
            match self.datagrams.recv_from(&mut question) {
                Ok((count, sender)) => self.ask(Asker::Datagram(sender), false, &question[..count]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => return log!("names: the relay could not read a question: {e}"),
            }
        }
    }

    // Takes a host's connection under the number funcore gives it, and lets go of the one funcore has go for it.
    fn take_connection(&mut self, connection: TcpStream) {
        let (number, oldest) = self.streams.open();
        if let Some(oldest) = oldest {
            self.drop_connection(oldest);
        }
        let watched = connection
            .set_read_timeout(Some(TIMEOUT))
            .and_then(|()| connection.set_write_timeout(Some(TIMEOUT)))
            .and_then(|()| self.epoll.add(connection.as_raw_fd(), CONNECTIONS + number));
        match watched {
            Ok(()) => {
                self.connections.insert(number, connection);
            }
            Err(e) => {
                log!("names: the relay could not take a connection: {e}");
                self.streams.close(number);
            }
        }
    }

    fn accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((connection, _)) => self.take_connection(connection),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => return log!("names: the relay could not take a connection: {e}"),
            }
        }
    }

    fn drop_connection(&mut self, number: u64) {
        if let Some(connection) = self.connections.remove(&number)
            && let Err(e) = self.epoll.remove(connection.as_raw_fd())
        {
            log!("names: ending the watch of a connection to the relay: {e}");
        }
        self.streams.close(number);
        self.relay.forget(|asker| *asker == Asker::Connection(number));
    }

    fn take_question(&mut self, number: u64) {
        let question = self.connections.get_mut(&number).map(read_message);
        match question {
            Some(Ok(question)) => self.ask(Asker::Connection(number), true, &question),
            // A host that closed its connection, or said nothing whole in time.
            Some(Err(_)) => self.drop_connection(number),
            None => {}
        }
    }

    fn answer(&mut self, asker: Asker, answer: &[u8]) {
        match asker {
            Asker::Datagram(address) => {
                if let Err(e) = self.datagrams.send_to(answer, address) {
                    log!("names: the relay could not answer: {e}");
                }
            }
            Asker::Connection(number) => {
                let written = self.connections.get_mut(&number).map(|connection| connection.write_all(answer));
                if let Some(Err(_)) = written {
                    self.drop_connection(number);
                }
            }
        }
    }

    // Takes the answers the resolver wrote; an error where its connection ended.
    fn take_answers(&mut self) -> io::Result<()> {
        loop {
            let Some(resolver) = &mut self.resolver else {
                return Ok(());
            };
            if !resolver.is_ready()? {
                return Ok(());
            }
            let message = read_message(resolver)?;
            if let Some((asker, answer)) = self.relay.answer(&message) {
                self.answer(asker, &answer);
            }
        }
    }

    fn serve(&mut self) -> io::Result<()> {
        loop {
            for token in self.epoll.wait(None)? {
                match token {
                    STOP | DOWN => return Ok(()),
                    DATAGRAMS => self.take_datagrams(),
                    LISTENER => self.accept(),
                    RESOLVER => {
                        if self.take_answers().is_err() {
                            self.drop_resolver();
                        }
                    }
                    connection => self.take_question(connection - CONNECTIONS),
                }
            }
        }
    }
}

// Relays the host's lookups, which come to the device's own address in a routed stay, over one connection to the firmware's resolver, until `stop` is raised or the stay ends.
pub(super) fn run(hold: &Hold, stop: &Wake) -> io::Result<()> {
    let address = SocketAddr::from((OWN_ADDRESS, PORT));
    let (datagrams, listener) = (UdpSocket::bind(address)?, TcpListener::bind(address)?);
    datagrams.set_nonblocking(true)?;
    listener.set_nonblocking(true)?;
    let mut epoll = Epoll::new()?;
    epoll.add(stop.fd(), STOP)?;
    epoll.add(hold.down().fd(), DOWN)?;
    epoll.add(datagrams.as_raw_fd(), DATAGRAMS)?;
    epoll.add(listener.as_raw_fd(), LISTENER)?;
    let mut worker = Worker {
        hold,
        epoll,
        datagrams,
        listener,
        connections: HashMap::new(),
        streams: Streams::default(),
        resolver: None,
        relay: Relay::default(),
    };
    let served = worker.serve();
    worker.drop_resolver();
    served
}
