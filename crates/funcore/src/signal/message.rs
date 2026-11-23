// The two ciphertexts of a session on the wire, as libsignal writes them for session version 4.

use super::Crypto;
use crate::base::bytes::same;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::vec::Vec;

const VERSION_BYTE: u8 = 0x44;
const CURVE_TYPE: u8 = 0x05;
const KYBER_TYPE: u8 = 0x08;
const MAC_BYTES: usize = 8;

pub(super) fn encode_key(public: &[u8; 32]) -> [u8; 33] {
    let mut bytes = [CURVE_TYPE; 33];
    bytes[1..].copy_from_slice(public);
    bytes
}

pub(super) fn decode_key(bytes: &[u8]) -> Option<[u8; 32]> {
    match bytes {
        [CURVE_TYPE, key @ ..] if key.len() >= 32 => key[..32].try_into().ok(),
        _ => None,
    }
}

pub(super) fn encode_kyber(bytes: &[u8]) -> Vec<u8> {
    [&[KYBER_TYPE][..], bytes].concat()
}

pub(super) fn decode_kyber(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(&[KYBER_TYPE])
}

fn is_version_4(byte: u8) -> bool {
    byte >> 4 == 4
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SignalMessage {
    pub(super) ratchet_key: [u8; 32],
    pub(super) counter: u32,
    previous_counter: u32,
    pub(super) ciphertext: Vec<u8>,
    pub(super) ratchet: Vec<u8>,
    pub(super) addresses: Option<Vec<u8>>,
    signed_bytes: Vec<u8>,
    mac: [u8; MAC_BYTES],
}

// The counters alone: no key, nothing of what it carries and nobody it is with.
impl core::fmt::Debug for SignalMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SignalMessage")
            .field("counter", &self.counter)
            .field("previous_counter", &self.previous_counter)
            .finish_non_exhaustive()
    }
}

fn mac(
    crypto: &impl Crypto,
    key: &[u8; 32],
    sender: &[u8; 32],
    recipient: &[u8; 32],
    signed_bytes: &[u8],
) -> [u8; MAC_BYTES] {
    let full = crypto.hmac_sha256(key, &[&encode_key(sender), &encode_key(recipient), signed_bytes]);
    full[..MAC_BYTES].try_into().expect("eight of thirty-two")
}

impl SignalMessage {
    pub(crate) fn read(bytes: &[u8]) -> Option<SignalMessage> {
        if bytes.len() < 1 + MAC_BYTES || !is_version_4(bytes[0]) {
            return None;
        }
        let (signed_bytes, mac) = bytes.split_at(bytes.len() - MAC_BYTES);
        let message = Message::parse(&signed_bytes[1..])?;
        Some(SignalMessage {
            ratchet_key: decode_key(message.bytes(1)?)?,
            counter: message.u32(2)?,
            previous_counter: message.u32(3).unwrap_or(0),
            ciphertext: message.bytes(4)?.to_vec(),
            ratchet: message.bytes(5).unwrap_or_default().to_vec(),
            addresses: message.bytes(6).map(<[u8]>::to_vec),
            signed_bytes: signed_bytes.to_vec(),
            mac: mac.try_into().ok()?,
        })
    }

    pub(super) fn has_valid_mac(
        &self,
        crypto: &impl Crypto,
        key: &[u8; 32],
        sender: &[u8; 32],
        recipient: &[u8; 32],
    ) -> bool {
        same(&mac(crypto, key, sender, recipient, &self.signed_bytes), &self.mac)
    }
}

pub(super) struct Outgoing<'a> {
    pub(super) ratchet_key: &'a [u8; 32],
    pub(super) counter: u32,
    pub(super) previous_counter: u32,
    pub(super) ciphertext: &'a [u8],
    pub(super) ratchet: &'a [u8],
    pub(super) addresses: Option<&'a [u8]>,
}

pub(super) fn write_message(
    crypto: &impl Crypto,
    outgoing: &Outgoing,
    key: &[u8; 32],
    sender: &[u8; 32],
    recipient: &[u8; 32],
) -> Vec<u8> {
    let protobuf = Writer::new()
        .bytes(1, &encode_key(outgoing.ratchet_key))
        .varint(2, u64::from(outgoing.counter))
        .varint(3, u64::from(outgoing.previous_counter))
        .bytes(4, outgoing.ciphertext)
        .maybe((!outgoing.ratchet.is_empty()).then_some(|w: Writer| w.bytes(5, outgoing.ratchet)))
        .maybe(outgoing.addresses.map(|addresses| move |w: Writer| w.bytes(6, addresses)))
        .finish();
    let mut bytes = alloc::vec![VERSION_BYTE];
    bytes.extend_from_slice(&protobuf);
    let mac = mac(crypto, key, sender, recipient, &bytes);
    bytes.extend_from_slice(&mac);
    bytes
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PreKeyMessage {
    pub(super) registration: u32,
    pub(super) pre_key: Option<u32>,
    pub(super) signed_pre_key: u32,
    pub(super) kyber_pre_key: u32,
    pub(super) kyber_ciphertext: Vec<u8>,
    pub(super) base_key: [u8; 32],
    pub(super) identity: [u8; 32],
    pub(super) message: Vec<u8>,
}

// The numbers of the keys it names alone.
impl core::fmt::Debug for PreKeyMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreKeyMessage")
            .field("registration", &self.registration)
            .field("pre_key", &self.pre_key)
            .field("signed_pre_key", &self.signed_pre_key)
            .field("kyber_pre_key", &self.kyber_pre_key)
            .finish_non_exhaustive()
    }
}

impl PreKeyMessage {
    pub(crate) fn read(bytes: &[u8]) -> Option<PreKeyMessage> {
        let (&version, rest) = bytes.split_first()?;
        if !is_version_4(version) {
            return None;
        }
        let message = Message::parse(rest)?;
        Some(PreKeyMessage {
            registration: message.u32(5).unwrap_or(0),
            pre_key: message.u32(1),
            signed_pre_key: message.u32(6)?,
            kyber_pre_key: message.u32(7)?,
            kyber_ciphertext: decode_kyber(message.bytes(8)?)?.to_vec(),
            base_key: decode_key(message.bytes(2)?)?,
            identity: decode_key(message.bytes(3)?)?,
            message: message.bytes(4)?.to_vec(),
        })
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let protobuf = Writer::new()
            .maybe(self.pre_key.map(|id| move |w: Writer| w.varint(1, u64::from(id))))
            .bytes(2, &encode_key(&self.base_key))
            .bytes(3, &encode_key(&self.identity))
            .bytes(4, &self.message)
            .varint(5, u64::from(self.registration))
            .varint(6, u64::from(self.signed_pre_key))
            .varint(7, u64::from(self.kyber_pre_key))
            .bytes(8, &encode_kyber(&self.kyber_ciphertext))
            .finish();
        [&[VERSION_BYTE][..], &protobuf].concat()
    }
}
