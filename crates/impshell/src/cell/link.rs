use crate::sys::iface;
use r1_funcore::cell::link::{IPV4_MASK, IPV6_PREFIX, Interface, OWN_ADDRESS, OWN_INTERFACE, Routed};
use r1_funcore::net::ipv6::JOIN_WRITES;
use std::ffi::CString;
use std::io;
use std::net::Ipv4Addr;

// The kernel names the modem's network interface N `ccmniN` (`ccmni/ccmni.c`).
pub(super) fn to_name(index: u32) -> String {
    format!("ccmni{index}")
}

fn name(interface: &Interface) -> io::Result<CString> {
    Ok(CString::new(to_name(interface.index()))?)
}

// Puts the connection's MTU and addresses on the modem's interface, with the default route over it; the kernel takes the IPv6 prefix from the routers' announcement after the link-local address.
pub(super) fn configure(interface: &Interface) -> io::Result<()> {
    let name = name(interface)?;
    iface::down(&name)?;
    iface::write_ipv6(&name, "cell", &JOIN_WRITES);
    if let Some(mtu) = interface.mtu {
        iface::set_mtu(&name, mtu)?;
    }
    let index = iface::index(&name, true)?;
    if let Some(address) = interface.ipv4 {
        iface::set_ipv4(&name, address.octets(), IPV4_MASK)?;
        iface::add_default_route(&name, None)?;
    }
    if let Some(address) = interface.ipv6 {
        iface::add_ipv6_address(index, address.octets(), IPV6_PREFIX)?;
    }
    Ok(())
}

// Takes the IPv4 address off the modem's interface, and the interface down even where that fails, as only the down drops its routes.
pub(super) fn unconfigure(interface: &Interface) -> io::Result<()> {
    let name = name(interface)?;
    let removed = iface::clear_ipv4(&name);
    iface::down(&name).and(removed)
}

// Hands a configured connection's IPv4 over to the router's forwarder: its address goes off the modem's interface of that name, and with it the default route, so that the kernel answers nothing that comes for it, and its segments come unmerged, each a packet the forwarder can pass on; the device's own stack gets its address on the forwarder's TUN interface, with the default route over it.
pub(super) fn route(name: &str) -> io::Result<Routed> {
    let (name, own) = (CString::new(name)?, CString::new(OWN_INTERFACE)?);
    let routed = Routed { address: iface::ipv4(&name)?.map(Ipv4Addr::from), mtu: iface::mtu(&name)? as usize };
    // The TUN interface first: where the forwarder has made none, the modem's interface is left as it is.
    iface::set_mtu(&own, routed.to_outside().mtu as u32)?;
    iface::index(&own, true)?;
    iface::set_ipv4(&own, OWN_ADDRESS.octets(), IPV4_MASK)?;
    iface::set_merging(&name, false)?;
    iface::clear_ipv4(&name)?;
    iface::add_default_route(&own, None)?;
    Ok(routed)
}

// Takes the device's own stack off the forwarder's TUN interface, the interface down even where that fails, and has the modem's interface of that name merge segments again for the kernel's own connections.
pub(super) fn unroute(name: &str) -> io::Result<()> {
    let (name, own) = (CString::new(name)?, CString::new(OWN_INTERFACE)?);
    let removed = iface::clear_ipv4(&own);
    iface::down(&own).and(removed).and(iface::set_merging(&name, true))
}
