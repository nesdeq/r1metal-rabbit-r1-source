use crate::net::tls;
use crate::sys;
use r1_funcore::net::MTU;
use r1_funcore::net::tls::Purpose;
use rustls::ClientConnection;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, RawFd};

// A stream of the call's agent toward a relay or the calling server: TCP, under TLS where the server is named. None of its calls waits.
pub(super) struct Stream {
    tcp: TcpStream,
    session: Option<Box<ClientConnection>>,
    is_connected: bool,
    // What was written and the socket has not taken yet, where no TLS session keeps it.
    unsent: Vec<u8>,
}

impl Stream {
    // Begins a connection to an address, to be greeted as `name` under TLS where one is given.
    pub(super) fn begin(far: &SocketAddr, name: Option<&str>) -> io::Result<Stream> {
        let session = name.map(|name| tls::session(name, Purpose::Relay)).transpose()?;
        let tcp = TcpStream::from(sys::begin_connect(far)?);
        // A call's packets wait for no more to fill a segment.
        tcp.set_nodelay(true)?;
        Ok(Stream { tcp, session: session.map(Box::new), is_connected: false, unsent: Vec::new() })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.tcp.as_raw_fd()
    }

    // How the connection begun has ended, told once: the address it is connected from, and none every time after.
    pub(super) fn take_connected(&mut self) -> io::Result<Option<SocketAddr>> {
        if self.is_connected {
            return Ok(None);
        }
        sys::connect_result(self.fd())?;
        self.tcp.peer_addr()?;
        self.is_connected = true;
        self.tcp.local_addr().map(Some)
    }

    // Sends bytes whole or not at all: while what was written before still waits for the socket they are dropped, as a link drops a datagram. True while some wait for the socket to be writable.
    pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<bool> {
        if self.flush()? {
            return Ok(true);
        }
        match &mut self.session {
            Some(session) => session.writer().write_all(bytes)?,
            None => self.unsent.extend_from_slice(bytes),
        }
        self.flush()
    }

    // Sends what waits as far as the socket takes it now; true while more waits.
    pub(super) fn flush(&mut self) -> io::Result<bool> {
        loop {
            let written = match &mut self.session {
                Some(session) if session.wants_write() => session.write_tls(&mut self.tcp),
                None if !self.unsent.is_empty() => {
                    let written = self.tcp.write(&self.unsent);
                    self.unsent.drain(..*written.as_ref().unwrap_or(&0));
                    written
                }
                _ => return Ok(false),
            };
            match written {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(e) => return Err(e),
            }
        }
    }

    // Reads what has come onto `out`; false once the far side has ended the stream.
    pub(super) fn read(&mut self, out: &mut Vec<u8>) -> io::Result<bool> {
        let mut buffer = [0u8; MTU];
        loop {
            let read = match &mut self.session {
                Some(session) => session.read_tls(&mut self.tcp),
                None => self.tcp.read(&mut buffer),
            };
            match (read, &mut self.session) {
                (Ok(0), _) => return Ok(false),
                (Ok(count), None) => out.extend_from_slice(&buffer[..count]),
                (Ok(_), Some(session)) => {
                    session.process_new_packets().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    match session.reader().read_to_end(out) {
                        Ok(_) => return Ok(false),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e),
                    }
                }
                (Err(e), _) if e.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                (Err(e), _) => return Err(e),
            }
        }
    }

    // Tells a server under TLS that the stream ends, as far as the socket takes it now.
    pub(super) fn close(mut self) {
        if let Some(session) = &mut self.session {
            session.send_close_notify();
            let _ = session.write_tls(&mut self.tcp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Connection, listen};
    use crate::sys::Epoll;
    use std::net::{Ipv4Addr, TcpListener};
    use std::time::Duration;

    const TOKEN: u64 = 7;
    const PATIENCE: Option<Duration> = Some(Duration::from_secs(10));

    // Reads until `count` bytes have come, waiting on the stream's socket as the calls' thread does, and sends what the session owes meanwhile.
    fn read_bytes(stream: &mut Stream, epoll: &mut Epoll, count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        while bytes.len() < count {
            assert_eq!(epoll.wait(PATIENCE).unwrap(), [TOKEN], "the socket wakes its watcher");
            assert!(stream.take_connected().is_ok(), "connected");
            assert!(stream.read(&mut bytes).unwrap(), "the far side keeps the stream");
            let is_waiting = stream.flush().unwrap();
            epoll.set_writable(stream.fd(), TOKEN, is_waiting).unwrap();
        }
        bytes
    }

    #[test]
    fn plain_stream() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut stream = Stream::begin(&listener.local_addr().unwrap(), None).unwrap();
        let mut epoll = Epoll::with_fd(stream.fd());
        epoll.set_writable(stream.fd(), TOKEN, true).unwrap();
        let (mut far, from) = listener.accept().unwrap();
        assert_eq!(epoll.wait(PATIENCE).unwrap(), [TOKEN], "writable once the connection has come");
        assert_eq!(stream.take_connected().unwrap(), Some(from), "connected from the address the far side sees");
        assert_eq!(stream.take_connected().unwrap(), None, "told once");
        assert!(!stream.write(b"asked").unwrap(), "a socket with room takes it at once");
        epoll.set_writable(stream.fd(), TOKEN, false).unwrap();
        let mut asked = [0u8; 5];
        far.read_exact(&mut asked).unwrap();
        far.write_all(b"answered").unwrap();
        assert_eq!((&asked, read_bytes(&mut stream, &mut epoll, 8).as_slice()), (b"asked", &b"answered"[..]));
        drop(far);
        assert_eq!(epoll.wait(PATIENCE).unwrap(), [TOKEN]);
        assert!(!stream.read(&mut Vec::new()).unwrap(), "the far side's end is told");
        let refused = listener.local_addr().unwrap();
        drop(listener);
        let mut stream = Stream::begin(&refused, None).unwrap();
        let mut epoll = Epoll::with_fd(stream.fd());
        epoll.set_writable(stream.fd(), TOKEN, true).unwrap();
        assert_eq!(epoll.wait(PATIENCE).unwrap(), [TOKEN]);
        assert!(stream.take_connected().is_err(), "a port nothing listens on refuses");
    }

    #[test]
    fn stream_under_tls() {
        let (target, listener) = listen("/");
        let server = std::thread::spawn(move || {
            let mut connection = Connection::accept(&listener);
            let mut asked = [0u8; 5];
            connection.read_exact(&mut asked).unwrap();
            connection.write_all(b"answered").unwrap();
            let end = connection.read(&mut [0u8]).unwrap();
            (asked, end)
        });
        let far = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
        let mut stream = Stream::begin(&far, Some(&target.host)).unwrap();
        let mut epoll = Epoll::with_fd(stream.fd());
        epoll.set_writable(stream.fd(), TOKEN, true).unwrap();
        assert_eq!(epoll.wait(PATIENCE).unwrap(), [TOKEN]);
        assert!(stream.take_connected().unwrap().is_some());
        assert!(!stream.write(b"asked").unwrap(), "written before the greeting is over, it goes once that is");
        epoll.set_writable(stream.fd(), TOKEN, false).unwrap();
        assert_eq!(read_bytes(&mut stream, &mut epoll, 8), b"answered", "opened under the session's keys");
        stream.close();
        assert_eq!(server.join().unwrap(), (*b"asked", 0), "what was asked came, and then the stream's end, told");
        assert!(Stream::begin(&far, Some("not a name!")).is_err(), "what is no name of a server is refused");
    }
}
