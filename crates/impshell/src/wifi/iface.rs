use crate::sys;
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

pub(super) const NAME: &CStr = c"wlan0";

fn packet_address(interface: u32, protocol: u16, target: [u8; 6]) -> libc::sockaddr_ll {
    let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    (address.sll_family, address.sll_protocol, address.sll_ifindex) =
        (libc::AF_PACKET as u16, protocol.to_be(), interface as i32);
    address.sll_halen = target.len() as u8;
    address.sll_addr[..target.len()].copy_from_slice(&target);
    address
}

pub(super) fn packet_socket(interface: u32, protocol: u16) -> io::Result<OwnedFd> {
    let socket = sys::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_NONBLOCK, i32::from(protocol.to_be()))?;
    let address = packet_address(interface, protocol, [0; 6]);
    sys::check(unsafe {
        libc::bind(socket.as_raw_fd(), (&raw const address).cast(), size_of::<libc::sockaddr_ll>() as libc::socklen_t)
    })?;
    Ok(socket)
}

pub(super) fn send_packet(
    socket: &OwnedFd,
    interface: u32,
    protocol: u16,
    target: [u8; 6],
    packet: &[u8],
) -> io::Result<()> {
    let address = packet_address(interface, protocol, target);
    let result = unsafe {
        libc::sendto(
            socket.as_raw_fd(),
            packet.as_ptr().cast(),
            packet.len(),
            0,
            (&raw const address).cast(),
            size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    sys::check_count(result).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_address_layout() {
        let address = packet_address(7, 0x0800, [0xFF; 6]);
        assert_eq!(
            (address.sll_protocol, address.sll_ifindex, address.sll_halen, &address.sll_addr[..6]),
            (0x0008, 7, 6, &[0xFF; 6][..])
        );
    }
}
