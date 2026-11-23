use crate::crypto::Library;
use crate::log;
use crate::net::websocket::{Incoming, Socket};
use crate::sys::{self, Epoll};
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::http::Target;
use r1_funcore::net::tls::Purpose;
use r1_funcore::net::websocket::{Kind, Message};
use r1_funcore::signal::connection::{Action, Connection};
use r1_funcore::signal::device::Device;
use r1_funcore::signal::service::{self, Frame, TIMEOUT};
use std::io;
use std::os::fd::RawFd;

const ANONYMOUS: u64 = 4;

pub(super) struct Service {
    socket: Socket,
}

impl Service {
    pub(super) fn open(path: &str, headers: &[(&str, String)], down: &NetworkDown) -> io::Result<Service> {
        let target = Target::parse(&service::url(path)).ok_or_else(|| io::Error::other(service::NO_URL))?;
        let headers: Vec<(&str, &str)> = headers.iter().map(|(name, value)| (*name, value.as_str())).collect();
        Ok(Service { socket: Socket::reach(&target, Purpose::Signal, &headers, TIMEOUT, down)? })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.socket.fd()
    }

    /// Sends a frame the core made.
    pub fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.socket.send(Kind::Binary, frame)
    }

    pub(super) fn next_frame(&mut self, should_wait: bool) -> io::Result<Option<Frame>> {
        loop {
            let bytes = match self.socket.next_message(should_wait, to_binary)? {
                Incoming::Message(bytes) => bytes,
                Incoming::Nothing => return Ok(None),
                Incoming::Closed => {
                    let text = service::to_closed_text(self.socket.close_status);
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, text));
                }
            };
            let frame = Frame::read(&bytes);
            self.socket.give_back(bytes);
            match frame {
                Some(frame) => return Ok(Some(frame)),
                None => log!("{}", service::NOT_A_FRAME_LINE),
            }
        }
    }

    pub(super) fn close(self) {
        self.socket.close();
    }
}

fn to_binary(message: Message) -> Option<Vec<u8>> {
    service::to_binary(message).inspect_err(|line| log!("{line}")).ok()
}

#[derive(Default)]
pub(super) struct Anonymous {
    socket: Option<Service>,
}

impl Anonymous {
    pub(super) fn send(
        &mut self,
        frame: &[u8],
        epoll: &mut Epoll,
        (connection, device): (&mut Connection, &mut Device),
        down: &NetworkDown,
    ) {
        let result = (|| -> io::Result<()> {
            if self.socket.is_none() {
                let socket = Service::open(service::SOCKET_PATH, &service::socket_headers(None), down)?;
                epoll.add(socket.fd(), ANONYMOUS)?;
                self.socket = Some(socket);
            }
            self.socket.as_mut().map_or(Ok(()), |socket| socket.send(frame))
        })();
        if let Err(e) = result {
            log!("signal: the unauthenticated socket: {e}");
            self.close(epoll);
            connection.note_anonymous_lost(device);
        }
    }

    pub(super) fn read_answers(
        &mut self,
        epoll: &mut Epoll,
        connection: &mut Connection,
        device: &mut Device,
        actions: &mut Vec<Action>,
    ) {
        loop {
            let Some(socket) = self.socket.as_mut() else { return };
            match socket.next_frame(false) {
                Ok(Some(frame)) => {
                    let times = (sys::realtime_ms(), sys::boottime_ms());
                    match connection.take_anonymous_frame(device, &Library, frame, times) {
                        Ok(taken) => actions.extend(taken),
                        Err(line) => log!("{line}"),
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    log!("signal: the unauthenticated socket: {e}");
                    self.close(epoll);
                    connection.note_anonymous_lost(device);
                }
            }
        }
    }

    pub(super) fn close(&mut self, epoll: &mut Epoll) {
        if let Some(socket) = self.socket.take() {
            let _ = epoll.remove(socket.fd());
            socket.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::signal::curve::Pair;
    use r1_funcore::signal::link::{ProvisioningMessage, address, read_frame};

    #[test]
    #[ignore]
    fn live_provisioning_address() {
        let down = crate::wifi::radio::tests::up();
        let mut service = Service::open(service::PROVISIONING_PATH, &service::socket_headers(None), &down).unwrap();
        let key = Pair::new(&Library).unwrap();
        let frame = service.next_frame(true).unwrap().unwrap();
        let (ProvisioningMessage::Address(provisioning), Some(answer)) = read_frame(&Library, &key.private, &frame)
        else {
            panic!("an address first: {frame:?}")
        };
        service.send(&answer).unwrap();
        println!("{}", address(&provisioning, &key.public));
        service.close();
    }
}
