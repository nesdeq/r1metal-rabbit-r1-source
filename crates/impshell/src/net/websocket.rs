use crate::crypto;
use crate::net::tls::Stream;
use crate::net::web::{bad_header_error, reach, read_body, read_head, read_more, truncated_error};
use crate::sys::invalid;
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::http::{self, Target};
use r1_funcore::net::tls::Purpose;
use r1_funcore::net::websocket::{self, Action, KEY_BYTES, Kind, MESSAGE_BYTES, Message, NORMAL_CLOSURE, Receiver};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::RawFd;
use std::time::Duration;

struct OpenError {
    status: u16,
    body: Vec<u8>,
}

// A refusal's body may name part of the key: formatted, the error tells its size alone.
impl fmt::Debug for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenError").field("status", &self.status).field("body_bytes", &self.body.len()).finish()
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&websocket::to_unopened_text(self.status))
    }
}

impl std::error::Error for OpenError {}

/// The status and body of the answer that opened no WebSocket, where `error` is that.
pub fn open_error(error: &io::Error) -> Option<(u16, &[u8])> {
    let error = error.get_ref()?.downcast_ref::<OpenError>()?;
    Some((error.status, &error.body))
}

pub struct Socket {
    stream: Stream,
    receiver: Receiver,
    input: Vec<u8>,
    offset: usize,
    output: Vec<u8>,
    pub close_status: Option<u16>,
}

/// The outcome of a read: a message of the kind wanted, none yet, or the server's close.
#[derive(PartialEq, Debug)]
pub enum Incoming<T> {
    Message(T),
    Nothing,
    Closed,
}

impl Socket {
    /// Opens a WebSocket at the target with the TLS of `purpose` and those headers, under the connection's own limit of a wait; once it is open `timeout` bounds each wait on the server, and `down` ends any.
    pub fn reach(
        target: &Target,
        purpose: Purpose,
        headers: &[(&str, &str)],
        timeout: Duration,
        down: &NetworkDown,
    ) -> io::Result<Socket> {
        let mut socket = Socket::open(reach(target, purpose, down)?, target, headers)?;
        socket.set_timeout(timeout);
        Ok(socket)
    }

    fn open(mut stream: Stream, target: &Target, headers: &[(&str, &str)]) -> io::Result<Socket> {
        let random = crypto::random::<KEY_BYTES>().ok_or_else(|| io::Error::other("no random bytes for a key"))?;
        let key = websocket::client_key(&random);
        stream.write_all(&websocket::opening_request(target, &key, headers).ok_or_else(bad_header_error)?)?;
        stream.flush()?;
        let mut input = Vec::new();
        let head = read_head(&mut stream, &mut input)?;
        if !websocket::is_opened(&head, &websocket::accept_value(&key, crypto::sha1)) {
            let status = head.status;
            // What the server says in place of the WebSocket is read as far as it comes whole: it tells why.
            let mut body = Vec::new();
            if let Ok(answer) = read_body(&mut stream, head, std::mem::take(&mut input), http::GET) {
                let _ = answer.take(MESSAGE_BYTES as u64).read_to_end(&mut body);
            }
            return Err(io::Error::new(io::ErrorKind::InvalidData, OpenError { status, body }));
        }
        Ok(Socket {
            stream,
            receiver: Receiver::new(MESSAGE_BYTES),
            input,
            offset: 0,
            output: Vec::new(),
            close_status: None,
        })
    }

    /// The underlying socket, readable when the server may have sent something.
    pub fn fd(&self) -> RawFd {
        self.stream.fd()
    }

    /// Sets how long each wait on the server may take.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.stream.set_timeout(timeout);
    }

    /// Sends one message in one frame, reusing the send buffer.
    pub fn send(&mut self, kind: Kind, payload: &[u8]) -> io::Result<()> {
        let mask = crypto::random::<4>().ok_or_else(|| io::Error::other("no random bytes for a mask"))?;
        websocket::write_frame(kind, payload, mask, &mut self.output);
        self.stream.write_all(&self.output)?;
        self.stream.flush()
    }

    /// Takes back a read message's buffer for the next message.
    pub fn give_back(&mut self, buffer: Vec<u8>) {
        self.receiver.give_back(buffer);
    }

    /// The next message `select` takes, pings answered and other kinds skipped; with none whole, `should_wait` says whether to read on or return `Nothing`.
    pub fn next_message<T>(
        &mut self,
        should_wait: bool,
        select: impl Fn(Message) -> Option<T>,
    ) -> io::Result<Incoming<T>> {
        loop {
            match self.receiver.take_frame(&self.input[self.offset..]) {
                Ok(Some((length, message))) => {
                    self.offset += length;
                    match Action::from_message(message) {
                        Action::Pong(payload) => self.send(Kind::Pong, &payload)?,
                        Action::Nothing => {}
                        Action::Close { status, echo } => {
                            self.close_status = status;
                            let _ = self.send(Kind::Close, &echo);
                            return Ok(Incoming::Closed);
                        }
                        Action::Message(message) => {
                            if let Some(value) = select(message) {
                                return Ok(Incoming::Message(value));
                            }
                        }
                    }
                }
                Ok(None) => {
                    // What is left of a frame moves to the buffer's start once, before more is read behind it, however many messages came before it.
                    self.input.drain(..std::mem::take(&mut self.offset));
                    if !should_wait && !self.stream.is_ready()? {
                        return Ok(Incoming::Nothing);
                    }
                    if !read_more(&mut self.stream, &mut self.input)? {
                        return Err(truncated_error());
                    }
                }
                Err(violation) => {
                    let _ = self.send(Kind::Close, &websocket::close_payload(violation.status()));
                    return Err(invalid(violation.to_text()));
                }
            }
        }
    }

    /// Sends the close frame and ends the stream, without waiting for the server's.
    pub fn close(mut self) {
        let _ = self.send(Kind::Close, &websocket::close_payload(NORMAL_CLOSURE));
        self.stream.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Peer, head, peer, serve};
    use crate::wifi::radio::tests::up;

    #[test]
    fn read_without_wait_takes_whole_messages() {
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let (mut socket, server) = peer(move |peer| {
            peer.0.write_all(&[0x81, 0x05, b'H', b'e']).unwrap();
            receiver.recv().unwrap();
            peer.0.write_all(&[b'l', b'l', b'o', 0x82, 0x01, 9, 0x81, 0x02, b'H', b'i', 0x88, 0x00]).unwrap();
            let _ = peer.0.read_to_end(&mut Vec::new());
        });
        // Waits as a loop would that has other things to wait for beside the socket: after a look that found nothing.
        let wait_readable = |socket: &Socket| {
            let mut pollfd = libc::pollfd { fd: socket.fd(), events: libc::POLLIN, revents: 0 };
            assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 10_000) }, 1, "the socket tells when something has come");
        };
        assert_eq!(
            socket.next_message(false, Message::text).unwrap(),
            Incoming::Nothing,
            "half a message is none, and is not waited for"
        );
        assert_eq!(socket.next_message(false, Message::text).unwrap(), Incoming::Nothing);
        sender.send(()).unwrap();
        let mut messages = Vec::new();
        while messages.len() < 3 {
            match socket.next_message(false, Message::text).unwrap() {
                Incoming::Nothing => wait_readable(&socket),
                Incoming::Message(text) => {
                    messages.push(text.clone());
                    socket.give_back(text.into_bytes());
                }
                Incoming::Closed => messages.push("closed".into()),
            }
        }
        assert_eq!(
            messages,
            ["Hello", "Hi", "closed"],
            "then both texts that came in one piece, the binary message between them passed over, and the server's close"
        );
        socket.close();
        server.join().unwrap();
    }

    #[test]
    fn open_talk_close() {
        let (target, server) = serve("/", |connection| {
            let mut peer = Peer(connection);
            let request = peer.greet();
            // A ping, a text in two parts, and after the client's word a close.
            peer.0.write_all(&[0x89, 0x02, b'h', b'i', 0x01, 0x03, b'H', b'e', b'l', 0x80, 0x02, b'l', b'o']).unwrap();
            let mut frames = vec![peer.frame().unwrap(), peer.frame().unwrap()];
            peer.0.write_all(&[0x88, 0x02, 0x03, 0xE8]).unwrap();
            frames.push(peer.frame().unwrap());
            (request, frames)
        });
        let stream = reach(&target, Purpose::Http, &up()).unwrap();
        let mut socket = Socket::open(stream, &target, &[]).unwrap();
        assert_eq!(
            socket.next_message(true, Some).unwrap(),
            Incoming::Message(Message::Text("Hello".into())),
            "the ping before it is answered on the way"
        );
        socket.send(Kind::Text, b"Hello yourself").unwrap();
        assert_eq!(socket.next_message(true, Some).unwrap(), Incoming::Closed, "the server has closed");
        let (request, frames) = server.join().unwrap();
        assert!(
            request.contains("\r\nUpgrade: websocket\r\n") && request.contains("\r\nSec-WebSocket-Version: 13\r\n")
        );
        assert_eq!(
            frames,
            [(0xA, b"hi".to_vec()), (0x1, b"Hello yourself".to_vec()), (0x8, vec![0x03, 0xE8])],
            "the pong, the client's word, and the close of 1000 in answer to the server's"
        );
    }

    #[test]
    fn bad_accept_or_status_opens_nothing() {
        let (target, server) = serve("/", |mut connection| {
            head(&mut connection);
            connection.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n").unwrap();
        });
        let stream = reach(&target, Purpose::Http, &up()).unwrap();
        let error = Socket::open(stream, &target, &[]).map(|_| ()).unwrap_err();
        assert_eq!((error.kind(), open_error(&error)), (io::ErrorKind::InvalidData, Some((101, &[][..]))));
        server.join().unwrap();
        let (target, server) = serve("/", |mut connection| {
            head(&mut connection);
            connection.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 9\r\n\r\n{\"no\":1}\n").unwrap();
        });
        let stream = reach(&target, Purpose::Http, &up()).unwrap();
        let error = Socket::open(stream, &target, &[]).map(|_| ()).unwrap_err();
        assert_eq!(
            (open_error(&error), error.to_string().as_str()),
            (Some((401, &b"{\"no\":1}\n"[..])), "the server answers 401 and opens no WebSocket"),
            "a server that opens nothing says so by its status, and why in its body"
        );
        assert_eq!(
            error.get_ref().map(|inner| format!("{inner:?}")).as_deref(),
            Some("OpenError { status: 401, body_bytes: 9 }"),
            "formatted, it tells the body's size alone"
        );
        server.join().unwrap();
        assert_eq!(open_error(&io::Error::other("gone")), None, "any other error has no status");
    }
}
