use crate::sys::{self, Epoll};
use crate::wifi::association::Association;
use crate::wifi::{Preempt, iface};
use crate::{crypto, log};
use r1_funcore::net::MTU;
use r1_funcore::net::dhcp::{Client, Lease, Next, Step};
use r1_funcore::net::ipv6::{self, ALL_ROUTERS, LINK_HOPS};
use r1_funcore::wifi::join::JoinError;
use r1_funcore::wifi::time_left;
use std::io;
use std::net::{SocketAddr, SocketAddrV6};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

const IPV4: u16 = 0x0800;
const BROADCAST: [u8; 6] = [0xFF; 6];
const PACKETS: u64 = 0;
const NUDGE: u64 = 1;
const LINK_EVENTS: u64 = 2;
const LINK_FRAMES: u64 = 3;

pub(super) fn solicit(interface: u32, station: [u8; 6]) -> io::Result<()> {
    let socket = sys::socket(libc::AF_INET6, libc::SOCK_RAW, libc::IPPROTO_ICMPV6)?;
    let hops = libc::c_int::from(LINK_HOPS);
    for (option, value) in [(libc::IPV6_MULTICAST_HOPS, hops), (libc::IPV6_MULTICAST_IF, interface as libc::c_int)] {
        sys::set_option(&socket, libc::IPPROTO_IPV6, option, &value)?;
    }
    let target = SocketAddr::V6(SocketAddrV6::new(ALL_ROUTERS.into(), 0, 0, interface));
    sys::send_to(&socket, &target, &ipv6::router_solicitation(station))
}

pub(super) fn request_lease(
    link: &mut Association,
    preempt: &Preempt,
    limit: Duration,
) -> io::Result<Result<Lease, JoinError>> {
    let interface = link.interface;
    let socket = iface::packet_socket(interface, IPV4)?;
    let send = |message: &[u8]| iface::send_packet(&socket, interface, IPV4, BROADCAST, message);
    let id = crypto::random::<4>().ok_or_else(|| io::Error::other("no random bytes for a number to ask under"))?;
    let mut client = Client::new(link.station(), u32::from_be_bytes(id));
    let (events, frames) = link.fds();
    let mut epoll = Epoll::new()?;
    epoll.add(socket.as_raw_fd(), PACKETS)?;
    epoll.add(preempt.nudge.fd(), NUDGE)?;
    epoll.add(events, LINK_EVENTS)?;
    epoll.add(frames, LINK_FRAMES)?;
    let start = Instant::now();
    send(&client.message())?;
    let mut packet = [0u8; MTU];
    loop {
        let elapsed = start.elapsed();
        let wait = match client.next(elapsed, time_left(elapsed, limit)) {
            Next::Wait(wait) => wait,
            Next::Send => {
                send(&client.message())?;
                continue;
            }
            Next::Silent => return Ok(Err(JoinError::Silent)),
        };
        let ready = epoll.wait(Some(wait))?;
        if ready.contains(&NUDGE) && preempt.should_yield() {
            return Ok(Err(JoinError::GaveWay));
        }
        if let Some(error) = link.read_news(ready.contains(&LINK_FRAMES), ready.contains(&LINK_EVENTS))? {
            return Ok(Err(error));
        }
        if ready.contains(&PACKETS) {
            loop {
                let length = match sys::check_count(unsafe {
                    libc::recv(socket.as_raw_fd(), packet.as_mut_ptr().cast(), packet.len(), 0)
                }) {
                    Ok(length) => length,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                };
                match client.take_packet(&packet[..length], start.elapsed()) {
                    Some(Step::Bound(lease)) => return Ok(Ok(lease)),
                    Some(Step::Send) => send(&client.message())?,
                    None => {}
                }
            }
        }
    }
}

pub(super) fn apply_lease(lease: &Lease) -> io::Result<()> {
    sys::iface::set_ipv4(iface::NAME, lease.address, lease.mask)?;
    if let Some(router) = lease.router {
        sys::iface::add_default_route(iface::NAME, Some(router))?;
    }
    Ok(())
}

pub(super) fn clear_address() {
    if let Err(e) = sys::iface::clear_ipv4(iface::NAME) {
        log!("wifi: taking the address off the interface: {e}");
    }
}
