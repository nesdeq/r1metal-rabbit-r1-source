//! DNS (RFC 1035): the resolver over a stream (RFC 7858), what a server is named by, a network's NAT64 as its resolver tells it (RFC 7050), and a relay of another station's questions to the resolver.

mod batch;
mod cache;
mod lookup;
mod message;
pub mod nat64;
mod relay;
mod state;

pub use batch::{Batch, BatchError, NoAddress};
pub use lookup::Lookup;
pub use message::message_length;
pub use relay::{Relay, Streams, to_relay_line};
pub use state::{State, Step};

use alloc::format;
use alloc::string::String;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Cloudflare's resolvers, which check signatures, a family in turn.
pub const RESOLVERS: [IpAddr; 4] = [
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
    IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1001)),
    IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)),
];
/// The name their certificate is for.
pub const RESOLVER_NAME: &str = "one.one.one.one";
/// The port of DNS over TLS.
pub const PORT: u16 = 853;
pub const NO_ANSWER: &str = "the resolver sent what is no answer";

/// The log line of the port's watch.
pub fn to_watch_line(line: &str) -> String {
    format!("names: {line}")
}

/// The log line of an asking over TLS, its times in milliseconds.
pub fn to_reached_line(
    address: &str,
    connect_ms: u128,
    tls: &str,
    tls_ms: u128,
    names: usize,
    answer_ms: u128,
) -> String {
    format!(
        "names: {RESOLVER_NAME} reached at {address} in {connect_ms} ms, {tls} after {tls_ms} ms, the answers for {names} names after {answer_ms} ms"
    )
}

/// Why a name has no addresses: it is no name a server can have, the resolver's answers give it none, or the asking could not be made (`E`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unresolved<E> {
    Invalid,
    NoAddress(NoAddress),
    Failed(E),
}

fn canonical(name: &str) -> String {
    name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase()
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Host {
    Address(IpAddr),
    Name(String),
    Invalid,
}

impl Host {
    fn parse(name: &str) -> Host {
        if let Ok(address) = name.parse::<IpAddr>() {
            return Host::Address(address);
        }
        let name = canonical(name);
        if message::question(0, &name, message::TYPE_A).is_none() { Host::Invalid } else { Host::Name(name) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_families() {
        assert_eq!(RESOLVERS.map(|resolver| resolver.is_ipv6()), [true, false, true, false]);
    }

    #[test]
    fn log_lines() {
        assert_eq!(to_watch_line("53 answered 2 of 3"), "names: 53 answered 2 of 3");
        assert_eq!(
            to_reached_line("1.1.1.1:853", 20, "TLS 1.3", 61, 2, 80),
            "names: one.one.one.one reached at 1.1.1.1:853 in 20 ms, TLS 1.3 after 61 ms, the answers for 2 names after 80 ms"
        );
    }

    #[test]
    fn host_parse() {
        assert_eq!(canonical("One.One.One.ONE."), "one.one.one.one");
        assert_eq!(Host::parse("192.0.2.9"), Host::Address(IpAddr::from([192, 0, 2, 9])));
        assert_eq!(Host::parse("2001:db8::1"), Host::Address("2001:db8::1".parse().unwrap()));
        assert_eq!(Host::parse("One.One.One.ONE."), Host::Name("one.one.one.one".into()));
        assert_eq!((Host::parse("a..b"), Host::parse("")), (Host::Invalid, Host::Invalid));
    }
}
