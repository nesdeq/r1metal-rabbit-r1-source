use super::Crypto;
use super::attribute::{
    ALTERNATE_DOMAIN, ALTERNATE_SERVER, Attribute, CHANNEL_NUMBER, DATA, ERROR_CODE, FINGERPRINT, ICE_CONTROLLED,
    ICE_CONTROLLING, KNOWN, LIFETIME, MAPPED_ADDRESS, MESSAGE_INTEGRITY, MESSAGE_INTEGRITY_SHA256, NOMINATION, NONCE,
    OPTIONAL, PASSWORD_ALGORITHMS, PRIORITY, REALM, UNKNOWN_ATTRIBUTES, USE_CANDIDATE, USERNAME, WORD,
    XOR_MAPPED_ADDRESS, XOR_PEER_ADDRESS, XOR_RELAYED_ADDRESS, read_address, read_error,
};
use crate::base::bytes::{be16, be32, same};
use crate::base::checksum::crc32;
use crate::crypto::SHA256_BYTES;
use alloc::vec::Vec;
use core::net::SocketAddr;
use core::time::Duration;

pub(super) const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
pub(super) const HEAD: usize = 20;
pub(crate) type Transaction = [u8; 12];
const TRANSACTION_AT: usize = 8;
const LENGTH_AT: usize = 2;
const ATTRIBUTE_HEAD: usize = 4;
// The two bits every STUN message begins with are nought (RFC 8489, 5).
const TYPE_BITS: u16 = 0xC000;
const FINGERPRINT_XOR: u32 = 0x5354_554E;
// MESSAGE-INTEGRITY-SHA256 holds 16 to 32 bytes of the code, whole words (RFC 8489, 14.6); this side writes all 32.
const SHA256_LEAST: usize = 16;
// STUN's retransmissions over UDP: the least first wait, the requests sent at the most, and the wait after the last in first waits (RFC 8489, 6.2.1).
pub(crate) const RTO: Duration = Duration::from_millis(500);
pub(crate) const TRANSMISSIONS: u32 = 7;
const LAST_WAIT: u32 = 16;
// How long a transaction over a stream waits for its answer (RFC 8489, 6.2.2).
pub(crate) const STREAM_WAIT: Duration = Duration::from_millis(39_500);

// The methods (RFC 8489, 18.2; RFC 8656, 17).
pub(crate) const BINDING: u16 = 0x001;
pub(crate) const ALLOCATE: u16 = 0x003;
pub(crate) const REFRESH: u16 = 0x004;
pub(crate) const SEND: u16 = 0x006;
pub(crate) const DATA_METHOD: u16 = 0x007;
pub(crate) const CREATE_PERMISSION: u16 = 0x008;
pub(crate) const CHANNEL_BIND: u16 = 0x009;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    Request,
    Indication,
    Success,
    Error,
}

// The key a message's integrity is computed under, by its hash.
#[derive(Clone, Copy)]
pub(crate) enum Integrity<'a> {
    Sha1(&'a [u8]),
    Sha256(&'a [u8]),
}

// The message type's bits: the class's two between the method's (RFC 8489, 5).
const fn type_of(method: u16, class: Class) -> u16 {
    let class = match class {
        Class::Request => 0b00,
        Class::Indication => 0b01,
        Class::Success => 0b10,
        Class::Error => 0b11,
    };
    (method & 0x0F80) << 2 | (method & 0x0070) << 1 | (method & 0x000F) | (class & 0b10) << 7 | (class & 0b01) << 4
}

const fn split_type(kind: u16) -> (u16, Class) {
    let method = (kind & 0x3E00) >> 2 | (kind & 0x00E0) >> 1 | (kind & 0x000F);
    let class = match (kind >> 7 & 0b10) | (kind >> 4 & 0b01) {
        0b00 => Class::Request,
        0b01 => Class::Indication,
        0b10 => Class::Success,
        _ => Class::Error,
    };
    (method, class)
}

// A transaction's number nobody could guess (RFC 8489, 5); none where the library gives no random bytes.
pub(crate) fn new_transaction(crypto: &impl Crypto) -> Option<Transaction> {
    let mut transaction = [0u8; 12];
    crypto.random(&mut transaction).then_some(transaction)
}

// When a transaction over UDP whose first wait is `rto` is due again after `sent` requests, from its first: its next request, or after the last its end (RFC 8489, 6.2.1).
pub(crate) fn resend_at(rto: Duration, sent: u32) -> Duration {
    if sent < TRANSMISSIONS {
        rto * ((1 << sent) - 1)
    } else {
        rto * ((1 << (TRANSMISSIONS - 1)) - 1) + rto * LAST_WAIT
    }
}

// True for what demultiplexes as STUN by its first byte (RFC 7983, 7) and holds the magic cookie.
pub(crate) fn is_message(bytes: &[u8]) -> bool {
    bytes.first().is_some_and(|first| *first < 4) && bytes.get(4..8) == Some(&MAGIC[..])
}

// A message read whole, its attributes believed by its reader only after `verify` where it carries integrity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Message<'a> {
    bytes: &'a [u8],
    pub(crate) method: u16,
    pub(crate) class: Class,
    pub(crate) transaction: Transaction,
    pub(crate) username: Option<&'a [u8]>,
    // XOR-MAPPED-ADDRESS, or MAPPED-ADDRESS where that alone is there.
    pub(crate) mapped: Option<SocketAddr>,
    pub(crate) priority: Option<u32>,
    pub(crate) use_candidate: bool,
    pub(crate) controlling: Option<u64>,
    pub(crate) controlled: Option<u64>,
    pub(crate) nomination: Option<u32>,
    pub(crate) error: Option<u16>,
    pub(crate) realm: Option<&'a [u8]>,
    pub(crate) nonce: Option<&'a [u8]>,
    // UNKNOWN-ATTRIBUTES: what the far side did not understand.
    pub(crate) unknown: Vec<u16>,
    // The comprehension-required attributes of this message that this code does not know.
    pub(crate) required: Vec<u16>,
    pub(crate) lifetime: Option<u32>,
    pub(crate) peers: Vec<SocketAddr>,
    pub(crate) data: Option<&'a [u8]>,
    pub(crate) relayed: Option<SocketAddr>,
    pub(crate) channel: Option<u16>,
    pub(crate) alternate: Option<SocketAddr>,
    pub(crate) domain: Option<&'a [u8]>,
    pub(crate) algorithms: Option<&'a [u8]>,
    pub(crate) has_fingerprint: bool,
    sha1_at: Option<usize>,
    sha256_at: Option<(usize, usize)>,
}

impl<'a> Message<'a> {
    // Reads a message: its head, every attribute within its length, and its fingerprint where it has one; none for what is no STUN message or carries a wrong fingerprint or a malformed attribute.
    pub(crate) fn parse(bytes: &'a [u8]) -> Option<Message<'a>> {
        let kind = be16(bytes, 0)?;
        let length = usize::from(be16(bytes, LENGTH_AT)?);
        if kind & TYPE_BITS != 0 || bytes.get(4..8)? != MAGIC || length % WORD != 0 || HEAD + length != bytes.len() {
            return None;
        }
        let (method, class) = split_type(kind);
        let transaction: Transaction = bytes[TRANSACTION_AT..HEAD].try_into().ok()?;
        let mut message = Message {
            bytes,
            method,
            class,
            transaction,
            username: None,
            mapped: None,
            priority: None,
            use_candidate: false,
            controlling: None,
            controlled: None,
            nomination: None,
            error: None,
            realm: None,
            nonce: None,
            unknown: Vec::new(),
            required: Vec::new(),
            lifetime: None,
            peers: Vec::new(),
            data: None,
            relayed: None,
            channel: None,
            alternate: None,
            domain: None,
            algorithms: None,
            has_fingerprint: false,
            sha1_at: None,
            sha256_at: None,
        };
        let mut is_xor_mapped = false;
        let mut at = HEAD;
        while at < bytes.len() {
            let (code, size) = (be16(bytes, at)?, usize::from(be16(bytes, at + 2)?));
            let value = bytes.get(at + ATTRIBUTE_HEAD..at + ATTRIBUTE_HEAD + size)?;
            let next = at + ATTRIBUTE_HEAD + size.next_multiple_of(WORD);
            if next > bytes.len() || message.has_fingerprint {
                return None;
            }
            let is_sealed = message.sha1_at.is_some() || message.sha256_at.is_some();
            match code {
                FINGERPRINT => {
                    let sum = crc32(&bytes[..at]) ^ FINGERPRINT_XOR;
                    if value != sum.to_be_bytes() {
                        return None;
                    }
                    message.has_fingerprint = true;
                }
                MESSAGE_INTEGRITY_SHA256 if message.sha256_at.is_none() => {
                    if !(SHA256_LEAST..=SHA256_BYTES).contains(&size) || size % WORD != 0 {
                        return None;
                    }
                    message.sha256_at = Some((at, size));
                }
                MESSAGE_INTEGRITY if !is_sealed => {
                    if size != super::SHA1_BYTES {
                        return None;
                    }
                    message.sha1_at = Some(at);
                }
                // What follows the integrity is passed over (RFC 8489, 14.5).
                _ if is_sealed => {}
                _ => message.read(code, value, &mut is_xor_mapped)?,
            }
            at = next;
        }
        Some(message)
    }

    // Takes one attribute before the integrity; none for a known one that is malformed.
    fn read(&mut self, code: u16, value: &'a [u8], is_xor_mapped: &mut bool) -> Option<()> {
        let word = || be32(value, 0).filter(|_| value.len() == WORD);
        let long = || Some(u64::from_be_bytes(value.try_into().ok()?));
        let xor = Some(&self.transaction);
        match code {
            USERNAME => _ = self.username.get_or_insert(value),
            XOR_MAPPED_ADDRESS if !*is_xor_mapped => {
                self.mapped = Some(read_address(value, xor)?);
                *is_xor_mapped = true;
            }
            MAPPED_ADDRESS if self.mapped.is_none() => self.mapped = Some(read_address(value, None)?),
            PRIORITY => _ = self.priority.get_or_insert(word()?),
            USE_CANDIDATE => self.use_candidate = true,
            ICE_CONTROLLING => _ = self.controlling.get_or_insert(long()?),
            ICE_CONTROLLED => _ = self.controlled.get_or_insert(long()?),
            NOMINATION => _ = self.nomination.get_or_insert(word()?),
            ERROR_CODE => _ = self.error.get_or_insert(read_error(value)?),
            REALM => _ = self.realm.get_or_insert(value),
            NONCE => _ = self.nonce.get_or_insert(value),
            UNKNOWN_ATTRIBUTES => {
                let (kinds, rest) = value.as_chunks::<2>();
                if !rest.is_empty() {
                    return None;
                }
                self.unknown.extend(kinds.iter().map(|kind| u16::from_be_bytes(*kind)));
            }
            LIFETIME => _ = self.lifetime.get_or_insert(word()?),
            XOR_PEER_ADDRESS => self.peers.push(read_address(value, xor)?),
            DATA => _ = self.data.get_or_insert(value),
            XOR_RELAYED_ADDRESS => _ = self.relayed.get_or_insert(read_address(value, xor)?),
            CHANNEL_NUMBER => _ = self.channel.get_or_insert(word().map(|word| (word >> 16) as u16)?),
            ALTERNATE_SERVER => _ = self.alternate.get_or_insert(read_address(value, None)?),
            ALTERNATE_DOMAIN => _ = self.domain.get_or_insert(value),
            PASSWORD_ALGORITHMS => _ = self.algorithms.get_or_insert(value),
            code if code < OPTIONAL && !KNOWN.contains(&code) => self.required.push(code),
            _ => {}
        }
        Some(())
    }

    // True where the message carries the integrity of that kind and it holds under the key (RFC 8489, 14.5, 14.6).
    pub(crate) fn verify(&self, crypto: &impl Crypto, integrity: Integrity<'_>) -> bool {
        let head = |at: usize, size: usize| ((at + ATTRIBUTE_HEAD + size - HEAD) as u16).to_be_bytes();
        match integrity {
            Integrity::Sha1(key) => self.sha1_at.is_some_and(|at| {
                let length = head(at, super::SHA1_BYTES);
                let code = crypto.hmac_sha1(key, &[&self.bytes[..LENGTH_AT], &length, &self.bytes[WORD..at]]);
                same(&code, &self.bytes[at + ATTRIBUTE_HEAD..at + ATTRIBUTE_HEAD + super::SHA1_BYTES])
            }),
            Integrity::Sha256(key) => self.sha256_at.is_some_and(|(at, size)| {
                let length = head(at, size);
                let code = crypto.hmac_sha256(key, &[&self.bytes[..LENGTH_AT], &length, &self.bytes[WORD..at]]);
                same(&code[..size], &self.bytes[at + ATTRIBUTE_HEAD..at + ATTRIBUTE_HEAD + size])
            }),
        }
    }

    // True where the message carries an integrity of either kind.
    pub(crate) const fn is_sealed(&self) -> bool {
        self.sha1_at.is_some() || self.sha256_at.is_some()
    }

    // The first value of an attribute of that type, for what this code reads of it nowhere else.
    #[cfg(test)]
    pub(crate) fn find(&self, kind: u16) -> Option<&'a [u8]> {
        let mut at = HEAD;
        while at < self.bytes.len() {
            let (code, size) = (be16(self.bytes, at)?, usize::from(be16(self.bytes, at + 2)?));
            if code == kind {
                return self.bytes.get(at + ATTRIBUTE_HEAD..at + ATTRIBUTE_HEAD + size);
            }
            at += ATTRIBUTE_HEAD + size.next_multiple_of(WORD);
        }
        None
    }
}

// A message being written.
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new(method: u16, class: Class, transaction: &Transaction) -> Writer {
        let mut bytes = Vec::with_capacity(crate::net::MTU);
        bytes.extend(type_of(method, class).to_be_bytes());
        bytes.extend([0, 0]);
        bytes.extend(MAGIC);
        bytes.extend(transaction);
        Writer { bytes }
    }

    fn transaction(&self) -> Transaction {
        let mut transaction = [0u8; 12];
        transaction.copy_from_slice(&self.bytes[TRANSACTION_AT..HEAD]);
        transaction
    }

    fn put(&mut self, kind: u16, value: &[u8]) {
        self.bytes.extend(kind.to_be_bytes());
        self.bytes.extend((value.len() as u16).to_be_bytes());
        self.bytes.extend(value);
        self.bytes.resize(self.bytes.len().next_multiple_of(WORD), 0);
    }

    fn set_length(&mut self, extra: usize) {
        let length = (self.bytes.len() + extra - HEAD) as u16;
        self.bytes[LENGTH_AT..WORD].copy_from_slice(&length.to_be_bytes());
    }

    pub(crate) fn push(&mut self, attribute: Attribute<'_>) -> &mut Writer {
        let (kind, value) = attribute.encode(&self.transaction());
        self.put(kind, &value);
        self
    }

    // Adds the integrity of all written so far under the key (RFC 8489, 14.5, 14.6).
    pub(crate) fn seal(&mut self, crypto: &impl Crypto, integrity: Integrity<'_>) -> &mut Writer {
        match integrity {
            Integrity::Sha1(key) => {
                self.set_length(ATTRIBUTE_HEAD + super::SHA1_BYTES);
                let code = crypto.hmac_sha1(key, &[&self.bytes]);
                self.put(MESSAGE_INTEGRITY, &code);
            }
            Integrity::Sha256(key) => {
                self.set_length(ATTRIBUTE_HEAD + SHA256_BYTES);
                let code = crypto.hmac_sha256(key, &[&self.bytes]);
                self.put(MESSAGE_INTEGRITY_SHA256, &code);
            }
        }
        self
    }

    // The message whole, with its fingerprint where asked (RFC 8489, 14.7).
    pub(crate) fn finish(mut self, has_fingerprint: bool) -> Vec<u8> {
        if has_fingerprint {
            self.set_length(ATTRIBUTE_HEAD + WORD);
            let sum = crc32(&self.bytes) ^ FINGERPRINT_XOR;
            self.put(FINGERPRINT, &sum.to_be_bytes());
        }
        self.set_length(0);
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::unhex;
    use crate::net::stun::attribute::{REQUESTED_TRANSPORT, SOFTWARE, UNKNOWN_ATTRIBUTE};
    use crate::net::stun::tests::Exact;
    use core::cell::Cell;

    const PASSWORD: &[u8] = b"VOkJxbRl1RmTxUk/WvJxBt";

    fn bytes(text: &str) -> Vec<u8> {
        unhex(&text.replace([' ', '\n'], "")).unwrap()
    }

    // RFC 5769, 2.1 to 2.4.
    fn request() -> Vec<u8> {
        bytes(
            "000100582112a442b7e7a701bc34d686fa87dfae802200105354554e20746573742063 6c69656e74
             002400046e0001ff80290008932ff9b151263b36000600096576746a3a6836765920202000080014
             9aeaa70cbfd8cb56781ef2b5b2d3f249c1b571a280280004e57a3bcf",
        )
    }

    fn response_v4() -> Vec<u8> {
        bytes(
            "0101003c2112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
             002000080001a147e112a643000800142b91f599fd9e90c38c7489f92af9ba53f06be7d7
             80280004c07d4c96",
        )
    }

    fn response_v6() -> Vec<u8> {
        bytes(
            "010100482112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
             002000140002a1470113a9faa5d3f179bc25f4b5bed2b9d900080014a382954e4be67bf1
             1784c97c8292c275bfe3ed4180280004c8fb0b4c",
        )
    }

    fn request_long() -> Vec<u8> {
        bytes(
            "000100602112a44278ad3433c6ad72c029da412e00060012e3839ee38388e383aae38383e382afe382b9
             0000001500 1c662f2f3439396b39353464364f4c33346f4c394653547679363473410014000b
             6578616d706c652e6f726700000800 14f67024656dd64a3e02b8e0712e85c9a28ca89666",
        )
    }

    fn exact() -> Exact {
        Exact(Cell::new(1))
    }

    #[test]
    fn rfc_5769_read() {
        let crypto = exact();
        let request = request();
        let message = Message::parse(&request).expect("the sample request");
        assert_eq!((message.method, message.class), (BINDING, Class::Request));
        assert_eq!(message.transaction, request[8..20], "its transaction");
        assert!(message.verify(&crypto, Integrity::Sha1(PASSWORD)), "the short-term integrity holds");
        assert!(message.has_fingerprint, "and its fingerprint");
        assert_eq!(
            (message.username, message.priority, message.controlled, message.controlling),
            (Some(&b"evtj:h6vY"[..]), Some(0x6e0001ff), Some(0x932f_f9b1_5126_3b36), None),
            "the user, the priority and the tie breaker of the controlled side"
        );
        assert!(message.required.is_empty(), "SOFTWARE is optional and passed over");
        assert!(!message.verify(&crypto, Integrity::Sha1(b"VOkJxbRl1RmTxUk/WvJxBu")), "nor under another password");
        for (sample, address) in
            [(response_v4(), "192.0.2.1:32853"), (response_v6(), "[2001:db8:1234:5678:11:2233:4455:6677]:32853")]
        {
            let message = Message::parse(&sample).expect("the sample response");
            assert_eq!((message.method, message.class), (BINDING, Class::Success));
            assert!(message.verify(&crypto, Integrity::Sha1(PASSWORD)) && message.has_fingerprint);
            assert_eq!(message.mapped, Some(address.parse().unwrap()), "the mapped address unmasked");
        }
        let long = request_long();
        let message = Message::parse(&long).expect("the request under the long-term credential");
        let key = crypto.md5(&[message.username.unwrap(), b":", message.realm.unwrap(), b":TheMatrIX"]);
        assert!(message.verify(&crypto, Integrity::Sha1(&key)), "under MD5 of the user, realm and password");
        assert_eq!(
            (message.username, message.nonce, message.realm, message.has_fingerprint),
            (
                Some("\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}".as_bytes()),
                Some(&b"f//499k954d6OL34oL9FSTvy64sA"[..]),
                Some(&b"example.org"[..]),
                false
            )
        );
    }

    // Writes a sample again: its attributes before the integrity, each padded as the sample pads it (RFC 5769, 2: padding may take any value), then the integrity and the fingerprint.
    fn rewrite(sample: &[u8], attributes: &[Attribute<'_>], key: &[u8], has_fingerprint: bool) -> Vec<u8> {
        let message = Message::parse(sample).unwrap();
        let mut writer = Writer::new(message.method, message.class, &message.transaction);
        for attribute in attributes {
            let (_, value) = attribute.encode(&message.transaction);
            let padding = writer.bytes.len() + ATTRIBUTE_HEAD + value.len();
            writer.push(*attribute);
            let end = writer.bytes.len();
            writer.bytes[padding..end].copy_from_slice(&sample[padding..end]);
        }
        writer.seal(&exact(), Integrity::Sha1(key));
        writer.finish(has_fingerprint)
    }

    #[test]
    fn rfc_5769_write() {
        let software = Attribute::Raw(SOFTWARE, b"STUN test client");
        let attributes = [
            software,
            Attribute::Priority(0x6e0001ff),
            Attribute::Controlled(0x932f_f9b1_5126_3b36),
            Attribute::Username(b"evtj:h6vY"),
        ];
        assert_eq!(rewrite(&request(), &attributes, PASSWORD, true), request(), "the sample request byte for byte");
        let server = Attribute::Raw(SOFTWARE, b"test vector");
        let v4 = [server, Attribute::XorMapped("192.0.2.1:32853".parse().unwrap())];
        assert_eq!(rewrite(&response_v4(), &v4, PASSWORD, true), response_v4(), "the IPv4 response");
        let v6 = [server, Attribute::XorMapped("[2001:db8:1234:5678:11:2233:4455:6677]:32853".parse().unwrap())];
        assert_eq!(rewrite(&response_v6(), &v6, PASSWORD, true), response_v6(), "the IPv6 response");
        let user = "\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}".as_bytes();
        let key = exact().md5(&[user, b":example.org:TheMatrIX"]);
        let long = [
            Attribute::Username(user),
            Attribute::Nonce(b"f//499k954d6OL34oL9FSTvy64sA"),
            Attribute::Realm(b"example.org"),
        ];
        assert_eq!(rewrite(&request_long(), &long, &key, false), request_long(), "the long-term request");
    }

    #[test]
    fn type_bits() {
        for method in [BINDING, ALLOCATE, REFRESH, SEND, DATA_METHOD, CREATE_PERMISSION, CHANNEL_BIND, 0x0FFF] {
            for class in [Class::Request, Class::Indication, Class::Success, Class::Error] {
                assert_eq!(split_type(type_of(method, class)), (method, class), "{method} {class:?}");
            }
        }
        assert_eq!(
            [type_of(BINDING, Class::Success), type_of(BINDING, Class::Error), type_of(SEND, Class::Indication)],
            [0x0101, 0x0111, 0x0016],
            "the types of RFC 8489, 5 and RFC 8656, 17"
        );
    }

    #[test]
    fn attributes_round_trip() {
        let crypto = exact();
        let transaction = [9u8; 12];
        let peer: SocketAddr = "[2001:db8::7]:3478".parse().unwrap();
        let mut writer = Writer::new(ALLOCATE, Class::Error, &transaction);
        writer
            .push(Attribute::Error(UNKNOWN_ATTRIBUTE))
            .push(Attribute::Unknown(&[0x0031, 0x0032]))
            .push(Attribute::Realm(b"relay"))
            .push(Attribute::Nonce(b"obMatJos2AAAB"))
            .push(Attribute::Lifetime(600))
            .push(Attribute::Peer(peer))
            .push(Attribute::Peer("198.51.100.4:9".parse().unwrap()))
            .push(Attribute::Data(b"carried"))
            .push(Attribute::Relayed("203.0.113.9:50000".parse().unwrap()))
            .push(Attribute::Channel(0x4002))
            .push(Attribute::Alternate("192.0.2.80:3478".parse().unwrap()))
            .push(Attribute::Raw(ALTERNATE_DOMAIN, b"relay.example.org"))
            .push(Attribute::Algorithms(&[0, 2, 0, 0, 0, 1, 0, 0]))
            .push(Attribute::Nomination(3))
            .push(Attribute::UseCandidate)
            .push(Attribute::Mapped("192.0.2.1:1".parse().unwrap()));
        writer.seal(&crypto, Integrity::Sha256(b"key"));
        let bytes = writer.finish(true);
        let message = Message::parse(&bytes).unwrap();
        assert!(
            message.verify(&crypto, Integrity::Sha256(b"key")) && !message.verify(&crypto, Integrity::Sha1(b"key"))
        );
        assert_eq!((message.method, message.class, message.error), (ALLOCATE, Class::Error, Some(420)));
        assert_eq!(message.unknown, [0x0031, 0x0032]);
        assert_eq!(
            (message.realm, message.nonce, message.lifetime),
            (Some(&b"relay"[..]), Some(&b"obMatJos2AAAB"[..]), Some(600))
        );
        assert_eq!(message.peers, [peer, "198.51.100.4:9".parse().unwrap()], "every peer, in order");
        assert_eq!((message.data, message.channel), (Some(&b"carried"[..]), Some(0x4002)));
        assert_eq!(message.relayed, Some("203.0.113.9:50000".parse().unwrap()));
        assert_eq!(message.alternate, Some("192.0.2.80:3478".parse().unwrap()));
        assert_eq!(
            (message.domain, message.algorithms),
            (Some(&b"relay.example.org"[..]), Some(&[0, 2, 0, 0, 0, 1, 0, 0][..]))
        );
        assert_eq!((message.nomination, message.use_candidate), (Some(3), true));
        assert_eq!(message.mapped, Some("192.0.2.1:1".parse().unwrap()), "MAPPED-ADDRESS where no XOR one is");
        let mut writer = Writer::new(BINDING, Class::Request, &transaction);
        writer
            .push(Attribute::Raw(0x0031, b"x"))
            .push(Attribute::Raw(0x8031, b"y"))
            .push(Attribute::Raw(REQUESTED_TRANSPORT, &[17, 0, 0, 0]));
        let bytes = writer.finish(false);
        let message = Message::parse(&bytes).unwrap();
        assert_eq!(
            message.required,
            [0x0031],
            "an unknown required attribute is noted, an optional one and a known one are not"
        );
        assert_eq!(message.find(REQUESTED_TRANSPORT), Some(&[17, 0, 0, 0][..]));
        assert!(
            !message.is_sealed() && !message.verify(&crypto, Integrity::Sha1(b"key")),
            "no integrity, nothing holds"
        );
    }

    #[test]
    fn integrity_coverage() {
        let crypto = exact();
        let mut writer = Writer::new(BINDING, Class::Request, &[3; 12]);
        writer.push(Attribute::Username(b"a:b")).seal(&crypto, Integrity::Sha1(b"pwd"));
        let mut bytes = writer.finish(false);
        let sealed = Message::parse(&bytes).unwrap();
        assert!(sealed.verify(&crypto, Integrity::Sha1(b"pwd")));
        bytes.extend([0, 0x24, 0, 4, 0, 0, 0, 9]);
        let length = (bytes.len() - HEAD) as u16;
        bytes[2..4].copy_from_slice(&length.to_be_bytes());
        let extended = Message::parse(&bytes).unwrap();
        assert!(extended.verify(&crypto, Integrity::Sha1(b"pwd")), "the integrity holds over what it covers");
        assert_eq!(extended.priority, None, "and what follows it is not believed");
    }

    #[test]
    fn malformed_refused() {
        let mut writer = Writer::new(BINDING, Class::Request, &[5; 12]);
        writer.push(Attribute::Username(b"a:b")).push(Attribute::Priority(7)).push(Attribute::Controlled(9));
        let plain = writer.finish(false);
        assert!(Message::parse(&plain).is_some());
        let refused = |sample: &[u8], change: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = sample.to_vec();
            change(&mut bytes);
            Message::parse(&bytes).is_none()
        };
        assert!(refused(&plain, &|bytes| bytes[0] = 0x40), "a first byte of another protocol");
        assert!(refused(&plain, &|bytes| bytes[5] ^= 1), "no magic cookie");
        assert!(refused(&plain, &|bytes| bytes.push(0)), "longer than its length");
        assert!(refused(&plain, &|bytes| bytes.truncate(bytes.len() - 4)), "shorter");
        assert!(refused(&plain, &|bytes| bytes[23] = 40), "a user name past the end");
        assert!(refused(&plain, &|bytes| bytes[31] = 9), "a priority of nine bytes");
        assert!(refused(&plain, &|bytes| bytes[39] = 5), "ICE-CONTROLLED of five bytes");
        assert!(refused(&request(), &|bytes| *bytes.last_mut().unwrap() ^= 1), "a wrong fingerprint");
        let mut after = plain.clone();
        let length = (after.len() + 2 * ATTRIBUTE_HEAD + WORD - HEAD) as u16;
        after[2..4].copy_from_slice(&length.to_be_bytes());
        let sum = crc32(&after) ^ FINGERPRINT_XOR;
        after.extend([0x80, 0x28, 0, 4]);
        after.extend(sum.to_be_bytes());
        after.extend([0x80, 0x22, 0, 0]);
        assert!(Message::parse(&after).is_none(), "an attribute after a fingerprint that holds");
        assert!(Message::parse(&plain[..19]).is_none() && Message::parse(&[]).is_none());
        assert!(
            is_message(&plain) && !is_message(&[0x80, 0, 0, 0, 0x21, 0x12, 0xA4, 0x42]) && !is_message(&plain[..7])
        );
    }
}
