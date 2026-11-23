use super::{IOC_WRITE, check, ioc, ioctl, socket, write_settings};
use std::ffi::{CStr, OsStr};
use std::fs::{File, OpenOptions};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const IPV6_SETTINGS: &str = "/proc/sys/net/ipv6/conf";
const ARP_SETTINGS: &str = "/proc/sys/net/ipv4/neigh";
// An Ethernet address (`ETH_ALEN`).
const HARDWARE_BYTES: usize = 6;
const TUN: &str = "/dev/net/tun";
// The kernel's `TUNSETIFF` (`include/uapi/linux/if_tun.h`).
const TUNSETIFF: u32 = ioc(IOC_WRITE, b'T', 202, size_of::<libc::c_int>());
/// What a packet socket names every protocol of a link by (`ETH_P_ALL`).
pub const EVERY_PROTOCOL: u16 = libc::ETH_P_ALL as u16;
// The kernel's `SIOCETHTOOL` (`include/uapi/linux/sockios.h`).
const SIOCETHTOOL: u32 = 0x8946;
// The kernel's `ETHTOOL_SGRO` (`include/uapi/linux/ethtool.h`).
const SET_MERGING: u32 = 0x2c;

// The kernel's `struct ethtool_value` (`include/uapi/linux/ethtool.h`).
#[repr(C)]
struct EthtoolValue {
    command: u32,
    data: u32,
}

// The kernel's `struct in6_ifreq` (`include/uapi/linux/ipv6.h`).
#[repr(C)]
struct In6Request {
    address: [u8; 16],
    prefix: u32,
    index: libc::c_int,
}

// A socket for an interface's ioctls.
fn control() -> io::Result<OwnedFd> {
    socket(libc::AF_INET, libc::SOCK_DGRAM, 0)
}

fn request(name: &CStr) -> libc::ifreq {
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (target, byte) in request.ifr_name.iter_mut().zip(name.to_bytes()) {
        *target = *byte as libc::c_char;
    }
    request
}

/// The interface's index, raised first where asked.
pub fn index(name: &CStr, should_raise: bool) -> io::Result<u32> {
    let socket = control()?;
    let mut request = request(name);
    unsafe {
        if should_raise {
            ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS as u32, &mut request)?;
            request.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
            ioctl(socket.as_raw_fd(), libc::SIOCSIFFLAGS as u32, &mut request)?;
        }
        ioctl(socket.as_raw_fd(), libc::SIOCGIFINDEX as u32, &mut request)?;
        Ok(request.ifr_ifru.ifru_ifindex as u32)
    }
}

/// Takes the interface down, which drops its routes and its IPv6 addresses.
pub fn down(name: &CStr) -> io::Result<()> {
    let socket = control()?;
    let mut request = request(name);
    unsafe {
        ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS as u32, &mut request)?;
        request.ifr_ifru.ifru_flags &= !(libc::IFF_UP as libc::c_short);
        ioctl(socket.as_raw_fd(), libc::SIOCSIFFLAGS as u32, &mut request)
    }
}

// An IPv4 address as the interface ioctls take one.
fn to_sockaddr(address: [u8; 4]) -> libc::sockaddr {
    let (storage, _) = super::socket_address(&SocketAddr::from((Ipv4Addr::from(address), 0)));
    unsafe { (&raw const storage).cast::<libc::sockaddr>().read() }
}

// Sets the interface's address or netmask, as `request` (`SIOCSIFADDR`, `SIOCSIFNETMASK`) names it.
fn set_address(socket: &OwnedFd, name: &CStr, request: libc::c_ulong, address: [u8; 4]) -> io::Result<()> {
    let mut ifreq = self::request(name);
    ifreq.ifr_ifru.ifru_addr = to_sockaddr(address);
    unsafe { ioctl(socket.as_raw_fd(), request as u32, &mut ifreq) }
}

/// Puts an IPv4 address and its netmask on the interface.
pub fn set_ipv4(name: &CStr, address: [u8; 4], mask: [u8; 4]) -> io::Result<()> {
    let socket = control()?;
    set_address(&socket, name, libc::SIOCSIFADDR, address)?;
    set_address(&socket, name, libc::SIOCSIFNETMASK, mask)
}

/// Takes the interface's IPv4 address off, and with its last address the kernel drops its IPv4 routes (`fib_inetaddr_event`).
pub fn clear_ipv4(name: &CStr) -> io::Result<()> {
    set_address(&control()?, name, libc::SIOCSIFADDR, [0; 4])
}

/// The interface's IPv4 address; none where it has none.
pub fn ipv4(name: &CStr) -> io::Result<Option<[u8; 4]>> {
    let socket = control()?;
    let mut request = request(name);
    match unsafe { ioctl(socket.as_raw_fd(), libc::SIOCGIFADDR as u32, &mut request) } {
        Ok(()) => {
            let address = unsafe { request.ifr_ifru.ifru_addr.sa_data };
            Ok(Some(std::array::from_fn(|at| address[2 + at].to_ne_bytes()[0])))
        }
        Err(e) if e.raw_os_error() == Some(libc::EADDRNOTAVAIL) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The largest packet the interface carries.
pub fn mtu(name: &CStr) -> io::Result<u32> {
    let socket = control()?;
    let mut request = request(name);
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCGIFMTU as u32, &mut request) }?;
    Ok(unsafe { request.ifr_ifru.ifru_mtu } as u32)
}

/// Sets whether the kernel merges the TCP segments that come over the interface into larger packets before anything reads them (generic receive offload).
pub fn set_merging(name: &CStr, is_merging: bool) -> io::Result<()> {
    let socket = control()?;
    let mut request = request(name);
    let mut value = EthtoolValue { command: SET_MERGING, data: u32::from(is_merging) };
    request.ifr_ifru.ifru_data = (&raw mut value).cast();
    unsafe { ioctl(socket.as_raw_fd(), SIOCETHTOOL, &mut request) }
}

/// True while the interface is up and its link carries.
pub fn is_running(name: &CStr) -> io::Result<bool> {
    let socket = control()?;
    let mut request = request(name);
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCGIFFLAGS as u32, &mut request) }?;
    Ok(unsafe { request.ifr_ifru.ifru_flags } & libc::IFF_RUNNING as libc::c_short != 0)
}

/// Makes a TUN interface of that name, which lasts as long as the file: each read gives an IP packet the kernel routes over it, each write hands the kernel one; neither waits.
pub fn open_tun(name: &CStr) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(TUN)?;
    let mut request = request(name);
    request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
    unsafe { ioctl(file.as_raw_fd(), TUNSETIFF, &mut request) }?;
    Ok(file)
}

/// A socket for every interface's packets of one protocol of the link, or of all (`EVERY_PROTOCOL`), as `kind` (`SOCK_RAW` with the link's head, `SOCK_DGRAM` without) hands them over; it never waits.
pub fn open_packets(protocol: u16, kind: libc::c_int) -> io::Result<OwnedFd> {
    socket(libc::AF_PACKET, kind | libc::SOCK_NONBLOCK, i32::from(protocol.to_be()))
}

/// `open_packets` for one interface's packets alone, which the socket also sends over it.
pub fn open_packets_on(name: &CStr, protocol: u16, kind: libc::c_int) -> io::Result<OwnedFd> {
    let (index, socket) = (index(name, false)?, open_packets(protocol, kind)?);
    let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    (address.sll_family, address.sll_protocol, address.sll_ifindex) =
        (libc::AF_PACKET as libc::c_ushort, protocol.to_be(), index as libc::c_int);
    check(unsafe {
        libc::bind(socket.as_raw_fd(), (&raw const address).cast(), size_of::<libc::sockaddr_ll>() as libc::socklen_t)
    })?;
    Ok(socket)
}

/// A socket that is readable when the kernel tells news of those groups of its routing (`RTMGRP_*`); it never waits.
pub fn route_events(groups: libc::c_int) -> io::Result<OwnedFd> {
    let socket = socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_NONBLOCK, libc::NETLINK_ROUTE)?;
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    (address.nl_family, address.nl_groups) = (libc::AF_NETLINK as libc::sa_family_t, groups as u32);
    check(unsafe {
        libc::bind(socket.as_raw_fd(), (&raw const address).cast(), size_of::<libc::sockaddr_nl>() as libc::socklen_t)
    })?;
    Ok(socket)
}

/// Takes the news off such a socket, which tells only that there is some.
pub fn drain(events: &OwnedFd) {
    let mut message = [0u8; super::PAGE];
    while unsafe { libc::recv(events.as_raw_fd(), message.as_mut_ptr().cast(), message.len(), 0) } > 0 {}
}

/// Adds the default route over the interface, through `gateway` where it has one.
pub fn add_default_route(name: &CStr, gateway: Option<[u8; 4]>) -> io::Result<()> {
    let socket = control()?;
    let mut route: libc::rtentry = unsafe { std::mem::zeroed() };
    (route.rt_dst, route.rt_genmask) = (to_sockaddr([0; 4]), to_sockaddr([0; 4]));
    route.rt_flags = libc::RTF_UP;
    if let Some(gateway) = gateway {
        route.rt_gateway = to_sockaddr(gateway);
        route.rt_flags |= libc::RTF_GATEWAY;
    }
    route.rt_dev = name.as_ptr().cast_mut();
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCADDRT as u32, &mut route) }
}

pub fn set_mtu(name: &CStr, mtu: u32) -> io::Result<()> {
    let socket = control()?;
    let mut request = request(name);
    request.ifr_ifru.ifru_mtu = mtu as libc::c_int;
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCSIFMTU as u32, &mut request) }
}

/// Adds an IPv6 address with its prefix to the interface of that index.
pub fn add_ipv6_address(index: u32, address: [u8; 16], prefix: u32) -> io::Result<()> {
    let socket = socket(libc::AF_INET6, libc::SOCK_DGRAM, 0)?;
    let mut request = In6Request { address, prefix, index: index as libc::c_int };
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCSIFADDR as u32, &mut request) }
}

/// Writes the kernel's IPv6 settings of the interface in order (`net::ipv6`).
pub fn write_ipv6(name: &CStr, tag: &str, writes: &[(&str, &str)]) {
    write_settings(&Path::new(IPV6_SETTINGS).join(OsStr::from_bytes(name.to_bytes())), "IPv6", tag, writes);
}

/// Writes the kernel's ARP settings of the interface in order (`net::arp`).
pub fn write_arp(name: &CStr, tag: &str, writes: &[(&str, &str)]) {
    write_settings(&Path::new(ARP_SETTINGS).join(OsStr::from_bytes(name.to_bytes())), "ARP", tag, writes);
}

pub fn hardware_address(name: &CStr) -> io::Result<[u8; HARDWARE_BYTES]> {
    let socket = control()?;
    let mut request = request(name);
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCGIFHWADDR as u32, &mut request) }?;
    let address = unsafe { request.ifr_ifru.ifru_hwaddr.sa_data };
    Ok(std::array::from_fn(|at| address[at].to_ne_bytes()[0]))
}

pub fn set_hardware_address(name: &CStr, address: [u8; HARDWARE_BYTES]) -> io::Result<()> {
    let socket = control()?;
    let mut request = request(name);
    let mut hardware: libc::sockaddr = unsafe { std::mem::zeroed() };
    hardware.sa_family = libc::ARPHRD_ETHER;
    for (target, byte) in hardware.sa_data.iter_mut().zip(address) {
        *target = byte as libc::c_char;
    }
    request.ifr_ifru.ifru_hwaddr = hardware;
    unsafe { ioctl(socket.as_raw_fd(), libc::SIOCSIFHWADDR as u32, &mut request) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockaddr_layout() {
        let address = to_sockaddr([192, 168, 1, 57]);
        assert_eq!(
            (address.sa_family, &address.sa_data[2..6]),
            (libc::AF_INET as libc::sa_family_t, &[192u8 as libc::c_char, 168u8 as libc::c_char, 1, 57][..]),
            "the family, a port of nothing, and the four bytes in the network's order"
        );
    }

    #[test]
    fn request_names_interface() {
        let name = request(c"ccmni0").ifr_name;
        assert_eq!(name[..7].iter().map(|byte| byte.to_ne_bytes()[0]).collect::<Vec<_>>(), b"ccmni0\0");
        assert_eq!(
            (
                size_of::<In6Request>(),
                std::mem::offset_of!(In6Request, prefix),
                std::mem::offset_of!(In6Request, index)
            ),
            (24, 16, 20),
            "the address, the prefix and the index, as the kernel's in6_ifreq"
        );
    }

    #[test]
    fn loopback_read_and_tun_number() {
        assert_eq!(ipv4(c"lo").unwrap(), Some([127, 0, 0, 1]));
        assert!(mtu(c"lo").unwrap() >= 1500 && is_running(c"lo").unwrap(), "the loopback carries, whole packets too");
        assert!(ipv4(c"r1-none").is_err() && mtu(c"r1-none").is_err(), "an interface that is not there");
        assert_eq!(TUNSETIFF, 0x4004_54CA, "`_IOW('T', 202, int)`");
        assert_eq!((size_of::<EthtoolValue>(), std::mem::offset_of!(EthtoolValue, data)), (8, 4), "ethtool_value");
        assert!(set_merging(c"r1-none", false).is_err(), "an interface that is not there merges nothing");
        let events = route_events(libc::RTMGRP_LINK).unwrap();
        drain(&events);
        assert_eq!(
            unsafe { libc::recv(events.as_raw_fd(), [0u8; 1].as_mut_ptr().cast(), 1, 0) },
            -1,
            "with no news the socket gives nothing, and does not wait"
        );
    }
}
