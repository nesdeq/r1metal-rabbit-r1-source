use crate::net::websocket::Socket;
use crate::wifi::radio::tests::up;
use crate::{crypto, sys::lock};
use r1_funcore::codec::http::Target;
use r1_funcore::net::race::TIMEOUT;
use r1_funcore::net::tls::Purpose;
use r1_funcore::net::websocket;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

pub(crate) type Request = (usize, String, Vec<u8>);
const CLOSE_HEADER: &[u8] = b"\r\nConnection: close\r\n";
const ROOT: &[u8] = include_bytes!("served/root.der");
const CERTIFICATE: &[u8] = include_bytes!("served/server.der");
const KEY: &[u8] = include_bytes!("served/server.key.der");

pub(super) fn add_test_root(mut roots: RootCertStore) -> RootCertStore {
    roots.add(CertificateDer::from(ROOT)).expect("the tests' root is a certificate");
    roots
}

fn config() -> Arc<ServerConfig> {
    static CONFIG: OnceLock<Arc<ServerConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY));
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(rustls::DEFAULT_VERSIONS)
            .expect("the library's own versions and suites agree")
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERTIFICATE)], key)
            .expect("the servers' certificate and key belong together");
        Arc::new(config)
    }))
}

pub(crate) struct Connection(StreamOwned<ServerConnection, TcpStream>);

impl Connection {
    fn handshake(tcp: TcpStream) -> Connection {
        let mut session = StreamOwned::new(ServerConnection::new(config()).unwrap(), tcp);
        while session.conn.is_handshaking() {
            session.conn.complete_io(&mut session.sock).unwrap();
        }
        Connection(session)
    }

    pub(crate) fn accept(listener: &TcpListener) -> Connection {
        Connection::handshake(listener.accept().unwrap().0)
    }
}

impl Read for Connection {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Connection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.conn.send_close_notify();
        while self.0.conn.wants_write() && self.0.conn.write_tls(&mut self.0.sock).is_ok() {}
    }
}

pub(crate) fn listen(path: &str) -> (Target, TcpListener) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let target = Target::parse(&format!("https://{}{path}", listener.local_addr().unwrap())).unwrap();
    (target, listener)
}

pub(super) fn serve<T: Send + 'static>(
    path: &str,
    conversation: impl FnOnce(Connection) -> T + Send + 'static,
) -> (Target, JoinHandle<T>) {
    let (target, listener) = listen(path);
    (target, thread::spawn(move || conversation(Connection::accept(&listener))))
}

pub(crate) fn serve_answers(path: &str, answers: Vec<Vec<u8>>) -> (Target, Receiver<Vec<Request>>) {
    let (target, listener) = listen(path);
    let (sender, receiver) = mpsc::channel();
    let shared = Arc::new(Mutex::new((answers.into_iter(), Vec::new())));
    thread::spawn(move || {
        for (number, tcp) in listener.incoming().enumerate() {
            let (tcp, shared, sender) = (tcp.unwrap(), Arc::clone(&shared), sender.clone());
            thread::spawn(move || {
                let mut connection = Connection::handshake(tcp);
                while let Some((request, body)) = head(&mut connection) {
                    let mut guard = lock(&shared);
                    let answer = guard.0.next().unwrap();
                    guard.1.push((number, request, body));
                    let requests = (guard.0.len() == 0).then(|| guard.1.clone());
                    drop(guard);
                    let _ = answer.chunks(7).try_for_each(|piece| connection.write_all(piece));
                    requests.into_iter().for_each(|requests| sender.send(requests).unwrap());
                    // An answer that says it closes the connection does.
                    if answer.windows(CLOSE_HEADER.len()).any(|window| window == CLOSE_HEADER) {
                        break;
                    }
                }
            });
        }
    });
    (target, receiver)
}

pub(crate) fn head(connection: &mut Connection) -> Option<(String, Vec<u8>)> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8];
        if connection.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .map_or(0, |length| length.trim().parse().unwrap());
    let mut body = vec![0u8; length];
    connection.read_exact(&mut body).unwrap();
    Some((head, body))
}

pub(crate) struct Peer(pub(crate) Connection);

impl Peer {
    pub(super) fn greet(&mut self) -> String {
        let (request, _) = head(&mut self.0).unwrap();
        let key = request.lines().find_map(|line| line.strip_prefix("Sec-WebSocket-Key: ")).unwrap();
        let accept = websocket::accept_value(key, crypto::sha1);
        self.0.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).unwrap();
        request
    }

    pub(super) fn frame(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut head = [0u8; 4];
        self.0.read_exact(&mut head[..2]).ok()?;
        let length = match head[1] & 0x7F {
            126 => {
                self.0.read_exact(&mut head[2..]).unwrap();
                usize::from(u16::from_be_bytes([head[2], head[3]]))
            }
            length => usize::from(length),
        };
        let mut mask = [0u8; 4];
        self.0.read_exact(&mut mask).unwrap();
        let mut payload = vec![0u8; length];
        self.0.read_exact(&mut payload).unwrap();
        Some((head[0] & 0x0F, payload.iter().zip(mask.iter().cycle()).map(|(byte, mask)| byte ^ mask).collect()))
    }

    pub(crate) fn read_text(&mut self) -> Option<String> {
        let (opcode, payload) = self.frame()?;
        (opcode == 1).then(|| String::from_utf8(payload).unwrap())
    }

    pub(crate) fn write_text(&mut self, text: &str) {
        let length = text.len();
        let mut frame =
            if length < 126 { vec![0x81, length as u8] } else { vec![0x81, 126, (length >> 8) as u8, length as u8] };
        frame.extend_from_slice(text.as_bytes());
        self.0.write_all(&frame).unwrap();
    }
}

pub(crate) fn peer<T: Send + 'static>(
    service: impl FnOnce(&mut Peer) -> T + Send + 'static,
) -> (Socket, JoinHandle<T>) {
    let (target, server) = serve("/", |connection| {
        let mut peer = Peer(connection);
        peer.greet();
        service(&mut peer)
    });
    (Socket::reach(&target, Purpose::Http, &[], TIMEOUT, &up()).unwrap(), server)
}
