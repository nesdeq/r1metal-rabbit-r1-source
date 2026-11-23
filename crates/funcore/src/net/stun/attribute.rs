use super::message::{MAGIC, Transaction};
use crate::base::bytes::be16;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

// The attributes' types (RFC 8489, 18.3; RFC 8656, 18; RFC 8445, 16.1).
pub(super) const MAPPED_ADDRESS: u16 = 0x0001;
pub(super) const USERNAME: u16 = 0x0006;
pub(crate) const MESSAGE_INTEGRITY: u16 = 0x0008;
pub(super) const ERROR_CODE: u16 = 0x0009;
pub(super) const UNKNOWN_ATTRIBUTES: u16 = 0x000A;
pub(super) const CHANNEL_NUMBER: u16 = 0x000C;
pub(super) const LIFETIME: u16 = 0x000D;
pub(super) const XOR_PEER_ADDRESS: u16 = 0x0012;
pub(super) const DATA: u16 = 0x0013;
pub(super) const REALM: u16 = 0x0014;
pub(super) const NONCE: u16 = 0x0015;
pub(super) const XOR_RELAYED_ADDRESS: u16 = 0x0016;
pub(crate) const REQUESTED_ADDRESS_FAMILY: u16 = 0x0017;
const EVEN_PORT: u16 = 0x0018;
pub(crate) const REQUESTED_TRANSPORT: u16 = 0x0019;
pub(crate) const DONT_FRAGMENT: u16 = 0x001A;
pub(super) const MESSAGE_INTEGRITY_SHA256: u16 = 0x001C;
pub(crate) const PASSWORD_ALGORITHM: u16 = 0x001D;
pub(crate) const USERHASH: u16 = 0x001E;
pub(super) const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const RESERVATION_TOKEN: u16 = 0x0022;
pub(super) const PRIORITY: u16 = 0x0024;
pub(super) const USE_CANDIDATE: u16 = 0x0025;
pub(super) const PASSWORD_ALGORITHMS: u16 = 0x8002;
pub(crate) const ALTERNATE_DOMAIN: u16 = 0x8003;
#[cfg(test)]
pub(super) const SOFTWARE: u16 = 0x8022;
pub(super) const ALTERNATE_SERVER: u16 = 0x8023;
pub(crate) const FINGERPRINT: u16 = 0x8028;
pub(super) const ICE_CONTROLLED: u16 = 0x8029;
pub(super) const ICE_CONTROLLING: u16 = 0x802A;
// libwebrtc's renomination: a number that grows with each nomination (`STUN_ATTR_NOMINATION`).
pub(super) const NOMINATION: u16 = 0xC001;
// Below it an attribute is one a receiver must understand (RFC 8489, 14).
pub(super) const OPTIONAL: u16 = 0x8000;
// The comprehension-required attributes this code knows, whether it reads their values or passes them over.
pub(super) const KNOWN: [u16; 23] = [
    MAPPED_ADDRESS,
    USERNAME,
    MESSAGE_INTEGRITY,
    ERROR_CODE,
    UNKNOWN_ATTRIBUTES,
    CHANNEL_NUMBER,
    LIFETIME,
    XOR_PEER_ADDRESS,
    DATA,
    REALM,
    NONCE,
    XOR_RELAYED_ADDRESS,
    REQUESTED_ADDRESS_FAMILY,
    EVEN_PORT,
    REQUESTED_TRANSPORT,
    DONT_FRAGMENT,
    MESSAGE_INTEGRITY_SHA256,
    PASSWORD_ALGORITHM,
    USERHASH,
    XOR_MAPPED_ADDRESS,
    RESERVATION_TOKEN,
    PRIORITY,
    USE_CANDIDATE,
];

// The error codes (RFC 8489, 14.8; RFC 8656, 19; RFC 8445, 16.2).
pub(crate) const TRY_ALTERNATE: u16 = 300;
pub(crate) const BAD_REQUEST: u16 = 400;
pub(crate) const UNAUTHENTICATED: u16 = 401;
#[cfg(test)]
pub(crate) const FORBIDDEN: u16 = 403;
pub(crate) const UNKNOWN_ATTRIBUTE: u16 = 420;
pub(crate) const ALLOCATION_MISMATCH: u16 = 437;
pub(crate) const STALE_NONCE: u16 = 438;
pub(crate) const FAMILY_UNSUPPORTED: u16 = 440;
#[cfg(test)]
pub(crate) const WRONG_CREDENTIALS: u16 = 441;
#[cfg(test)]
pub(crate) const TRANSPORT_UNSUPPORTED: u16 = 442;
#[cfg(test)]
pub(crate) const PEER_FAMILY_MISMATCH: u16 = 443;
#[cfg(test)]
pub(crate) const QUOTA_REACHED: u16 = 486;
pub(crate) const ROLE_CONFLICT: u16 = 487;
pub(crate) const SERVER_ERROR: u16 = 500;
pub(crate) const INSUFFICIENT_CAPACITY: u16 = 508;
const LEAST_CODE: u16 = 300;
const MOST_CODE: u16 = 699;
const HUNDRED: u16 = 100;

// An address family's number (RFC 8489, 14.1; RFC 8656, 18.6).
pub(crate) const IPV4: u8 = 0x01;
pub(crate) const IPV6: u8 = 0x02;
const IPV4_VALUE: usize = 8;
const IPV6_VALUE: usize = 20;
// REQUESTED-TRANSPORT's protocol number for UDP (RFC 8656, 18.8).
pub(crate) const UDP: u8 = 17;
// The bytes attributes and ChannelData on a stream are padded to whole ones of (RFC 8489, 14; RFC 8656, 12.5).
pub(crate) const WORD: usize = 4;

// An attribute as a message is written with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Attribute<'a> {
    Username(&'a [u8]),
    Userhash(&'a [u8]),
    XorMapped(SocketAddr),
    Priority(u32),
    UseCandidate,
    Controlling(u64),
    Controlled(u64),
    Nomination(u32),
    Error(u16),
    Unknown(&'a [u16]),
    Realm(&'a [u8]),
    Nonce(&'a [u8]),
    // PASSWORD-ALGORITHMS as the server listed them, copied whole.
    Algorithms(&'a [u8]),
    // PASSWORD-ALGORITHM without parameters.
    Algorithm(u16),
    Lifetime(u32),
    Peer(SocketAddr),
    Data(&'a [u8]),
    Channel(u16),
    RequestedTransport(u8),
    RequestedFamily(u8),
    DontFragment,
    #[cfg(test)]
    Relayed(SocketAddr),
    #[cfg(test)]
    Mapped(SocketAddr),
    #[cfg(test)]
    Alternate(SocketAddr),
    #[cfg(test)]
    Raw(u16, &'a [u8]),
}

// The reason phrase RFC 8489 and RFC 8445 give a code that this side sends.
const fn reason(code: u16) -> &'static str {
    match code {
        BAD_REQUEST => "Bad Request",
        UNAUTHENTICATED => "Unauthenticated",
        UNKNOWN_ATTRIBUTE => "Unknown Attribute",
        ROLE_CONFLICT => "Role Conflict",
        _ => "",
    }
}

// The family's number of an address.
pub(crate) const fn family_of(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => IPV4,
        IpAddr::V6(_) => IPV6,
    }
}

// The bytes an address is masked with in an XOR attribute: the magic cookie, and the transaction for IPv6.
fn mask(transaction: &Transaction) -> [u8; 16] {
    let mut mask = [0u8; 16];
    mask[..WORD].copy_from_slice(&MAGIC);
    mask[WORD..].copy_from_slice(transaction);
    mask
}

// The value of an address attribute, masked under `transaction` where it is one of the XOR kind.
fn write_address(address: SocketAddr, transaction: Option<&Transaction>, value: &mut Vec<u8>) {
    let mask = transaction.map_or([0; 16], mask);
    value.extend([0, family_of(address.ip())]);
    value.extend((address.port() ^ u16::from_be_bytes([mask[0], mask[1]])).to_be_bytes());
    match address.ip() {
        IpAddr::V4(ip) => value.extend(ip.octets().iter().zip(mask).map(|(byte, mask)| byte ^ mask)),
        IpAddr::V6(ip) => value.extend(ip.octets().iter().zip(mask).map(|(byte, mask)| byte ^ mask)),
    }
}

// An address attribute's value, unmasked under `transaction` where it is one of the XOR kind.
pub(super) fn read_address(value: &[u8], transaction: Option<&Transaction>) -> Option<SocketAddr> {
    let mask = transaction.map_or([0; 16], mask);
    let port = be16(value, 2)? ^ u16::from_be_bytes([mask[0], mask[1]]);
    let unmask = |at: usize, byte: &u8| byte ^ mask[at];
    let ip = match (*value.get(1)?, value.len()) {
        (IPV4, IPV4_VALUE) => {
            let octets: [u8; 4] = core::array::from_fn(|at| unmask(at, &value[WORD + at]));
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        (IPV6, IPV6_VALUE) => {
            let octets: [u8; 16] = core::array::from_fn(|at| unmask(at, &value[WORD + at]));
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

// ERROR-CODE's value: the class, the number and the reason phrase.
fn write_error(code: u16, value: &mut Vec<u8>) {
    value.extend([0, 0, (code / HUNDRED) as u8, (code % HUNDRED) as u8]);
    value.extend(reason(code).as_bytes());
}

// The code ERROR-CODE holds; none for one outside 300 to 699.
pub(super) fn read_error(value: &[u8]) -> Option<u16> {
    let (class, number) = (u16::from(*value.get(2)? & 0x07), u16::from(*value.get(3)?));
    let code = class * HUNDRED + number;
    (number < HUNDRED && (LEAST_CODE..=MOST_CODE).contains(&code)).then_some(code)
}

impl Attribute<'_> {
    // The attribute's type, and its value under the message's transaction.
    pub(super) fn encode(&self, transaction: &Transaction) -> (u16, Vec<u8>) {
        let mut value = Vec::new();
        let kind = match *self {
            Attribute::Username(name) => {
                value.extend(name);
                USERNAME
            }
            Attribute::Userhash(hash) => {
                value.extend(hash);
                USERHASH
            }
            Attribute::XorMapped(address) => {
                write_address(address, Some(transaction), &mut value);
                XOR_MAPPED_ADDRESS
            }
            Attribute::Priority(priority) => {
                value.extend(priority.to_be_bytes());
                PRIORITY
            }
            Attribute::UseCandidate => USE_CANDIDATE,
            Attribute::Controlling(tiebreaker) => {
                value.extend(tiebreaker.to_be_bytes());
                ICE_CONTROLLING
            }
            Attribute::Controlled(tiebreaker) => {
                value.extend(tiebreaker.to_be_bytes());
                ICE_CONTROLLED
            }
            Attribute::Nomination(count) => {
                value.extend(count.to_be_bytes());
                NOMINATION
            }
            Attribute::Error(code) => {
                write_error(code, &mut value);
                ERROR_CODE
            }
            Attribute::Unknown(kinds) => {
                value.extend(kinds.iter().flat_map(|kind| kind.to_be_bytes()));
                UNKNOWN_ATTRIBUTES
            }
            Attribute::Realm(realm) => {
                value.extend(realm);
                REALM
            }
            Attribute::Nonce(nonce) => {
                value.extend(nonce);
                NONCE
            }
            Attribute::Algorithms(list) => {
                value.extend(list);
                PASSWORD_ALGORITHMS
            }
            Attribute::Algorithm(number) => {
                value.extend(number.to_be_bytes());
                value.extend([0, 0]);
                PASSWORD_ALGORITHM
            }
            Attribute::Lifetime(seconds) => {
                value.extend(seconds.to_be_bytes());
                LIFETIME
            }
            Attribute::Peer(address) => {
                write_address(address, Some(transaction), &mut value);
                XOR_PEER_ADDRESS
            }
            Attribute::Data(data) => {
                value.extend(data);
                DATA
            }
            Attribute::Channel(number) => {
                value.extend(number.to_be_bytes());
                value.extend([0, 0]);
                CHANNEL_NUMBER
            }
            Attribute::RequestedTransport(protocol) => {
                value.extend([protocol, 0, 0, 0]);
                REQUESTED_TRANSPORT
            }
            Attribute::RequestedFamily(family) => {
                value.extend([family, 0, 0, 0]);
                REQUESTED_ADDRESS_FAMILY
            }
            Attribute::DontFragment => DONT_FRAGMENT,
            #[cfg(test)]
            Attribute::Relayed(address) => {
                write_address(address, Some(transaction), &mut value);
                XOR_RELAYED_ADDRESS
            }
            #[cfg(test)]
            Attribute::Mapped(address) => {
                write_address(address, None, &mut value);
                MAPPED_ADDRESS
            }
            #[cfg(test)]
            Attribute::Alternate(address) => {
                write_address(address, None, &mut value);
                ALTERNATE_SERVER
            }
            #[cfg(test)]
            Attribute::Raw(kind, raw) => {
                value.extend(raw);
                kind
            }
        };
        (kind, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_masked() {
        let transaction = [7u8; 12];
        let v4: SocketAddr = "192.0.2.1:32853".parse().unwrap();
        let v6: SocketAddr = "[2001:db8:1234:5678:11:2233:4455:6677]:32853".parse().unwrap();
        let mut value = Vec::new();
        write_address(v4, Some(&transaction), &mut value);
        assert_eq!(value, [0, 1, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43], "RFC 5769, 2.2: port and address masked");
        assert_eq!(read_address(&value, Some(&transaction)), Some(v4));
        for address in [v4, v6] {
            for xor in [None, Some(&transaction)] {
                let mut value = Vec::new();
                write_address(address, xor, &mut value);
                assert_eq!(read_address(&value, xor), Some(address), "{address} reads back");
            }
        }
        assert_eq!(read_address(&value[..7], None), None, "too short");
        assert_eq!(read_address(&[0, 3, 0, 0, 1, 2, 3, 4], None), None, "a family of none");
        assert_eq!(read_address(&[0, 2, 0, 0, 1, 2, 3, 4], None), None, "IPv6 of four bytes");
    }

    #[test]
    fn error_codes() {
        let mut value = Vec::new();
        write_error(ROLE_CONFLICT, &mut value);
        assert_eq!(&value[..4], [0, 0, 4, 87], "the class and the number apart");
        assert_eq!(&value[4..], b"Role Conflict");
        assert_eq!(read_error(&value), Some(ROLE_CONFLICT));
        assert_eq!(read_error(&[0, 0, 0xFC, 1]), Some(401), "the bits above the class are passed over");
        assert_eq!(
            [read_error(&[0, 0, 2, 0]), read_error(&[0, 0, 7, 0]), read_error(&[0, 0, 4, 100]), read_error(&[0, 0, 4])],
            [None, None, None, None],
            "no class of 2 or 7, no number of 100, no value of three bytes"
        );
    }

    #[test]
    fn attribute_values() {
        let transaction = [0u8; 12];
        assert_eq!(Attribute::Channel(0x4001).encode(&transaction), (CHANNEL_NUMBER, alloc::vec![0x40, 1, 0, 0]));
        assert_eq!(
            Attribute::RequestedTransport(UDP).encode(&transaction),
            (REQUESTED_TRANSPORT, alloc::vec![17, 0, 0, 0])
        );
        assert_eq!(
            Attribute::RequestedFamily(IPV6).encode(&transaction),
            (REQUESTED_ADDRESS_FAMILY, alloc::vec![2, 0, 0, 0])
        );
        assert_eq!(Attribute::Algorithm(1).encode(&transaction), (PASSWORD_ALGORITHM, alloc::vec![0, 1, 0, 0]));
        assert_eq!(
            Attribute::Unknown(&[0x0031, 0x7FFF]).encode(&transaction),
            (UNKNOWN_ATTRIBUTES, alloc::vec![0, 0x31, 0x7F, 0xFF])
        );
        assert_eq!(Attribute::UseCandidate.encode(&transaction), (USE_CANDIDATE, alloc::vec![]));
        assert_eq!(Attribute::DontFragment.encode(&transaction), (DONT_FRAGMENT, alloc::vec![]));
        assert!(KNOWN.iter().all(|kind| *kind < OPTIONAL), "the list holds required attributes alone");
    }
}
