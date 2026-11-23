//! The modem's network interface a data connection runs on (`ccmniN`): its id, its addresses and MTU, how the AP sets them, and what a routed stay takes off it for the router's forwarder.

use crate::net::nat::Outside;
use crate::net::siit;
use alloc::format;
use alloc::string::String;
use core::net::{Ipv4Addr, Ipv6Addr};

/// The interface the device's own IPv4 packets leave by in a routed stay: a TUN device the router's forwarder reads.
pub const OWN_INTERFACE: &str = "tun0";
/// The address of the device's own stack on it (RFC 7335).
pub const OWN_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 0, 2);

/// Why a stay that was to be routed is none, with what the kernel refused.
pub fn to_unrouted_text(error: &str) -> String {
    format!("the modem's connection could not be routed: {error}")
}

/// A routed stay's connection as the forwarder takes it over: the IPv4 address taken off the modem's interface, none on a connection of IPv6 alone, and the largest packet the interface carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Routed {
    pub address: Option<Ipv4Addr>,
    pub mtu: usize,
}

impl Routed {
    /// The outer end of the way IPv4 packets leave by: the connection's own address, or on a connection of IPv6 alone the one they carry into their translation, which costs them the difference of the two heads.
    pub fn to_outside(self) -> Outside {
        match self.address {
            Some(address) => Outside { address, mtu: self.mtu },
            None => Outside { address: siit::ADDRESS, mtu: self.mtu.saturating_sub(siit::GROWTH) },
        }
    }
}

/// The netmask of the interface's IPv4 address, one of its own on a link of two, and the prefix of its IPv6 address, as MediaTek's NetAgent sets them (`IPV4_REFIX_LENGTH`, `IPV6_REFIX_LENGTH`).
pub const IPV4_MASK: [u8; 4] = [255; 4];
pub const IPV6_PREFIX: u32 = 64;
// The modem numbers an interface `100 * n + index`, `index` that of its `ccmni` device (nixos-agm-m7, `docs/MODEM-RE.md` 9).
const ID_FACTOR: u32 = 100;

/// A network interface of the modem as it names one: its id, `100 * n + index`, and its addresses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interface {
    pub(super) id: u32,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub mtu: Option<u32>,
}

impl Interface {
    /// The index of its `ccmni` device.
    pub const fn index(&self) -> u32 {
        self.id % ID_FACTOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_index() {
        let interface = |id| Interface { id, ipv4: None, ipv6: None, mtu: None };
        assert_eq!([100, 201, 302].map(|id| interface(id).index()), [0, 1, 2], "ccmni0, ccmni1, ccmni2");
    }

    #[test]
    fn routed_outside() {
        let address = Ipv4Addr::new(10, 64, 3, 7);
        assert_eq!(
            Routed { address: Some(address), mtu: 1500 }.to_outside(),
            Outside { address, mtu: 1500 },
            "a connection of IPv4: its own address, and whole packets"
        );
        assert_eq!(
            Routed { address: None, mtu: 1500 }.to_outside(),
            Outside { address: Ipv4Addr::new(192, 0, 0, 1), mtu: 1480 },
            "one of IPv6 alone: the address before the translation, and twenty bytes less"
        );
        assert_eq!((OWN_INTERFACE, OWN_ADDRESS), ("tun0", Ipv4Addr::new(192, 0, 0, 2)));
        assert_eq!(
            to_unrouted_text("No such device (os error 19)"),
            "the modem's connection could not be routed: No such device (os error 19)"
        );
    }
}
