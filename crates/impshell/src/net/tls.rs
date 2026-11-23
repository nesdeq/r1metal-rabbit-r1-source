use crate::net::dial::{self, Wire};
use crate::wifi::radio::NetworkDown;
use r1_funcore::net::tls::{Purpose, Roots, Versions, to_text};
use r1_funcore::signal::service;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, HandshakeKind, RootCertStore, StreamOwned};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

fn public_roots() -> RootCertStore {
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    #[cfg(test)]
    let roots = crate::net::served::add_test_root(roots);
    roots
}

fn signal_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    for der in service::ROOTS {
        roots.add(rustls::pki_types::CertificateDer::from(der)).expect("Signal's roots are certificates");
    }
    roots
}

fn build_config(purpose: Purpose) -> Arc<ClientConfig> {
    let versions: &[&'static rustls::SupportedProtocolVersion] = match purpose.versions() {
        Versions::Both => rustls::DEFAULT_VERSIONS,
        Versions::Tls13 => &[&rustls::version::TLS13],
    };
    let roots = match purpose.roots() {
        Roots::Public => public_roots(),
        Roots::Signal => signal_roots(),
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("the library's own versions and suites agree")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = purpose.protocols().iter().map(|protocol| protocol.to_vec()).collect();
    Arc::new(config)
}

// One configuration a purpose for the process, so that a server met again resumes.
fn config(purpose: Purpose) -> Arc<ClientConfig> {
    static CONFIGS: [OnceLock<Arc<ClientConfig>>; Purpose::ALL.len()] = [const { OnceLock::new() }; Purpose::ALL.len()];
    Arc::clone(CONFIGS[purpose as usize].get_or_init(|| build_config(purpose)))
}

/// A client's side of TLS toward the server of `name` for `purpose`, before its first byte: what is no name of a server is refused here.
pub fn session(name: &str, purpose: Purpose) -> io::Result<ClientConnection> {
    let server = ServerName::try_from(name.to_owned()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    ClientConnection::new(config(service::to_purpose(name, purpose)), server).map_err(io::Error::other)
}

pub struct Stream(Box<StreamOwned<ClientConnection, Wire>>);

impl Stream {
    // Connects to the first of `addresses` that answers and greets it as `name` for `purpose`: the stream, the address reached and how long the connection took from `start`.
    pub(super) fn reach(
        addresses: &[SocketAddr],
        name: &str,
        purpose: Purpose,
        start: Instant,
        down: &NetworkDown,
    ) -> io::Result<(Stream, String, Duration)> {
        let wire = dial::connect(addresses, down)?;
        let (peer, connect_time) = (wire.peer(), start.elapsed());
        Ok((Stream::handshake(wire, name, purpose)?, peer, connect_time))
    }

    fn handshake(mut wire: Wire, name: &str, purpose: Purpose) -> io::Result<Stream> {
        let mut session = session(name, purpose)?;
        while session.is_handshaking() {
            session.complete_io(&mut wire)?;
        }
        Ok(Stream(Box::new(StreamOwned::new(session, wire))))
    }

    /// Sets how long each wait on the peer may take.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.0.sock.timeout = timeout;
    }

    pub(super) fn network_down(&self) -> &NetworkDown {
        &self.0.sock.down
    }

    /// Readable when the server may have written: `is_ready` then tells whether a read would wait.
    pub fn fd(&self) -> RawFd {
        self.0.sock.fd()
    }

    /// True where a read would not wait: what the server wrote, or its close, has come. Records that carry neither, as a TLS 1.3 session ticket a server sends after the greeting, are taken in on the way.
    pub fn is_ready(&mut self) -> io::Result<bool> {
        let StreamOwned { conn, sock } = self.0.as_mut();
        while conn.wants_read() && sock.is_readable()? {
            if conn.read_tls(sock)? == 0 {
                return Ok(true);
            }
            conn.process_new_packets().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
        Ok(!conn.wants_read())
    }

    pub(super) fn to_log_text(&self) -> String {
        let version = self.0.conn.protocol_version().map(|version| format!("{version:?}"));
        to_text(version, self.0.conn.handshake_kind() == Some(HandshakeKind::Resumed))
    }

    /// Sends close_notify and ends the connection.
    pub fn close(mut self) {
        self.0.conn.send_close_notify();
        let _ = self.0.conn.complete_io(&mut self.0.sock);
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::dial::wait;
    use crate::net::served::serve;
    use crate::wifi::radio::tests::up;
    use r1_funcore::codec::http;
    use r1_funcore::net::race::TIMEOUT;
    use std::net::{Ipv4Addr, TcpListener};

    // Waits as a loop that waits on more than the stream would: until its socket is readable.
    fn await_readable(stream: &Stream) {
        let mut socket = vec![libc::pollfd { fd: stream.fd(), events: libc::POLLIN, revents: 0 }];
        assert!(
            wait(&mut socket, &up(), Some(Instant::now() + TIMEOUT)).unwrap(),
            "whoever waits on its socket is woken"
        );
    }

    #[test]
    fn pending_read_ready() {
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let (target, server) = serve("/", move |mut connection| {
            receiver.recv().unwrap();
            connection.write_all(b"more").unwrap();
            receiver.recv().unwrap();
        });
        let down = up();
        let wire = dial::connect(&[SocketAddr::from((Ipv4Addr::LOCALHOST, target.port))], &down).unwrap();
        let mut stream = Stream::handshake(wire, &target.host, Purpose::Http).unwrap();
        await_readable(&stream);
        assert!(
            !stream.is_ready().unwrap(),
            "the server's session tickets came after the greeting, and nothing more: a read would wait"
        );
        sender.send(()).unwrap();
        await_readable(&stream);
        let mut buffer = [0u8; 8];
        assert!(stream.is_ready().unwrap() && stream.read(&mut buffer).unwrap() == 4 && !stream.is_ready().unwrap());
        assert!(stream.to_log_text().starts_with("TLSv1_3, "), "{}", stream.to_log_text());
        down.raise();
        assert_eq!(
            stream.read(&mut buffer).map_err(|e| e.kind()),
            Err(io::ErrorKind::NetworkDown),
            "without the network no wait is begun"
        );
        sender.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn one_config_per_purpose() {
        for purpose in Purpose::ALL {
            assert!(Arc::ptr_eq(&config(purpose), &config(purpose)), "{purpose:?}");
        }
        assert_eq!(
            (
                config(Purpose::Names).alpn_protocols.len(),
                config(Purpose::Http).alpn_protocols.clone(),
                config(Purpose::Signal).alpn_protocols.clone()
            ),
            (0, vec![http::ALPN.to_vec()], vec![http::ALPN.to_vec()])
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let wire = dial::connect(&[listener.local_addr().unwrap()], &up()).unwrap();
        assert!(
            Stream::handshake(wire, "not a name!", Purpose::Http).is_err(),
            "what is no name of a server is refused before a byte is sent"
        );
    }
}
