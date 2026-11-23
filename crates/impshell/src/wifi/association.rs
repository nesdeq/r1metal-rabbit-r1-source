use crate::crypto::Library;
use crate::crypto::curve::P256;
use crate::log;
use crate::sys::{self, Epoll};
use crate::wifi::nl80211::Socket;
use crate::wifi::{Preempt, iface};
use r1_funcore::wifi::credential::{Credential, NO_CURVE};
use r1_funcore::wifi::join::{
    Action, Attempt, JoinError, Next, to_lost_line, to_rsn_line, to_skipped_line, to_unread_line, to_untaken_line,
};
use r1_funcore::wifi::netlink::{self, Quality};
use r1_funcore::wifi::scan::Network;
use r1_funcore::wifi::wpa::Step;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

const HANDSHAKE: u16 = 0x888E;
const FRAME_BYTES: usize = 2304;
const EVENTS: u64 = 0;
const FRAMES: u64 = 1;
const NUDGE: u64 = 2;

pub(super) struct Association {
    events: Socket,
    commands: Socket,
    family: u16,
    frames: OwnedFd,
    pub(super) interface: u32,
    name: Vec<u8>,
    attempt: Attempt,
    curve: Option<P256>,
    // The log line of a loss read and not taken yet: a scan beside the stay reads the link too.
    loss: Option<String>,
}

impl Association {
    pub(super) fn join(
        name: &[u8],
        network: &Network,
        credential: Option<Credential>,
        address: Option<[u8; 6]>,
        preempt: &Preempt,
        limit: Duration,
    ) -> io::Result<Result<Association, JoinError>> {
        let start = Instant::now();
        let interface = sys::iface::index(iface::NAME, true)?;
        if let Some(address) = address {
            sys::iface::set_hardware_address(iface::NAME, address)?;
        }
        let station = sys::iface::hardware_address(iface::NAME)?;
        let attempt = match Attempt::new(&network.rsn, credential, station, network.address) {
            Ok(attempt) => attempt,
            Err(error) => return Ok(Err(error)),
        };
        let curve =
            if attempt.uses_sae() { Some(P256::new().ok_or_else(|| io::Error::other(NO_CURVE))?) } else { None };
        let frames = iface::packet_socket(interface, HANDSHAKE)?;
        let (mut events, commands) = (Socket::open()?, Socket::open()?);
        let family = events.wireless_family()?;
        events.listen(family.links)?;
        for registration in attempt.frame_registrations(interface) {
            events.request_ack(family.id, netlink::REGISTER_FRAME, &registration)?;
        }
        // A wish that came while the join was set up has it give way before the network hears of it, which would only be told to forget it.
        if preempt.is_pending() {
            return Ok(Err(JoinError::GaveWay));
        }
        // The request goes out over the socket that hears the news and owns the link from then on; the kernel's answer to it comes among the news.
        events.send(
            family.id,
            0,
            netlink::CONNECT,
            0,
            &attempt.request(name, network.frequency).to_attributes(interface),
        )?;
        let mut association = Association {
            events,
            commands,
            family: family.id,
            frames,
            interface,
            name: name.to_vec(),
            attempt,
            curve,
            loss: None,
        };
        match association.settle(start, preempt, limit) {
            Ok(Ok(())) => Ok(Ok(association)),
            Ok(Err(error)) => {
                if error.should_disconnect() {
                    association.leave();
                }
                Ok(Err(error))
            }
            Err(e) => {
                association.leave();
                Err(e)
            }
        }
    }

    fn settle(&mut self, start: Instant, preempt: &Preempt, limit: Duration) -> io::Result<Result<(), JoinError>> {
        let mut epoll = Epoll::new()?;
        epoll.add(self.events.fd(), EVENTS)?;
        epoll.add(preempt.nudge.fd(), NUDGE)?;
        loop {
            let wait = match self.attempt.next(start.elapsed(), limit) {
                Next::Wait(wait) => wait,
                Next::Done(line) => {
                    log!("{line}");
                    self.curve = None;
                    return Ok(Ok(()));
                }
                Next::Failed(error) => return Ok(Err(error)),
            };
            let ready = epoll.wait(Some(wait))?;
            if ready.contains(&NUDGE) && preempt.should_yield() {
                return Ok(Err(JoinError::GaveWay));
            }
            if ready.contains(&EVENTS) {
                match self.read_events()? {
                    Some(LinkEvent::Joined) => {
                        self.attempt.note_join(start.elapsed());
                        // The handshake's frames are taken from here on: the station's answer names what its association request had, which is known only now.
                        epoll.add(self.frames.as_raw_fd(), FRAMES)?;
                    }
                    Some(LinkEvent::Lost(error)) => return Ok(Err(error)),
                    None => {}
                }
                self.attempt.note_proof(start.elapsed());
            }
            if ready.contains(&FRAMES) {
                self.read_frames()?;
            }
        }
    }

    pub(super) fn fds(&self) -> (RawFd, RawFd) {
        (self.events.fd(), self.frames.as_raw_fd())
    }

    pub(super) fn station(&self) -> [u8; 6] {
        self.attempt.station
    }

    fn read_events(&mut self) -> io::Result<Option<LinkEvent>> {
        let datagram = self.events.datagram()?;
        let mut event = None;
        for message in netlink::messages(&datagram) {
            if let Some(code) = message.error() {
                return Err(io::Error::from_raw_os_error(code));
            }
            for action in self.attempt.handle(&Library, self.curve.as_ref(), &message) {
                match action {
                    Action::Send(frame) => {
                        let attributes = netlink::frame_attributes(self.interface, &frame);
                        self.events.send(self.family, 0, netlink::FRAME, 0, &attributes)?;
                    }
                    Action::Report(is_proved) => self.events.send(
                        self.family,
                        0,
                        netlink::EXTERNAL_AUTH,
                        0,
                        &netlink::external_auth_attributes(
                            self.interface,
                            &self.name,
                            &self.attempt.access_point,
                            is_proved,
                        ),
                    )?,
                    Action::Joined(rsn) => {
                        if let Some(rsn) = rsn {
                            log!("{}", to_rsn_line(&rsn));
                        }
                        event = Some(LinkEvent::Joined);
                    }
                    Action::Skipped(error) => log!("{}", to_skipped_line(error)),
                    Action::Failed(error) => return Ok(Some(LinkEvent::Lost(error))),
                }
            }
        }
        Ok(event)
    }

    fn read_loss(&mut self) -> io::Result<Option<JoinError>> {
        Ok(match self.read_events()? {
            Some(LinkEvent::Lost(error)) => Some(error),
            _ => None,
        })
    }

    // Reads the news of a joined link where its frames or its events are ready, while a step of its join waits: the loss they tell of, if any.
    pub(super) fn read_news(&mut self, is_frames: bool, is_events: bool) -> io::Result<Option<JoinError>> {
        if is_frames {
            self.read_frames()?;
        }
        if is_events { self.read_loss() } else { Ok(None) }
    }

    // Reads the news of a stay's link where its frames or its events are ready, keeping the log line of a loss it tells of for `take_loss`.
    pub(super) fn serve(&mut self, is_frames: bool, is_events: bool) {
        let frames = if is_frames { self.read_frames() } else { Ok(()) };
        let events = if is_events { self.read_loss() } else { Ok(None) };
        let loss = match (frames, events) {
            (Err(e), _) | (_, Err(e)) => Some(to_unread_line(&e.to_string())),
            (Ok(()), Ok(Some(error))) => Some(to_lost_line(error)),
            (Ok(()), Ok(None)) => None,
        };
        self.loss = self.loss.take().or(loss);
    }

    pub(super) const fn take_loss(&mut self) -> Option<String> {
        self.loss.take()
    }

    fn read_frames(&mut self) -> io::Result<()> {
        let mut frame = [0u8; FRAME_BYTES];
        loop {
            let mut source: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
            let mut size = size_of::<libc::sockaddr_ll>() as libc::socklen_t;
            let result = unsafe {
                libc::recvfrom(
                    self.frames.as_raw_fd(),
                    frame.as_mut_ptr().cast(),
                    frame.len(),
                    0,
                    (&raw mut source).cast(),
                    &mut size,
                )
            };
            let length = match sys::check_count(result) {
                Ok(length) => length,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            };
            if !self.attempt.is_from_access_point(&source.sll_addr) {
                continue;
            }
            match self.attempt.take_eapol(&Library, &frame[..length]) {
                Ok(steps) => steps.into_iter().try_for_each(|step| self.apply_step(step))?,
                Err(error) => log!("{}", to_untaken_line(error)),
            }
        }
    }

    fn apply_step(&mut self, step: Step) -> io::Result<()> {
        let (family, interface, access_point) = (self.family, self.interface, self.attempt.access_point);
        if let Step::Send(frame) = &step {
            return iface::send_packet(&self.frames, interface, HANDSHAKE, access_point, frame);
        }
        for (command, attributes) in netlink::key_requests(&step, interface, &access_point) {
            self.commands.request_ack(family, command, &attributes)?;
        }
        Ok(())
    }

    pub(super) fn quality(&mut self) -> io::Result<Option<Quality>> {
        let answer = self.commands.request(
            self.family,
            netlink::GET_STATION,
            &netlink::get_station_attributes(self.interface, &self.attempt.access_point),
        )?;
        Ok(netlink::parse_quality(&answer))
    }

    pub(super) fn leave(mut self) {
        let attributes = netlink::disconnect_attributes(self.interface);
        if let Err(e) = self.events.request_ack(self.family, netlink::DISCONNECT, &attributes) {
            log!("wifi: leaving the network: {e}");
        }
    }
}

enum LinkEvent {
    Joined,
    Lost(JoinError),
}
