use super::{Crypto, TAG_BYTES, WIDE_NONCE_BYTES};
use crate::base::bytes::{le32, same};
use crate::crypto::KEY_BYTES;
use crate::crypto::blake2s::{self, HASH_BYTES};
use alloc::vec::Vec;
use core::time::Duration;

const CONSTRUCTION: &[u8] = b"Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";
const IDENTIFIER: &[u8] = b"WireGuard v1 zx2c4 Jason@zx2c4.com";
const MAC1_LABEL: &[u8] = b"mac1----";
const COOKIE_LABEL: &[u8] = b"cookie--";
const INITIATION: u32 = 1;
const RESPONSE: u32 = 2;
const COOKIE_REPLY: u32 = 3;
const DATA: u32 = 4;
const MAC_BYTES: usize = 16;
pub(super) const COOKIE_BYTES: usize = 16;
const STAMP_BYTES: usize = 12;
const WORD: usize = 4;
const INITIATION_BYTES: usize = 2 * WORD + KEY_BYTES + KEY_BYTES + TAG_BYTES + STAMP_BYTES + TAG_BYTES + 2 * MAC_BYTES;
const RESPONSE_BYTES: usize = 3 * WORD + KEY_BYTES + TAG_BYTES + 2 * MAC_BYTES;
const COOKIE_REPLY_BYTES: usize = 2 * WORD + WIDE_NONCE_BYTES + COOKIE_BYTES + TAG_BYTES;
pub(super) const DATA_HEAD: usize = 2 * WORD + 8;
const PADDING: usize = 16;
// TAI64's label of the second 1970 began with, ten seconds ahead of UTC's.
const TAI64_EPOCH: u64 = (1 << 62) + 10;

/// The keys of a tunnel's two ends; its `Debug` tells none of them.
#[derive(Clone, PartialEq, Eq)]
pub struct Keys {
    pub(super) private: [u8; KEY_BYTES],
    pub(super) public: [u8; KEY_BYTES],
    pub(super) peer: [u8; KEY_BYTES],
    pub(super) preshared: [u8; KEY_BYTES],
}

impl core::fmt::Debug for Keys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Keys")
    }
}

// The message's kind in its first word, whose other three bytes are nought.
pub(super) fn kind_of(message: &[u8]) -> Option<Kind> {
    match le32(message, 0)? {
        INITIATION => Some(Kind::Initiation),
        RESPONSE => Some(Kind::Response),
        COOKIE_REPLY => Some(Kind::CookieReply),
        DATA => Some(Kind::Data),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Initiation,
    Response,
    CookieReply,
    Data,
}

// Noise's HKDF over HMAC-BLAKE2s: as many keys as asked, each from the one before.
fn derive<const N: usize>(chain: &[u8; HASH_BYTES], input: &[u8]) -> [[u8; HASH_BYTES]; N] {
    let secret = blake2s::hmac(chain, &[input]);
    let mut keys = [[0u8; HASH_BYTES]; N];
    for count in 0..N {
        let previous: &[u8] = if count == 0 { &[] } else { &keys[count - 1] };
        keys[count] = blake2s::hmac(&secret, &[previous, &[count as u8 + 1]]);
    }
    keys
}

fn mix_hash(hash: &[u8; HASH_BYTES], data: &[u8]) -> [u8; HASH_BYTES] {
    blake2s::hash(&[hash, data])
}

fn mac1_key(public: &[u8; KEY_BYTES]) -> [u8; HASH_BYTES] {
    blake2s::hash(&[MAC1_LABEL, public])
}

// TAI64N of a time since 1970 began, as a peer compares it with the last it took from these keys.
pub(super) fn to_stamp(utc: Duration) -> [u8; STAMP_BYTES] {
    let mut stamp = [0u8; STAMP_BYTES];
    stamp[..8].copy_from_slice(&TAI64_EPOCH.wrapping_add(utc.as_secs()).to_be_bytes());
    stamp[8..].copy_from_slice(&utc.subsec_nanos().to_be_bytes());
    stamp
}

// A handshake begun and not yet answered: what its answer is read with.
pub(super) struct Handshake {
    chain: [u8; HASH_BYTES],
    hash: [u8; HASH_BYTES],
    ephemeral: [u8; KEY_BYTES],
    pub(super) index: u32,
    mac1: [u8; MAC_BYTES],
}

// The keys a completed handshake gives a session, and the peer's number for it.
pub(super) struct Agreed {
    pub(super) sending: [u8; KEY_BYTES],
    pub(super) receiving: [u8; KEY_BYTES],
    pub(super) peer_index: u32,
}

fn seal_onto(
    crypto: &impl Crypto,
    key: &[u8; KEY_BYTES],
    hash: &[u8; HASH_BYTES],
    message: &mut Vec<u8>,
    plain: &[u8],
) -> Option<()> {
    let start = message.len();
    message.extend(plain);
    let tag = crypto.seal(key, 0, hash, &mut message[start..])?;
    message.extend(tag);
    Some(())
}

// The message that begins a handshake under the number `index`, stamped, and signed with the cookie where the peer asked for one.
pub(super) fn initiate(
    crypto: &impl Crypto,
    keys: &Keys,
    index: u32,
    stamp: &[u8; STAMP_BYTES],
    cookie: Option<&[u8; COOKIE_BYTES]>,
) -> Option<(Handshake, Vec<u8>)> {
    let mut ephemeral = [0u8; KEY_BYTES];
    if !crypto.random(&mut ephemeral) {
        return None;
    }
    let ephemeral_public = crypto.public_of(&ephemeral)?;
    let chain = blake2s::hash(&[CONSTRUCTION]);
    let hash = mix_hash(&mix_hash(&chain, IDENTIFIER), &keys.peer);
    let [chain] = derive(&chain, &ephemeral_public);
    let hash = mix_hash(&hash, &ephemeral_public);
    let mut message = Vec::with_capacity(INITIATION_BYTES);
    message.extend(INITIATION.to_le_bytes());
    message.extend(index.to_le_bytes());
    message.extend(ephemeral_public);
    let [chain, key] = derive(&chain, &crypto.agree(&ephemeral, &keys.peer)?);
    seal_onto(crypto, &key, &hash, &mut message, &keys.public)?;
    let hash = mix_hash(&hash, &message[2 * WORD + KEY_BYTES..]);
    let [chain, key] = derive(&chain, &crypto.agree(&keys.private, &keys.peer)?);
    let sealed_stamp = message.len();
    seal_onto(crypto, &key, &hash, &mut message, stamp)?;
    let hash = mix_hash(&hash, &message[sealed_stamp..]);
    let mac1: [u8; MAC_BYTES] = blake2s::keyed(&mac1_key(&keys.peer), &[&message]);
    message.extend(mac1);
    let mac2 = cookie.map_or([0; MAC_BYTES], |cookie| blake2s::keyed(cookie, &[&message]));
    message.extend(mac2);
    Some((Handshake { chain, hash, ephemeral, index, mac1 }, message))
}

impl Handshake {
    // Reads the peer's answer: none for what is no answer to this handshake by these keys.
    pub(super) fn complete(&self, crypto: &impl Crypto, keys: &Keys, message: &[u8]) -> Option<Agreed> {
        let message: &[u8; RESPONSE_BYTES] = message.try_into().ok()?;
        let (signed, macs) = message.split_at(RESPONSE_BYTES - 2 * MAC_BYTES);
        let mac1: [u8; MAC_BYTES] = blake2s::keyed(&mac1_key(&keys.public), &[signed]);
        if le32(message, 2 * WORD)? != self.index || !same(&mac1, &macs[..MAC_BYTES]) {
            return None;
        }
        let peer_ephemeral: &[u8; KEY_BYTES] = message[3 * WORD..].first_chunk()?;
        let [chain] = derive(&self.chain, peer_ephemeral);
        let hash = mix_hash(&self.hash, peer_ephemeral);
        let [chain] = derive(&chain, &crypto.agree(&self.ephemeral, peer_ephemeral)?);
        let [chain] = derive(&chain, &crypto.agree(&keys.private, peer_ephemeral)?);
        let [chain, mixed, key] = derive(&chain, &keys.preshared);
        let hash = mix_hash(&hash, &mixed);
        let mut nothing: [u8; TAG_BYTES] = *message[3 * WORD + KEY_BYTES..].first_chunk()?;
        if !crypto.open(&key, 0, &hash, &mut nothing) {
            return None;
        }
        let [sending, receiving] = derive(&chain, &[]);
        Some(Agreed { sending, receiving, peer_index: le32(message, WORD)? })
    }

    // Reads the cookie a peer under load answers with, which the next message of this handshake is signed with.
    pub(super) fn read_cookie(&self, crypto: &impl Crypto, keys: &Keys, message: &[u8]) -> Option<[u8; COOKIE_BYTES]> {
        let message: &[u8; COOKIE_REPLY_BYTES] = message.try_into().ok()?;
        if le32(message, WORD)? != self.index {
            return None;
        }
        let nonce: &[u8; WIDE_NONCE_BYTES] = message[2 * WORD..].first_chunk()?;
        let mut sealed: [u8; COOKIE_BYTES + TAG_BYTES] = *message[2 * WORD + WIDE_NONCE_BYTES..].first_chunk()?;
        let key = blake2s::hash(&[COOKIE_LABEL, &keys.peer]);
        crypto.open_wide(&key, nonce, &self.mac1, &mut sealed).then(|| *sealed.first_chunk().expect("the cookie"))
    }
}

// A data message of a packet, filled up to whole blocks of sixteen within `most` bytes; an empty packet keeps the session alive.
pub(super) fn seal_data(
    crypto: &impl Crypto,
    key: &[u8; KEY_BYTES],
    peer_index: u32,
    counter: u64,
    packet: &[u8],
    most: usize,
) -> Option<Vec<u8>> {
    let padded = packet.len().next_multiple_of(PADDING).min(most.max(packet.len()));
    let mut message = Vec::with_capacity(DATA_HEAD + padded + TAG_BYTES);
    message.extend(DATA.to_le_bytes());
    message.extend(peer_index.to_le_bytes());
    message.extend(counter.to_le_bytes());
    message.extend(packet);
    message.resize(DATA_HEAD + padded, 0);
    let tag = crypto.seal(key, counter, &[], &mut message[DATA_HEAD..])?;
    message.extend(tag);
    Some(message)
}

// The number a data message names its session by, and its counter.
pub(super) fn read_data_head(message: &[u8]) -> Option<(u32, u64)> {
    let counter = u64::from_le_bytes(*message.get(2 * WORD..)?.first_chunk()?);
    (message.len() >= DATA_HEAD + TAG_BYTES).then_some((le32(message, WORD)?, counter))
}

// What a data message carries, its filling with it; none for what the key does not open.
pub(super) fn open_data(crypto: &impl Crypto, key: &[u8; KEY_BYTES], counter: u64, message: &[u8]) -> Option<Vec<u8>> {
    let mut sealed = message.get(DATA_HEAD..)?.to_vec();
    let length = sealed.len().checked_sub(TAG_BYTES)?;
    crypto.open(key, counter, &[], &mut sealed).then(|| {
        sealed.truncate(length);
        sealed
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::crypto::X25519;
    use crate::net::wireguard::tests::Toy;

    // The side that answers, as far as the tests ask it.
    pub(in crate::net::wireguard) struct Responder {
        pub(in crate::net::wireguard) keys: Keys,
        pub(in crate::net::wireguard) index: u32,
        pub(in crate::net::wireguard) last_stamp: Option<[u8; STAMP_BYTES]>,
    }

    pub(in crate::net::wireguard) fn pair(crypto: &Toy) -> (Keys, Responder) {
        let (own, their, preshared) = ([0x11; KEY_BYTES], [0x22; KEY_BYTES], [0x33; KEY_BYTES]);
        let (own_public, their_public) = (crypto.public_of(&own).unwrap(), crypto.public_of(&their).unwrap());
        let keys = Keys { private: own, public: own_public, peer: their_public, preshared };
        let responder = Responder {
            keys: Keys { private: their, public: their_public, peer: own_public, preshared },
            index: 0x0B0B_0B0B,
            last_stamp: None,
        };
        (keys, responder)
    }

    impl Responder {
        // Reads a message that begins a handshake and answers it: the answer, and the session's keys as this side has them.
        pub(in crate::net::wireguard) fn answer(&mut self, crypto: &Toy, message: &[u8]) -> Option<(Vec<u8>, Agreed)> {
            let message: &[u8; INITIATION_BYTES] = message.try_into().ok()?;
            let (signed, macs) = message.split_at(INITIATION_BYTES - 2 * MAC_BYTES);
            let mac1: [u8; MAC_BYTES] = blake2s::keyed(&mac1_key(&self.keys.public), &[signed]);
            if kind_of(message) != Some(Kind::Initiation) || mac1 != macs[..MAC_BYTES] {
                return None;
            }
            let peer_index = le32(message, WORD)?;
            let ephemeral: &[u8; KEY_BYTES] = message[2 * WORD..].first_chunk()?;
            let chain = blake2s::hash(&[CONSTRUCTION]);
            let hash = mix_hash(&mix_hash(&chain, IDENTIFIER), &self.keys.public);
            let [chain] = derive(&chain, ephemeral);
            let hash = mix_hash(&hash, ephemeral);
            let [chain, key] = derive(&chain, &crypto.agree(&self.keys.private, ephemeral)?);
            let sealed_static = &message[2 * WORD + KEY_BYTES..][..KEY_BYTES + TAG_BYTES];
            let mut opened = sealed_static.to_vec();
            if !crypto.open(&key, 0, &hash, &mut opened) || opened[..KEY_BYTES] != self.keys.peer {
                return None;
            }
            let hash = mix_hash(&hash, sealed_static);
            let [chain, key] = derive(&chain, &crypto.agree(&self.keys.private, &self.keys.peer)?);
            let sealed_stamp = &message[2 * WORD + 2 * KEY_BYTES + TAG_BYTES..][..STAMP_BYTES + TAG_BYTES];
            let mut opened = sealed_stamp.to_vec();
            if !crypto.open(&key, 0, &hash, &mut opened) {
                return None;
            }
            let stamp: [u8; STAMP_BYTES] = *opened.first_chunk()?;
            if self.last_stamp.is_some_and(|last| stamp <= last) {
                return None;
            }
            self.last_stamp = Some(stamp);
            let hash = mix_hash(&hash, sealed_stamp);
            let own_ephemeral = [0x44; KEY_BYTES];
            let own_public = crypto.public_of(&own_ephemeral)?;
            let [chain] = derive(&chain, &own_public);
            let hash = mix_hash(&hash, &own_public);
            let [chain] = derive(&chain, &crypto.agree(&own_ephemeral, ephemeral)?);
            let [chain] = derive(&chain, &crypto.agree(&own_ephemeral, &self.keys.peer)?);
            let [chain, mixed, key] = derive(&chain, &self.keys.preshared);
            let hash = mix_hash(&hash, &mixed);
            self.index += 1;
            let mut answer = Vec::with_capacity(RESPONSE_BYTES);
            answer.extend(RESPONSE.to_le_bytes());
            answer.extend(self.index.to_le_bytes());
            answer.extend(peer_index.to_le_bytes());
            answer.extend(own_public);
            seal_onto(crypto, &key, &hash, &mut answer, &[])?;
            let mac1: [u8; MAC_BYTES] = blake2s::keyed(&mac1_key(&self.keys.peer), &[&answer]);
            answer.extend(mac1);
            answer.extend([0; MAC_BYTES]);
            let [receiving, sending] = derive(&chain, &[]);
            Some((answer, Agreed { sending, receiving, peer_index }))
        }

        // The answer of a peer under load to a message that began a handshake: a cookie sealed for whoever sent it.
        pub(in crate::net::wireguard) fn refuse(&self, message: &[u8], cookie: [u8; COOKIE_BYTES]) -> Vec<u8> {
            let mac1 = &message[INITIATION_BYTES - 2 * MAC_BYTES..][..MAC_BYTES];
            let (nonce, mut sealed) = ([0x55; WIDE_NONCE_BYTES], cookie.to_vec());
            let tag = Toy::seal_wide(&blake2s::hash(&[COOKIE_LABEL, &self.keys.public]), &nonce, mac1, &mut sealed);
            let mut reply = COOKIE_REPLY.to_le_bytes().to_vec();
            reply.extend_from_slice(&message[WORD..2 * WORD]);
            reply.extend(nonce);
            reply.extend(sealed);
            reply.extend(tag);
            reply
        }
    }

    #[test]
    fn handshake_gives_both_sides_one_session() {
        let crypto = Toy::default();
        let (keys, mut responder) = pair(&crypto);
        let stamp = to_stamp(Duration::new(1_800_000_000, 5));
        let (handshake, message) = initiate(&crypto, &keys, 0x0A0A_0A0A, &stamp, None).unwrap();
        assert_eq!(
            (message.len(), &message[..8], kind_of(&message)),
            (148, &[1, 0, 0, 0, 0x0A, 0x0A, 0x0A, 0x0A][..], Some(Kind::Initiation)),
            "the kind, three bytes of nought, this side's number, lowest byte first"
        );
        assert_eq!(message[132..], [0; 16], "no cookie, no second signature");
        let (answer, theirs) = responder.answer(&crypto, &message).expect("the peer takes it");
        assert_eq!((answer.len(), kind_of(&answer)), (92, Some(Kind::Response)));
        let ours = handshake.complete(&crypto, &keys, &answer).expect("and its answer is taken");
        assert!(
            ours.sending == theirs.receiving && ours.receiving == theirs.sending && ours.sending != ours.receiving,
            "what one side sends under, the other receives under"
        );
        assert_eq!((ours.peer_index, theirs.peer_index), (0x0B0B_0B0C, 0x0A0A_0A0A));
        assert!(responder.answer(&crypto, &message).is_none(), "the same stamp is taken once");
    }

    #[test]
    fn answers_refused() {
        let crypto = Toy::default();
        let (keys, mut responder) = pair(&crypto);
        let (handshake, message) = initiate(&crypto, &keys, 7, &to_stamp(Duration::from_secs(9)), None).unwrap();
        let (answer, _) = responder.answer(&crypto, &message).unwrap();
        for at in [0, 5, 9, 12, 44, 60, 75] {
            let mut changed = answer.clone();
            changed[at] ^= 1;
            assert!(handshake.complete(&crypto, &keys, &changed).is_none(), "byte {at} changed");
        }
        assert!(handshake.complete(&crypto, &keys, &answer[..91]).is_none(), "cut short");
        let mut other_psk = keys.clone();
        other_psk.preshared[0] ^= 1;
        assert!(handshake.complete(&crypto, &other_psk, &answer).is_none(), "another preshared key");
        let (other, _) = initiate(&crypto, &keys, 8, &to_stamp(Duration::from_secs(10)), None).unwrap();
        assert!(other.complete(&crypto, &keys, &answer).is_none(), "an answer to another handshake");
    }

    #[test]
    fn cookie_signs_the_next_message() {
        let crypto = Toy::default();
        let (keys, responder) = pair(&crypto);
        let (handshake, message) = initiate(&crypto, &keys, 7, &to_stamp(Duration::from_secs(9)), None).unwrap();
        let reply = responder.refuse(&message, [0x66; COOKIE_BYTES]);
        assert_eq!((reply.len(), kind_of(&reply)), (64, Some(Kind::CookieReply)));
        assert_eq!(handshake.read_cookie(&crypto, &keys, &reply), Some([0x66; COOKIE_BYTES]));
        let mut changed = reply.clone();
        changed[40] ^= 1;
        assert_eq!(handshake.read_cookie(&crypto, &keys, &changed), None);
        let (other, _) = initiate(&crypto, &keys, 8, &to_stamp(Duration::from_secs(10)), None).unwrap();
        assert_eq!(other.read_cookie(&crypto, &keys, &reply), None, "a cookie for another message");
        let (_, signed) =
            initiate(&crypto, &keys, 9, &to_stamp(Duration::from_secs(11)), Some(&[0x66; COOKIE_BYTES])).unwrap();
        let mac2: [u8; MAC_BYTES] = blake2s::keyed(&[0x66; COOKIE_BYTES], &[&signed[..132]]);
        assert_eq!(signed[132..], mac2, "the second signature, under the cookie, over all before it");
    }

    #[test]
    fn data_messages() {
        let (crypto, key) = (Toy::default(), [0x77; KEY_BYTES]);
        let packet: Vec<u8> = (0..21).collect();
        let message = seal_data(&crypto, &key, 0x0102_0304, 5, &packet, 1384).unwrap();
        assert_eq!(
            (message.len(), &message[..16], kind_of(&message)),
            (16 + 32 + 16, &[4, 0, 0, 0, 4, 3, 2, 1, 5, 0, 0, 0, 0, 0, 0, 0][..], Some(Kind::Data)),
            "the kind, the peer's number, the counter, and the packet filled up to thirty-two bytes"
        );
        assert_eq!(read_data_head(&message), Some((0x0102_0304, 5)));
        let opened = open_data(&crypto, &key, 5, &message).unwrap();
        assert_eq!((&opened[..21], &opened[21..]), (&packet[..], &[0u8; 11][..]));
        assert!(
            open_data(&crypto, &key, 6, &message).is_none() && open_data(&crypto, &[0x78; 32], 5, &message).is_none()
        );
        assert_eq!(
            seal_data(&crypto, &key, 1, 0, &packet, 24).unwrap().len(),
            16 + 24 + 16,
            "filled no further than the tunnel carries"
        );
        assert_eq!(seal_data(&crypto, &key, 1, 0, &packet, 20).unwrap().len(), 16 + 21 + 16, "and never cut");
        let alive = seal_data(&crypto, &key, 1, 0, &[], 1384).unwrap();
        assert_eq!((alive.len(), open_data(&crypto, &key, 0, &alive)), (32, Some(Vec::new())), "an empty packet");
        assert_eq!((read_data_head(&alive[..31]), kind_of(&[9, 0, 0, 0])), (None, None));
    }

    #[test]
    fn stamps_and_keys_text() {
        assert_eq!(
            to_stamp(Duration::new(1, 2)),
            [0x40, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 2],
            "TAI64N: the second with its label, then the nanoseconds, highest byte first"
        );
        assert!(to_stamp(Duration::new(5, 0)) > to_stamp(Duration::new(4, 999_999_999)));
        let (keys, _) = pair(&Toy::default());
        assert_eq!(alloc::format!("{keys:?}"), "Keys");
    }
}
