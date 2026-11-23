//! IPv6 as a station takes part in it: join settings, router solicitation (RFC 4861), the kernel's address list.

use alloc::vec::Vec;
use core::net::Ipv6Addr;

const DISABLE_SETTING: &str = "disable_ipv6";
const OFF: &str = "1";
const ON: &str = "0";
/// What a join for a stay writes of the kernel's IPv6 settings, in order: no duplicate address detection, routers asked at once, changing addresses, and IPv6 given up and taken up again, so that the interface forgets an earlier stay.
pub const JOIN_WRITES: [(&str, &str); 5] = [
    ("dad_transmits", "0"),
    ("router_solicitation_delay", "0"),
    ("use_tempaddr", "2"),
    (DISABLE_SETTING, OFF),
    (DISABLE_SETTING, ON),
];
/// What an interface that carries no IPv6 has written, a join that carries no traffic among them: IPv6 given up, so that no router's announcement routes traffic over the interface, and the kernel sends none of its own.
pub const OFF_WRITES: [(&str, &str); 1] = [(DISABLE_SETTING, OFF)];

const SOLICITATION: [u8; 8] = [133, 0, 0, 0, 0, 0, 0, 0];
const SOURCE_ADDRESS: [u8; 2] = [1, 1];
pub const ALL_ROUTERS: [u8; 16] = [0xFF, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
pub const LINK_HOPS: u8 = 255;
const SCOPE_FIELD: usize = 3;
const GLOBAL: &str = "00";
const UNUSABLE: u32 = 0x40 | 0x08;

/// The message that asks a link's routers for their announcement, with the asker's hardware address so that a router answers it alone and at once.
pub fn router_solicitation(station: [u8; 6]) -> Vec<u8> {
    [&SOLICITATION[..], &SOURCE_ADDRESS, &station].concat()
}

/// The first global address the kernel's list of IPv6 addresses holds on the interface of that name that it sends from.
pub fn find_usable_global(list: &str, interface: &str) -> Option<Ipv6Addr> {
    let line = list.lines().find(|line| is_usable_global(line, interface))?;
    u128::from_str_radix(line.split_whitespace().next()?, 16).ok().map(Ipv6Addr::from)
}

// A line of `/proc/net/if_inet6`: the address, the interface's index, the prefix's length, the scope, the marks and the interface's name.
fn is_usable_global(line: &str, interface: &str) -> bool {
    let mut fields = line.split_whitespace().skip(SCOPE_FIELD);
    let (scope, marks) = (fields.next(), fields.next().and_then(|marks| u32::from_str_radix(marks, 16).ok()));
    scope == Some(GLOBAL) && marks.is_some_and(|marks| marks & UNUSABLE == 0) && fields.next() == Some(interface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn solicitation_bytes() {
        assert_eq!(
            router_solicitation([2, 0, 0, 0, 0, 9]),
            [133, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 0, 0, 0, 0, 9],
            "the question, and the asker's hardware address in an option of eight bytes"
        );
        assert_eq!(Ipv6Addr::from(ALL_ROUTERS).to_string(), "ff02::2");
        assert_eq!(LINK_HOPS, u8::MAX, "a router takes no solicitation that came over another link");
    }

    #[test]
    fn join_writes() {
        assert_eq!(
            JOIN_WRITES[3..],
            [("disable_ipv6", "1"), ("disable_ipv6", "0")],
            "IPv6 given up and taken up again last, after the settings it then starts with"
        );
        assert_eq!(OFF_WRITES, [("disable_ipv6", "1")], "and given up alone where nothing is to go over it");
    }

    #[test]
    fn usable_globals() {
        let usable = |line: &str| is_usable_global(line, "wlan0");
        assert!(usable("20010db8000000000000000000000001 1e 40 00 00    wlan0"));
        assert!(usable("20010db8000000000000000000000001 1e 40 00 01    wlan0"), "one that changes with time");
        assert!(!usable("20010db8000000000000000000000001 1e 40 00 40    wlan0"), "one that is still being checked");
        assert!(!usable("20010db8000000000000000000000001 1e 40 00 08    wlan0"), "one that another station has");
        assert!(!usable("fe800000000000000000000000000001 1e 40 20 80    wlan0"), "one of the link alone");
        assert!(!usable("00000000000000000000000000000001 01 80 10 80       lo") && !usable("") && !usable("x y z 00"));
        let list = "20010db8000000000000000000000001 07 40 00 00  ccmni0\nfe800000000000000000000000000001 1e 40 20 80    wlan0\n";
        assert_eq!(
            (find_usable_global(list, "ccmni0"), find_usable_global(list, "wlan0")),
            (Some("2001:db8::1".parse().unwrap()), None),
            "a global address counts for its own interface's stay alone, and is read out of its line"
        );
    }
}
