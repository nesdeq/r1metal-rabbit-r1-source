//! The router: the device as a modem for one host on its cable, whose packets leave over the modem's connection or, with the tunnel on, through a WireGuard tunnel and nowhere else.

mod forward;
mod state;

pub use forward::{Machine, Step};
pub use state::{Answer, Asker, Order, Report, State, Wishes};

use crate::base::code::unbase64;
use crate::net::wireguard::config::{Config, FILE, Flaw};
use alloc::format;
use alloc::string::String;
use core::net::Ipv4Addr;

/// The directory of the tunnel's config, under the data partition's private directory.
pub const DIRECTORY: &str = ".r1metal/router";
// The router's address on the link to the host, which the host has as its router and its resolver.
const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 42, 1);
// The one address lent on that link.
const HOST: Ipv4Addr = Ipv4Addr::new(192, 168, 42, 2);
// A block of four: the two stations, the block's own address and the one for all its stations.
const MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
// The log line of a config taken off the partition.
const FORGOTTEN_LINE: &str = "router: the tunnel's config is kept no more";
// The log line of a kept config that does not read.
const UNREAD_LINE: &str = "router: the kept config of the tunnel does not read, and none counts as kept";
// Why a peer whose name has no address is not reached.
const NO_ADDRESS: &str = "the peer has no address";
const UNFIT_LINE: &str =
    "router: the tunnel is not set up: the connection carries too little, or the library gives no key";
const NO_NUMBER_LINE: &str = "router: no random bytes for the address the translated packets leave from";
const NO_NAT64_LINE: &str = "router: the network tells no NAT64, and the host's packets have no way";
const UNROUTED_LINE: &str = "router: the stay is not routed, and is waited out";

// The log line of a config kept.
fn to_kept_line() -> String {
    format!("router: a tunnel's config is kept in {DIRECTORY}/{FILE}")
}

// The log line of the host's link come up or gone down.
const fn to_link_line(is_up: bool) -> &'static str {
    if is_up { "router: a host's link is up" } else { "router: the host's link is down" }
}

fn to_refused_line(reason: &str) -> String {
    format!("router: no stay in the modem's network: {reason}")
}

fn to_routed_line(mtu: usize) -> String {
    format!("router: the stay is routed, packets of {mtu} bytes")
}

fn to_unfound_line(reason: &str) -> String {
    format!("router: the tunnel's peer is not reached: {reason}")
}

fn to_untranslated_line(reason: &str) -> String {
    format!("router: no translation to IPv6: {reason}")
}

fn is_of_link(address: Ipv4Addr) -> bool {
    (address & MASK) == (GATEWAY & MASK)
}

// Reads the text `wg-quick` takes as a config the router keeps: one the firmware takes, with no address on the link to the host.
pub(crate) fn read_config(text: &str) -> Result<Config, Flaw> {
    let config = Config::parse(text)?;
    if is_of_link(config.address) || is_of_link(config.resolver) { Err(Flaw::Clash) } else { Ok(config) }
}

/// Reads the config a run before kept, or gives the log line of one that does not read.
pub fn read_kept(text: &str) -> Result<Config, &'static str> {
    Config::parse_kept(text).ok_or(UNREAD_LINE)
}

// Reads a config the console hands over: its text in base64.
pub(super) fn read_handover(code: &str) -> Result<Config, Flaw> {
    let text = unbase64(code).and_then(|bytes| String::from_utf8(bytes).ok()).ok_or(Flaw::NotAConfig)?;
    read_config(&text)
}

/// How the router stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    // The device shows a host no network adapter.
    Absent,
    Off,
    // On, and no host's link is up.
    Alone,
    // A host's link is up and the modem's connection is not routed yet.
    Joining,
    Lte,
    Tunnel,
    // The tunnel is wanted and not up: the host's packets are dropped.
    Shut,
    // The tunnel is wanted and no config is kept: the host's packets are dropped.
    Unset,
}

impl Standing {
    // The log line of the router come to stand so.
    const fn to_log_line(self) -> &'static str {
        match self {
            Standing::Absent => "router: no network adapter for a host, nothing is routed",
            Standing::Off => "router: off",
            Standing::Alone => "router: on, no host's link is up",
            Standing::Joining => "router: a host waits for the modem's connection",
            Standing::Lte => "router: the host's packets leave over the modem's connection",
            Standing::Tunnel => "router: the host's packets leave through the tunnel",
            Standing::Shut => "router: the tunnel is not up, the host's packets are dropped",
            Standing::Unset => "router: no config of a tunnel is kept, the host's packets are dropped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::base64;
    use crate::net::wireguard::config::tests::TEXT;

    #[test]
    fn configs_read() {
        assert_eq!(read_config(TEXT).map(|config| config.address), Ok(Ipv4Addr::new(10, 2, 0, 2)));
        assert_eq!(
            read_handover(&base64(TEXT.as_bytes())).map(|config| config.resolver),
            Ok(Ipv4Addr::new(10, 2, 0, 1))
        );
        for (address, reason) in [
            ("192.168.42.2", "the host's own"),
            ("192.168.42.0", "the block's"),
            ("192.168.42.3", "the one for all its stations"),
        ] {
            assert_eq!(read_config(&TEXT.replace("10.2.0.2", address)), Err(Flaw::Clash), "{reason}");
        }
        assert_eq!(
            read_config(&TEXT.replace("10.2.0.1", "192.168.42.1")),
            Err(Flaw::Clash),
            "a resolver at the router's"
        );
        assert!(read_config(&TEXT.replace("10.2.0.2", "192.168.42.4")).is_ok(), "the next block is another's");
        assert_eq!(read_config(&TEXT.replace("0.0.0.0/0, ", "")), Err(Flaw::NotWhole), "what the firmware refuses");
        assert_eq!(
            [read_handover("not base64!").err(), read_handover(&base64(&[0xFF, 0xFE])).err(), read_handover("").err()],
            [Some(Flaw::NotAConfig); 3],
            "what is no base64, no text, or nothing"
        );
        let kept = read_config(TEXT).unwrap();
        assert_eq!(read_kept(&kept.to_text()), Ok(kept), "a kept config reads back");
        assert_eq!(read_kept("private x\n"), Err(UNREAD_LINE), "one that does not read is told in the log");
    }

    #[test]
    fn log_lines() {
        assert_eq!(to_kept_line(), "router: a tunnel's config is kept in .r1metal/router/tunnel");
        assert_eq!(
            [to_link_line(true), to_link_line(false)],
            ["router: a host's link is up", "router: the host's link is down"]
        );
        assert_eq!(
            to_refused_line("the modem is not ready"),
            "router: no stay in the modem's network: the modem is not ready"
        );
        assert_eq!(to_routed_line(1500), "router: the stay is routed, packets of 1500 bytes");
        assert_eq!(to_unfound_line("no such name"), "router: the tunnel's peer is not reached: no such name");
        assert_eq!(to_untranslated_line("the network is gone"), "router: no translation to IPv6: the network is gone");
        assert_eq!(
            [
                Standing::Absent,
                Standing::Off,
                Standing::Alone,
                Standing::Joining,
                Standing::Lte,
                Standing::Tunnel,
                Standing::Shut,
                Standing::Unset
            ]
            .map(Standing::to_log_line),
            [
                "router: no network adapter for a host, nothing is routed",
                "router: off",
                "router: on, no host's link is up",
                "router: a host waits for the modem's connection",
                "router: the host's packets leave over the modem's connection",
                "router: the host's packets leave through the tunnel",
                "router: the tunnel is not up, the host's packets are dropped",
                "router: no config of a tunnel is kept, the host's packets are dropped",
            ]
        );
    }
}
