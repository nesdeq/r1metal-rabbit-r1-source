use crate::log;
use r1_funcore::hw::uevent::{Uevent, parse};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

const MAX_MESSAGE: usize = 2048 + libc::PATH_MAX as usize;
const KERNEL_GROUP: u32 = 1;

pub struct Monitor {
    fd: OwnedFd,
}

impl Monitor {
    pub fn open() -> io::Result<Self> {
        let kind = libc::SOCK_DGRAM | libc::SOCK_NONBLOCK;
        let fd = super::socket(libc::AF_NETLINK, kind, libc::NETLINK_KOBJECT_UEVENT)?;
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_groups = KERNEL_GROUP;
        let length = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        super::check(unsafe { libc::bind(fd.as_raw_fd(), (&raw const address).cast(), length) })?;
        Ok(Monitor { fd })
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Takes every pending event.
    pub fn read(&self) -> Vec<Uevent> {
        let mut events = Vec::new();
        let mut buffer = [0u8; MAX_MESSAGE];
        loop {
            let count = unsafe { libc::recv(self.fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            if count > 0 {
                events.extend(parse(&buffer[..count as usize]));
                continue;
            }
            match super::check(count as libc::c_int).map_err(|e| (e.kind(), e)) {
                Err((io::ErrorKind::Interrupted, _)) => {}
                // No more waits; what else fails, a queue that overflowed and lost events among it, is told.
                Ok(_) | Err((io::ErrorKind::WouldBlock, _)) => return events,
                Err((_, e)) => {
                    log!("uevents: reading the kernel's: {e}");
                    return events;
                }
            }
        }
    }
}
