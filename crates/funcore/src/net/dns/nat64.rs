//! A network's NAT64: its prefix, which an IPv6 network alone tells through its own resolver (RFC 7050) and a stay searches for until the resolver has told it, and the IPv6 address it reaches an IPv4 one at (RFC 6052).

use super::message::{TYPE_AAAA, parse_answer, question};
use crate::base::handoff::Turns;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

// The name whose IPv4 addresses the network's resolver writes in its NAT64's prefix, and its two addresses (RFC 7050).
const NAME: &str = "ipv4only.arpa";
const KNOWN: [Ipv4Addr; 2] = [Ipv4Addr::new(192, 0, 0, 170), Ipv4Addr::new(192, 0, 0, 171)];
/// The port of DNS in the clear, over UDP and over TCP.
pub const PORT: u16 = 53;
/// The bytes of a question's number, drawn anew for each, as a question in the clear is to have one nobody can guess (RFC 5452, 9.2).
pub const ID_BYTES: usize = 2;
// The byte left nought between the prefix and the suffix (RFC 6052, bits 64 to 71).
const U_BYTE: usize = 8;
// Where each length of prefix puts the four bytes of an IPv4 address (RFC 6052, section 2.2), the longest first, as RFC 7050 looks for them.
const PLACES: [(u8, [usize; 4]); 6] = [
    (96, [12, 13, 14, 15]),
    (64, [9, 10, 11, 12]),
    (56, [7, 9, 10, 11]),
    (48, [6, 7, 9, 10]),
    (40, [5, 6, 7, 9]),
    (32, [4, 5, 6, 7]),
];

/// A NAT64's prefix: its bytes, nought past its length, and its length in bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefix {
    bytes: [u8; 16],
    length: u8,
}

impl Prefix {
    fn places(self) -> [usize; 4] {
        PLACES.iter().find(|(length, _)| *length == self.length).map_or(PLACES[0].1, |(_, places)| *places)
    }

    // The IPv6 address the NAT64 reaches `address` at.
    pub(crate) fn synthesize(self, address: Ipv4Addr) -> Ipv6Addr {
        let mut bytes = self.bytes;
        for (place, byte) in self.places().into_iter().zip(address.octets()) {
            bytes[place] = byte;
        }
        Ipv6Addr::from(bytes)
    }

    // The IPv4 address the NAT64 stands at `address` for; none for an address outside the prefix.
    pub(crate) fn extract(self, address: Ipv6Addr) -> Option<Ipv4Addr> {
        let bytes = address.octets();
        let extracted = Ipv4Addr::from(self.places().map(|place| bytes[place]));
        (self.synthesize(extracted) == address).then_some(extracted)
    }

    // The prefix of an address that carries `known` where a prefix of that length puts it, with its middle byte nought.
    fn of(address: Ipv6Addr, known: Ipv4Addr) -> Option<Prefix> {
        let bytes = address.octets();
        let (length, _) = PLACES.iter().find(|(length, places)| {
            places.iter().map(|&place| bytes[place]).eq(known.octets()) && (*length == 96 || bytes[U_BYTE] == 0)
        })?;
        let mut prefix = [0u8; 16];
        let end = usize::from(*length / 8);
        prefix[..end].copy_from_slice(&bytes[..end]);
        Some(Prefix { bytes: prefix, length: *length })
    }
}

impl core::fmt::Display for Prefix {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}/{}", Ipv6Addr::from(self.bytes), self.length)
    }
}

/// What an asker for a stay's prefix does next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // The prefix as far as this asker goes: the resolver's word, none where it told none, or none where the search the asker waited for got no answer.
    Known(Option<Prefix>),
    // Asks these resolvers; `Search::end_asking` takes the outcome.
    Ask(Vec<IpAddr>),
    // Waits for the search under way; `Search::add_waiter` takes what wakes the waiter.
    Await,
}

/// A stay's search for its NAT64's prefix, one asker at a time: the resolver's word counts for the stay, and a search that got no answer is the failure of those who waited for it, the next asker searching again; `W` wakes a waiter.
pub struct Search<W> {
    resolvers: Vec<IpAddr>,
    word: Option<Option<Prefix>>,
    turns: Turns<(), W>,
}

impl<W> Search<W> {
    /// The search of a stay in a network of IPv6 alone, whose resolvers tell its prefix.
    pub const fn new(resolvers: Vec<IpAddr>) -> Search<W> {
        Search { resolvers, word: None, turns: Turns::new() }
    }

    /// What an asker does next; `noted` keeps the count of ended searches it noted as it first looked.
    pub fn next_step(&mut self, noted: &mut Option<u64>) -> Step {
        if let Some(word) = self.word {
            return Step::Known(word);
        }
        let count = *noted.get_or_insert(self.turns.end_count());
        if self.turns.failure_since(count).is_some() {
            return Step::Known(None);
        }
        if self.turns.take() { Step::Ask(self.resolvers.clone()) } else { Step::Await }
    }

    /// Adds a waiter for the search under way, which its end hands back.
    pub fn add_waiter(&mut self, waiter: W) {
        self.turns.add_waiter(waiter);
    }

    /// Takes what the search came to: the resolver's word, or none where no answer came; returns the waiters to wake.
    pub fn end_asking(&mut self, word: Option<Option<Prefix>>) -> Vec<W> {
        let failure = match word {
            Some(word) => {
                self.word = Some(word);
                None
            }
            None => Some(()),
        };
        self.turns.end(failure)
    }
}

/// The question of that number for the AAAA records of the name whose IPv4 addresses tell the prefix.
pub fn to_question(id: [u8; ID_BYTES]) -> Vec<u8> {
    question(u16::from_be_bytes(id), NAME, TYPE_AAAA).unwrap_or_default()
}

/// What an answer to the question of that number tells: the prefix, or none where the resolver knows of no NAT64; none for what is no answer to it, or one where the resolver could not say.
pub fn parse_prefix(message: &[u8], id: [u8; ID_BYTES]) -> Option<Option<Prefix>> {
    let answer = parse_answer(message).filter(|answer| answer.id == u16::from_be_bytes(id) && !answer.is_failed)?;
    Some(answer.addresses.iter().find_map(|address| match address {
        IpAddr::V6(address) => KNOWN.iter().find_map(|&known| Prefix::of(*address, known)),
        IpAddr::V4(_) => None,
    }))
}

/// How a stay reaches the addresses it is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    // A network of IPv4: every address as it is.
    Direct,
    // A network of IPv6 alone: an IPv4 address through the NAT64 of the prefix found, or not at all where none was.
    Ipv6Alone(Option<Prefix>),
}

/// The addresses to try: all of them as they are in a network of IPv4; in a network of IPv6 alone each IPv4 one at its place in the order as the NAT64 of the prefix found reaches it, or none where no prefix was found, and each address once.
pub fn to_reachable(addresses: &[SocketAddr], reach: Reach) -> Vec<SocketAddr> {
    let Reach::Ipv6Alone(prefix) = reach else {
        return addresses.to_vec();
    };
    let mut reachable: Vec<SocketAddr> = Vec::with_capacity(addresses.len());
    for address in addresses {
        let reached = match (address.ip(), prefix) {
            (IpAddr::V4(ipv4), Some(prefix)) => SocketAddr::new(IpAddr::V6(prefix.synthesize(ipv4)), address.port()),
            (IpAddr::V4(_), None) => continue,
            (IpAddr::V6(_), _) => *address,
        };
        if !reachable.contains(&reached) {
            reachable.push(reached);
        }
    }
    reachable
}

/// The log line of a resolver's word on the prefix, with the time it took.
pub fn to_told_line(word: Option<Prefix>, resolver: &str, ms: u128) -> String {
    match word {
        Some(prefix) => format!("names: the network's NAT64 is {prefix}, as {resolver} told it in {ms} ms"),
        None => {
            format!("names: the network has no NAT64, as {resolver} told in {ms} ms, so IPv4 addresses are passed over")
        }
    }
}

/// The log line of a search for the prefix that got no answer.
pub fn to_unfound_line(reason: &str) -> String {
    format!("names: the network's NAT64 is not known, so IPv4 addresses are passed over: {reason}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::message::samples::reply_message;
    use super::*;
    use alloc::vec;

    const ID: [u8; ID_BYTES] = [0x9e, 0x37];

    pub(crate) fn prefix(text: &str, length: u8) -> Prefix {
        Prefix { bytes: text.parse::<Ipv6Addr>().unwrap().octets(), length }
    }

    #[test]
    fn synthesized_as_rfc_6052_shows() {
        let address = Ipv4Addr::new(192, 0, 2, 33);
        let table = [
            ("2001:db8::", 32, "2001:db8:c000:221::"),
            ("2001:db8:100::", 40, "2001:db8:1c0:2:21::"),
            ("2001:db8:122::", 48, "2001:db8:122:c000:2:2100::"),
            ("2001:db8:122:300::", 56, "2001:db8:122:3c0:0:221::"),
            ("2001:db8:122:344::", 64, "2001:db8:122:344:c0:2:2100::"),
            ("2001:db8:122:344::", 96, "2001:db8:122:344::c000:221"),
            ("64:ff9b::", 96, "64:ff9b::c000:221"),
        ];
        for (network, length, synthesized) in table {
            let prefix = prefix(network, length);
            let expected: Ipv6Addr = synthesized.parse().unwrap();
            assert_eq!(prefix.synthesize(address), expected, "{network}/{length}");
            assert_eq!(Prefix::of(expected, address), Some(prefix), "and back, {network}/{length}");
            assert_eq!(prefix.extract(expected), Some(address), "the address read out again, {network}/{length}");
            let outside: Ipv6Addr = "2001:db9::c000:221".parse().unwrap();
            assert_eq!(prefix.extract(outside), None, "none of an address outside {network}/{length}");
        }
        assert_eq!(format!("{}", prefix("64:ff9b::", 96)), "64:ff9b::/96");
    }

    #[test]
    fn prefix_from_answer() {
        let id = u16::from_be_bytes(ID);
        let well_known = |last: u8| [0, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 0, last];
        let (first, second) = (well_known(170), well_known(171));
        let answer = reply_message(id, 0x8180, &[(TYPE_AAAA, 60, &first), (TYPE_AAAA, 60, &second)]);
        assert_eq!(parse_prefix(&answer, ID), Some(Some(prefix("64:ff9b::", 96))));
        let longer = "2001:db8:122:344:c0:0:aa00:0".parse::<Ipv6Addr>().unwrap().octets();
        assert_eq!(
            parse_prefix(&reply_message(id, 0x8180, &[(TYPE_AAAA, 60, &longer)]), ID),
            Some(Some(prefix("2001:db8:122:344::", 64))),
            "a prefix of 64 bits, the IPv4 address past the byte that is nought"
        );
        let plain = "2001:db8::1".parse::<Ipv6Addr>().unwrap().octets();
        assert_eq!(
            parse_prefix(&reply_message(id, 0x8180, &[(TYPE_AAAA, 60, &plain)]), ID),
            Some(None),
            "an address without the known one: the resolver tells of no NAT64"
        );
        assert_eq!(
            parse_prefix(&reply_message(id, 0x8183, &[]), ID),
            Some(None),
            "nor does one that knows the name not"
        );
        assert_eq!(parse_prefix(&reply_message(id, 0x8182, &[]), ID), None, "one that could not say tells nothing");
        assert_eq!(
            parse_prefix(&reply_message(id + 1, 0x8180, &[(TYPE_AAAA, 60, &first)]), ID),
            None,
            "nor is another question's an answer"
        );
        let question = to_question(ID);
        assert_eq!(
            (&question[..2], &question[12..29]),
            (&ID[..], &b"\x08ipv4only\x04arpa\x00\x00\x1C"[..]),
            "AAAA records of ipv4only.arpa, under the number drawn for the question"
        );
    }

    #[test]
    fn search_until_told() {
        let resolvers = vec!["2001:db8::53".parse().unwrap()];
        let mut search: Search<char> = Search::new(resolvers.clone());
        let (mut first, mut second) = (None, None);
        assert_eq!(search.next_step(&mut first), Step::Ask(resolvers.clone()), "the first asker asks");
        assert_eq!(search.next_step(&mut second), Step::Await, "the next waits for it");
        search.add_waiter('w');
        assert_eq!(search.end_asking(None), ['w'], "no answer came, and whoever waited is woken");
        assert_eq!(search.next_step(&mut second), Step::Known(None), "to pass IPv4 over this time");
        assert_eq!(search.next_step(&mut None), Step::Ask(resolvers), "the next asker searches again");
        assert!(search.end_asking(Some(None)).is_empty());
        assert_eq!(search.next_step(&mut None), Step::Known(None), "the resolver's word that there is none counts");
        let mut search: Search<char> = Search::new(Vec::new());
        search.next_step(&mut None);
        search.end_asking(Some(Some(prefix("64:ff9b::", 96))));
        assert_eq!(search.next_step(&mut None), Step::Known(Some(prefix("64:ff9b::", 96))), "for the rest of the stay");
    }

    #[test]
    fn reachable_addresses() {
        let at = |text: &str| -> SocketAddr { text.parse().unwrap() };
        let addresses = [at("[2001:db8::1]:443"), at("192.0.2.33:443"), at("192.0.2.34:443"), at("[2001:db8::2]:443")];
        let nat64 = prefix("64:ff9b::", 96);
        assert_eq!(
            to_reachable(&addresses, Reach::Direct),
            addresses,
            "a network of IPv4 reaches them all as they are"
        );
        assert_eq!(
            to_reachable(&addresses, Reach::Ipv6Alone(Some(nat64))),
            [
                at("[2001:db8::1]:443"),
                at("[64:ff9b::c000:221]:443"),
                at("[64:ff9b::c000:222]:443"),
                at("[2001:db8::2]:443")
            ],
            "each IPv4 address where it stood, through the NAT64"
        );
        assert_eq!(
            to_reachable(&addresses, Reach::Ipv6Alone(None)),
            [at("[2001:db8::1]:443"), at("[2001:db8::2]:443")],
            "without a NAT64 no IPv4 address is reached"
        );
        assert_eq!(
            to_reachable(&[at("[64:ff9b::c000:221]:443"), at("192.0.2.33:443")], Reach::Ipv6Alone(Some(nat64))),
            [at("[64:ff9b::c000:221]:443")],
            "once each"
        );
        assert_eq!(
            to_told_line(Some(nat64), "2001:db8::53", 41),
            "names: the network's NAT64 is 64:ff9b::/96, as 2001:db8::53 told it in 41 ms"
        );
        assert_eq!(
            to_told_line(None, "2001:db8::53", 41),
            "names: the network has no NAT64, as 2001:db8::53 told in 41 ms, so IPv4 addresses are passed over"
        );
        assert_eq!(
            to_unfound_line("the other side is silent"),
            "names: the network's NAT64 is not known, so IPv4 addresses are passed over: the other side is silent"
        );
    }
}
