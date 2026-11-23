use crate::sys;
use crate::wifi::iface;
use r1_funcore::wifi::TIMEOUT;
use r1_funcore::wifi::netlink::{self, Channel, Family, Message};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

pub(super) struct Socket {
    socket: OwnedFd,
    sequence: u32,
}

impl Socket {
    pub(super) fn open() -> io::Result<Socket> {
        let socket = sys::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_GENERIC)?;
        // The fields take the width the target's C library gives them, which libc names by aliases it is changing for musl.
        let timeout = libc::timeval { tv_sec: TIMEOUT.as_secs() as _, tv_usec: TIMEOUT.subsec_micros().into() };
        sys::set_option(&socket, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &timeout)?;
        Ok(Socket { socket, sequence: 0 })
    }

    pub(super) fn fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    pub(super) fn request_ack(&mut self, family: u16, command: u8, attributes: &[u8]) -> io::Result<()> {
        self.send(family, netlink::ACKNOWLEDGE, command, 0, attributes)?;
        let sequence = self.sequence;
        self.read(|message| message.is_ack_of(sequence))
    }

    pub(super) fn request(&mut self, family: u16, command: u8, attributes: &[u8]) -> io::Result<Vec<u8>> {
        self.send(family, 0, command, 0, attributes)?;
        let sequence = self.sequence;
        let mut answer = Vec::new();
        self.read(|message| {
            if let Some((_, attributes)) = message.command_of(sequence) {
                answer = attributes.to_vec();
            }
            message.is_answer_of(sequence)
        })?;
        Ok(answer)
    }

    pub(super) fn send(
        &mut self,
        family: u16,
        flags: u16,
        command: u8,
        version: u8,
        attributes: &[u8],
    ) -> io::Result<()> {
        self.sequence += 1;
        let message = netlink::request(family, flags, self.sequence, command, version, attributes);
        let sent = sys::check_count(unsafe {
            libc::send(self.socket.as_raw_fd(), message.as_ptr().cast(), message.len(), 0)
        })?;
        if sent == message.len() { Ok(()) } else { Err(io::Error::other("the kernel took a part of a request")) }
    }

    pub(super) fn listen(&self, group: u32) -> io::Result<()> {
        sys::set_option(&self.socket, libc::SOL_NETLINK, libc::NETLINK_ADD_MEMBERSHIP, &group)
    }

    pub(super) fn datagram(&self) -> io::Result<Vec<u8>> {
        let fd = self.socket.as_raw_fd();
        let length =
            sys::check_count(unsafe { libc::recv(fd, std::ptr::null_mut(), 0, libc::MSG_PEEK | libc::MSG_TRUNC) })?;
        let mut datagram = vec![0u8; length];
        let count = sys::check_count(unsafe { libc::recv(fd, datagram.as_mut_ptr().cast(), length, 0) })?;
        datagram.truncate(count);
        Ok(datagram)
    }

    fn read(&self, mut is_last: impl FnMut(&Message) -> bool) -> io::Result<()> {
        loop {
            for message in netlink::messages(&self.datagram()?) {
                if let Some(code) = message.error() {
                    return Err(io::Error::from_raw_os_error(code));
                }
                if is_last(&message) {
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn wireless_family(&mut self) -> io::Result<Family> {
        let mut attributes = Vec::new();
        netlink::add_attribute(&mut attributes, netlink::FAMILY_NAME, netlink::WIRELESS);
        self.send(netlink::CONTROL, 0, netlink::GET_FAMILY, netlink::CONTROL_VERSION, &attributes)?;
        let mut family = None;
        self.read(|message| {
            family = message.command().and_then(|(_, attributes)| netlink::parse_family(attributes));
            true
        })?;
        family.ok_or_else(|| io::Error::other("the kernel does not name its wireless family"))
    }

    pub(super) fn dump(
        &mut self,
        family: u16,
        command: u8,
        attributes: &[u8],
        mut handle: impl FnMut(u8, &[u8]),
    ) -> io::Result<()> {
        self.send(family, netlink::DUMP, command, 0, attributes)?;
        let sequence = self.sequence;
        self.read(|message| {
            if let Some((command, attributes)) = message.command_of(sequence) {
                handle(command, attributes);
            }
            message.is_end_of(sequence)
        })
    }
}

pub(super) fn channels() -> io::Result<Vec<Channel>> {
    let attributes =
        netlink::interface_attributes(sys::iface::index(iface::NAME, false)?, &[(netlink::SPLIT_WIPHY_DUMP, &[])]);
    let mut socket = Socket::open()?;
    let family = socket.wireless_family()?;
    let mut channels = Vec::new();
    socket.dump(family.id, netlink::GET_WIPHY, &attributes, |command, attributes| {
        channels.extend(netlink::to_channels(command, attributes));
    })?;
    Ok(channels)
}
