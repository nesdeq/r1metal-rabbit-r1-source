//! A session with one device: PQXDH, and the Triple Ratchet to send and receive in it.

use super::address::Address;
use super::content::{DOUBLE_RATCHET, PREKEY_MESSAGE};
use super::curve::Pair;
use super::message::{self, Outgoing, PreKeyMessage, SignalMessage};
use super::spqr::{Direction, Limits, Ratchet};
use super::{Crypto, curve, hkdf, kem, read_skipped, write_skipped};
use crate::base::bytes::same;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

const HANDSHAKE: &[u8] = b"WhisperText_X25519_SHA-256_CRYSTALS-KYBER-1024";
const MESSAGE_KEYS: &[u8] = b"WhisperMessageKeys";
const RATCHET: &[u8] = b"WhisperRatchet";
pub(super) const MAX_JUMP: u32 = 25_000;
const SKIPPED_KEYS: usize = 2_000;
const RECEIVER_CHAINS: usize = 5;
const EARLIER_STATES: usize = 40;
const UNACKNOWLEDGED_AGE: u64 = 30 * 24 * 60 * 60 * 1000;

/// Why a message was not taken; nothing is changed by a refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    // Not a message of this protocol, or of another version.
    Malformed,
    // A message already received, or one so late its key is gone.
    Duplicate,
    // Too far ahead of its chain, or past the chain's last key.
    TooFar,
    // It does not open under any state this record holds.
    Unopened,
    NoSession,
    // A signature, a key or a pre-key does not check out.
    BadKey,
    // libsignal's `UntrustedIdentity`.
    Untrusted,
    Library,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ChainKey {
    pub(super) key: [u8; 32],
    pub(super) index: u32,
}

impl ChainKey {
    pub(super) fn seed(&self, crypto: &impl Crypto) -> [u8; 32] {
        crypto.hmac_sha256(&self.key, &[&[1]])
    }

    pub(super) fn next(&self, crypto: &impl Crypto) -> Option<ChainKey> {
        Some(ChainKey { key: crypto.hmac_sha256(&self.key, &[&[2]]), index: self.index.checked_add(1)? })
    }

    pub(super) fn take_seed(
        &mut self,
        crypto: &impl Crypto,
        counter: u32,
        skipped: &mut Vec<(u32, [u8; 32])>,
        jump: u32,
    ) -> Result<[u8; 32], Error> {
        if self.index > counter {
            let position = skipped.iter().position(|(index, _)| *index == counter).ok_or(Error::Duplicate)?;
            return Ok(skipped.remove(position).1);
        }
        if counter - self.index > jump {
            return Err(Error::TooFar);
        }
        // The seeds passed over go before those kept, newest first, in one go: put there a step at a time, a jump of 25 000 moves the whole list at every step, for a message not yet known to be its sender's.
        let mut passed = VecDeque::new();
        while self.index < counter {
            if passed.len() == SKIPPED_KEYS {
                passed.pop_front();
            }
            passed.push_back((self.index, self.seed(crypto)));
            *self = self.next(crypto).ok_or(Error::TooFar)?;
        }
        skipped.splice(0..0, passed.into_iter().rev());
        skipped.truncate(SKIPPED_KEYS);
        let seed = self.seed(crypto);
        *self = self.next(crypto).ok_or(Error::TooFar)?;
        Ok(seed)
    }
}

fn message_keys(
    crypto: &impl Crypto,
    seed: &[u8; 32],
    ratchet_key: Option<&[u8; 32]>,
) -> Option<([u8; 32], [u8; 32], [u8; 16])> {
    let out: [u8; 80] = hkdf(crypto, ratchet_key.map(|key| &key[..]), seed, &[MESSAGE_KEYS])?;
    Some((out[..32].try_into().ok()?, out[32..64].try_into().ok()?, out[64..].try_into().ok()?))
}

fn root_step(
    crypto: &impl Crypto,
    root: &[u8; 32],
    public: &[u8; 32],
    private: &[u8; 32],
) -> Option<([u8; 32], ChainKey)> {
    let out: [u8; 64] = hkdf(crypto, Some(root), &crypto.agree(private, public)?, &[RATCHET])?;
    Some((out[..32].try_into().ok()?, ChainKey { key: out[32..].try_into().ok()?, index: 0 }))
}

fn handshake<'a>(
    crypto: &impl Crypto,
    pairs: impl IntoIterator<Item = (&'a [u8; 32], &'a [u8; 32])>,
    shared: &[u8; 32],
) -> Result<[[u8; 32]; 3], Error> {
    let mut secrets = [0xFF; 32].to_vec();
    for (private, public) in pairs {
        secrets.extend_from_slice(&crypto.agree(private, public).ok_or(Error::BadKey)?);
    }
    secrets.extend_from_slice(shared);
    let out: [u8; 96] = hkdf(crypto, None, &secrets, &[HANDSHAKE]).ok_or(Error::Library)?;
    Ok(core::array::from_fn(|third| out[32 * third..32 * (third + 1)].try_into().expect("a third of 96")))
}

#[derive(Clone, PartialEq, Eq)]
struct Receiver {
    ratchet: [u8; 32],
    chain: ChainKey,
    skipped: Vec<(u32, [u8; 32])>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct UnacknowledgedPreKey {
    pre_key: Option<u32>,
    signed_pre_key: u32,
    kyber_pre_key: u32,
    kyber_ciphertext: Vec<u8>,
    base_key: [u8; 32],
    created: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct State {
    local_identity: [u8; 32],
    pub(super) remote_identity: [u8; 32],
    root: [u8; 32],
    sender: Pair,
    sender_chain: ChainKey,
    receivers: Vec<Receiver>,
    previous_counter: u32,
    unacknowledged: Option<UnacknowledgedPreKey>,
    local_registration: u32,
    pub(super) remote_registration: u32,
    base_key: [u8; 32],
    ratchet: Ratchet,
    is_own_account: bool,
}

impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("State")
            .field("local_registration", &self.local_registration)
            .field("remote_registration", &self.remote_registration)
            .field("own_account", &self.is_own_account)
            .finish_non_exhaustive()
    }
}

fn limits(is_own_account: bool) -> Limits {
    Limits { jump: if is_own_account { u32::MAX } else { MAX_JUMP }, out_of_order: SKIPPED_KEYS as u32 }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Bundle {
    pub(super) registration: u32,
    pub(super) identity: [u8; 32],
    pub(super) pre_key: Option<(u32, [u8; 32])>,
    pub(super) signed_pre_key: (u32, [u8; 32], [u8; 64]),
    pub(super) kyber_pre_key: (u32, Vec<u8>, [u8; 64]),
}

impl core::fmt::Debug for Bundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bundle").field("registration", &self.registration).finish_non_exhaustive()
    }
}

pub(super) struct PreKeys<'a> {
    pub(super) signed_pre_key: &'a Pair,
    pub(super) kyber_pre_key: &'a [u8],
    pub(super) pre_key: Option<&'a Pair>,
}

impl State {
    pub(super) fn begin(
        crypto: &impl Crypto,
        identity: &Pair,
        registration: u32,
        bundle: &Bundle,
        is_own_account: bool,
        now: u64,
    ) -> Result<State, Error> {
        let (signed_id, signed, signature) = &bundle.signed_pre_key;
        let (kyber_id, kyber, kyber_signature) = &bundle.kyber_pre_key;
        if !curve::verify(crypto, &bundle.identity, &[&message::encode_key(signed)], signature)
            || !curve::verify(crypto, &bundle.identity, &[&message::encode_kyber(kyber)], kyber_signature)
        {
            return Err(Error::BadKey);
        }
        let base = Pair::new(crypto).ok_or(Error::Library)?;
        let (ciphertext, shared) = kem::kyber_encapsulate(crypto, kyber).ok_or(Error::BadKey)?;
        let pairs = [(&identity.private, signed), (&base.private, &bundle.identity), (&base.private, signed)];
        let one_time = bundle.pre_key.as_ref().map(|(_, one_time)| (&base.private, one_time));
        let [root, chain, ratchet_key] = handshake(crypto, pairs.into_iter().chain(one_time), &shared)?;
        let sender = Pair::new(crypto).ok_or(Error::Library)?;
        let (root, sender_chain) = root_step(crypto, &root, signed, &sender.private).ok_or(Error::BadKey)?;
        Ok(State {
            local_identity: identity.public,
            remote_identity: bundle.identity,
            root,
            sender,
            sender_chain,
            receivers: alloc::vec![Receiver {
                ratchet: *signed,
                chain: ChainKey { key: chain, index: 0 },
                skipped: Vec::new()
            }],
            previous_counter: 0,
            unacknowledged: Some(UnacknowledgedPreKey {
                pre_key: bundle.pre_key.map(|(id, _)| id),
                signed_pre_key: *signed_id,
                kyber_pre_key: *kyber_id,
                kyber_ciphertext: ciphertext,
                base_key: base.public,
                created: now,
            }),
            local_registration: registration,
            remote_registration: bundle.registration,
            base_key: base.public,
            ratchet: Ratchet::new(crypto, ratchet_key, Direction::AliceToBob, limits(is_own_account))
                .ok_or(Error::Library)?,
            is_own_account,
        })
    }

    pub(super) fn accept(
        crypto: &impl Crypto,
        identity: &Pair,
        registration: u32,
        keys: &PreKeys,
        prekey: &PreKeyMessage,
        is_own_account: bool,
    ) -> Result<State, Error> {
        if !curve::is_canonical(&prekey.base_key) {
            return Err(Error::BadKey);
        }
        let shared =
            kem::kyber_decapsulate(crypto, keys.kyber_pre_key, &prekey.kyber_ciphertext).ok_or(Error::BadKey)?;
        let signed = &keys.signed_pre_key.private;
        let pairs = [(signed, &prekey.identity), (&identity.private, &prekey.base_key), (signed, &prekey.base_key)];
        let one_time = keys.pre_key.map(|one_time| (&one_time.private, &prekey.base_key));
        let [root, chain, ratchet_key] = handshake(crypto, pairs.into_iter().chain(one_time), &shared)?;
        Ok(State {
            local_identity: identity.public,
            remote_identity: prekey.identity,
            root,
            sender: *keys.signed_pre_key,
            sender_chain: ChainKey { key: chain, index: 0 },
            receivers: Vec::new(),
            previous_counter: 0,
            unacknowledged: None,
            local_registration: registration,
            remote_registration: prekey.registration,
            base_key: prekey.base_key,
            ratchet: Ratchet::new(crypto, ratchet_key, Direction::BobToAlice, limits(is_own_account))
                .ok_or(Error::Library)?,
            is_own_account,
        })
    }

    fn seal(&self, crypto: &impl Crypto, plain: &[u8], addresses: Option<&[u8]>) -> Result<(State, Vec<u8>), Error> {
        let (ratchet, ratchet_message, ratchet_key) = self.ratchet.send(crypto).ok_or(Error::Library)?;
        let (cipher_key, mac_key, iv) =
            message_keys(crypto, &self.sender_chain.seed(crypto), Some(&ratchet_key)).ok_or(Error::Library)?;
        let ciphertext = crypto.cbc_encrypt(&cipher_key, &iv, plain).ok_or(Error::Library)?;
        let outgoing = Outgoing {
            ratchet_key: &self.sender.public,
            counter: self.sender_chain.index,
            previous_counter: self.previous_counter,
            ciphertext: &ciphertext,
            ratchet: &ratchet_message,
            addresses,
        };
        let bytes = message::write_message(crypto, &outgoing, &mac_key, &self.local_identity, &self.remote_identity);
        let sender_chain = self.sender_chain.next(crypto).ok_or(Error::TooFar)?;
        Ok((State { sender_chain, ratchet, ..self.clone() }, bytes))
    }

    fn receiver_index(&mut self, crypto: &impl Crypto, ratchet_key: &[u8; 32]) -> Result<usize, Error> {
        if let Some(at) = self.receivers.iter().position(|receiver| receiver.ratchet == *ratchet_key) {
            return Ok(at);
        }
        let (root, chain) = root_step(crypto, &self.root, ratchet_key, &self.sender.private).ok_or(Error::BadKey)?;
        let sender = Pair::new(crypto).ok_or(Error::Library)?;
        let (root, sender_chain) = root_step(crypto, &root, ratchet_key, &sender.private).ok_or(Error::BadKey)?;
        self.previous_counter = self.sender_chain.index.saturating_sub(1);
        self.root = root;
        self.sender = sender;
        self.sender_chain = sender_chain;
        self.receivers.push(Receiver { ratchet: *ratchet_key, chain, skipped: Vec::new() });
        if self.receivers.len() > RECEIVER_CHAINS {
            self.receivers.remove(0);
        }
        Ok(self.receivers.len() - 1)
    }

    fn seed(&mut self, crypto: &impl Crypto, at: usize, counter: u32) -> Result<[u8; 32], Error> {
        let jump = limits(self.is_own_account).jump;
        let receiver = &mut self.receivers[at];
        receiver.chain.take_seed(crypto, counter, &mut receiver.skipped, jump)
    }

    fn open(
        &self,
        crypto: &impl Crypto,
        message: &SignalMessage,
        sender: &Address,
        recipient: &Address,
    ) -> Result<(State, Vec<u8>), Error> {
        let mut state = self.clone();
        let at = state.receiver_index(crypto, &message.ratchet_key)?;
        let seed = state.seed(crypto, at, message.counter)?;
        let (ratchet, ratchet_key) = state.ratchet.receive(crypto, &message.ratchet).ok_or(Error::Unopened)?;
        let (cipher_key, mac_key, iv) = message_keys(crypto, &seed, ratchet_key.as_ref()).ok_or(Error::Library)?;
        if !message.has_valid_mac(crypto, &mac_key, &state.remote_identity, &state.local_identity) {
            return Err(Error::Unopened);
        }
        if let Some(addresses) = &message.addresses
            && Address::pair(sender, recipient).as_deref() != Some(&addresses[..])
        {
            return Err(Error::Unopened);
        }
        if message.ciphertext.is_empty() || !message.ciphertext.len().is_multiple_of(16) {
            return Err(Error::Malformed);
        }
        let plain = crypto.cbc_decrypt(&cipher_key, &iv, &message.ciphertext).ok_or(Error::Unopened)?;
        state.ratchet = ratchet;
        state.unacknowledged = None;
        Ok((state, plain))
    }

    pub(crate) fn write(&self) -> Writer {
        let chain = |chain: &ChainKey| Writer::new().bytes(1, &chain.key).varint(2, u64::from(chain.index));
        let mut writer = Writer::new()
            .bytes(1, &self.local_identity)
            .bytes(2, &self.remote_identity)
            .bytes(3, &self.root)
            .bytes(4, &self.sender.private)
            .bytes(5, &self.sender.public)
            .message(6, chain(&self.sender_chain))
            .varint(8, u64::from(self.previous_counter))
            .varint(10, u64::from(self.local_registration))
            .varint(11, u64::from(self.remote_registration))
            .bytes(12, &self.base_key)
            .bytes(13, &self.ratchet.write())
            .bool(14, self.is_own_account);
        for receiver in &self.receivers {
            let skipped = write_skipped(&receiver.skipped);
            writer = writer.message(
                7,
                Writer::new().bytes(1, &receiver.ratchet).message(2, chain(&receiver.chain)).bytes(3, &skipped),
            );
        }
        if let Some(prekey) = &self.unacknowledged {
            let message = Writer::new()
                .maybe(prekey.pre_key.map(|id| move |w: Writer| w.varint(1, u64::from(id))))
                .varint(2, u64::from(prekey.signed_pre_key))
                .varint(3, u64::from(prekey.kyber_pre_key))
                .bytes(4, &prekey.kyber_ciphertext)
                .bytes(5, &prekey.base_key)
                .varint(6, prekey.created);
            writer = writer.message(9, message);
        }
        writer
    }

    pub(crate) fn read(message: &Message) -> Option<State> {
        let key = |message: &Message, number| -> Option<[u8; 32]> { message.bytes(number)?.try_into().ok() };
        let chain = |message: &Message| Some(ChainKey { key: key(message, 1)?, index: message.u32(2)? });
        let receivers = message
            .repeated(7)
            .map(|bytes| {
                let receiver = Message::parse(bytes)?;
                Some(Receiver {
                    ratchet: key(&receiver, 1)?,
                    chain: chain(&receiver.message(2)?)?,
                    skipped: read_skipped(receiver.bytes(3).unwrap_or_default())?,
                })
            })
            .collect::<Option<Vec<Receiver>>>()?;
        let unacknowledged = match message.message(9) {
            Some(prekey) => Some(UnacknowledgedPreKey {
                pre_key: prekey.u32(1),
                signed_pre_key: prekey.u32(2)?,
                kyber_pre_key: prekey.u32(3)?,
                kyber_ciphertext: prekey.bytes(4)?.to_vec(),
                base_key: key(&prekey, 5)?,
                created: prekey.varint(6)?,
            }),
            None => None,
        };
        Some(State {
            local_identity: key(message, 1)?,
            remote_identity: key(message, 2)?,
            root: key(message, 3)?,
            sender: Pair { private: key(message, 4)?, public: key(message, 5)? },
            sender_chain: chain(&message.message(6)?)?,
            receivers,
            previous_counter: message.u32(8)?,
            unacknowledged,
            local_registration: message.u32(10)?,
            remote_registration: message.u32(11)?,
            base_key: key(message, 12)?,
            ratchet: Ratchet::read(message.bytes(13)?)?,
            is_own_account: message.bool(14)?,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Record {
    pub(super) current: Option<State>,
    earlier: Vec<State>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Ciphertext {
    pub(super) kind: u32,
    pub(super) bytes: Vec<u8>,
}

impl Record {
    pub(super) fn promote(&mut self, state: State) {
        self.archive();
        self.current = Some(state);
    }

    pub(super) fn archive(&mut self) {
        if let Some(mut old) = self.current.take() {
            old.unacknowledged = None;
            self.earlier.insert(0, old);
            self.earlier.truncate(EARLIER_STATES);
        }
    }

    pub(super) fn sends_under(&self, ratchet: &[u8; 32]) -> bool {
        self.current.as_ref().is_some_and(|state| state.sender.public == *ratchet)
    }

    #[cfg(test)]
    pub(super) fn sending_ratchet(&self) -> Option<[u8; 32]> {
        Some(self.current.as_ref()?.sender.public)
    }

    pub(super) fn can_send(&self, now: u64) -> bool {
        self.current.as_ref().is_some_and(|state| {
            state.unacknowledged.as_ref().is_none_or(|prekey| prekey.created + UNACKNOWLEDGED_AGE >= now)
        })
    }

    pub(super) fn seal(
        &mut self,
        crypto: &impl Crypto,
        plain: &[u8],
        local: &Address,
        remote: &Address,
        now: u64,
    ) -> Result<Ciphertext, Error> {
        if !self.can_send(now) {
            return Err(Error::NoSession);
        }
        let state = self.current.as_ref().ok_or(Error::NoSession)?;
        let ciphertext = match &state.unacknowledged {
            Some(unacknowledged) => {
                let addresses = Address::pair(local, remote).ok_or(Error::Malformed)?;
                let (next, inner) = state.seal(crypto, plain, Some(&addresses))?;
                let prekey = PreKeyMessage {
                    registration: state.local_registration,
                    pre_key: unacknowledged.pre_key,
                    signed_pre_key: unacknowledged.signed_pre_key,
                    kyber_pre_key: unacknowledged.kyber_pre_key,
                    kyber_ciphertext: unacknowledged.kyber_ciphertext.clone(),
                    base_key: unacknowledged.base_key,
                    identity: state.local_identity,
                    message: inner,
                };
                self.current = Some(next);
                Ciphertext { kind: PREKEY_MESSAGE, bytes: prekey.write() }
            }
            None => {
                let (next, bytes) = state.seal(crypto, plain, None)?;
                self.current = Some(next);
                Ciphertext { kind: DOUBLE_RATCHET, bytes }
            }
        };
        Ok(ciphertext)
    }

    pub(super) fn open(
        &mut self,
        crypto: &impl Crypto,
        bytes: &[u8],
        remote: &Address,
        local: &Address,
    ) -> Result<Vec<u8>, Error> {
        let message = SignalMessage::read(bytes).ok_or(Error::Malformed)?;
        if let Some(current) = &self.current {
            match current.open(crypto, &message, remote, local) {
                Ok((state, plain)) => {
                    self.current = Some(state);
                    return Ok(plain);
                }
                Err(Error::Duplicate) => return Err(Error::Duplicate),
                Err(_) => {}
            }
        }
        for at in 0..self.earlier.len() {
            match self.earlier[at].open(crypto, &message, remote, local) {
                Ok((state, plain)) => {
                    self.earlier.remove(at);
                    self.promote(state);
                    return Ok(plain);
                }
                Err(Error::Duplicate) => return Err(Error::Duplicate),
                Err(_) => {}
            }
        }
        Err(if self.current.is_none() && self.earlier.is_empty() { Error::NoSession } else { Error::Unopened })
    }

    pub(super) fn promote_prekey_state(&mut self, prekey: &PreKeyMessage) -> Result<bool, Error> {
        let has_base_key = |state: &State| same(&state.base_key, &prekey.base_key);
        let earlier = match &self.current {
            Some(state) if has_base_key(state) => None,
            _ => match self.earlier.iter().position(has_base_key) {
                Some(at) => Some(at),
                None => return Ok(false),
            },
        };
        let known = earlier.map_or(self.current.as_ref(), |at| self.earlier.get(at));
        if !known.is_some_and(|state| same(&state.remote_identity, &prekey.identity)) {
            return Err(Error::BadKey);
        }
        if let Some(at) = earlier {
            let state = self.earlier.remove(at);
            self.promote(state);
        }
        Ok(true)
    }

    pub(super) fn open_prekey(
        &mut self,
        crypto: &impl Crypto,
        prekey: &PreKeyMessage,
        remote: &Address,
        local: &Address,
    ) -> Result<Vec<u8>, Error> {
        let message = SignalMessage::read(&prekey.message).ok_or(Error::Malformed)?;
        let current = self.current.as_ref().ok_or(Error::NoSession)?;
        let (state, plain) = current.open(crypto, &message, remote, local)?;
        self.current = Some(state);
        Ok(plain)
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let mut writer =
            Writer::new().maybe(self.current.as_ref().map(|state| |w: Writer| w.message(1, state.write())));
        for state in &self.earlier {
            writer = writer.message(2, state.write());
        }
        writer.finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Record> {
        let message = Message::parse(bytes)?;
        let current = match message.message(1) {
            Some(state) => Some(State::read(&state)?),
            None => None,
        };
        let earlier =
            message.repeated(2).map(|bytes| State::read(&Message::parse(bytes)?)).collect::<Option<Vec<State>>>()?;
        Some(Record { current, earlier })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Exact;
    use super::*;
    use crate::base::code::hex;
    use crate::base::code::unhex;

    #[test]
    fn chain_key_vector() {
        // libsignal v0.105.1, rust/protocol/src/ratchet/keys.rs, `test_chain_key_derivation`.
        let seed = unhex("8ab72d6f4cc5ac0d387eaf463378ddb28edd07385b1cb01250c715982e7ad48f").unwrap();
        let chain = ChainKey { key: seed.try_into().unwrap(), index: 0 };
        let (cipher_key, mac_key, _) = message_keys(&Exact::default(), &chain.seed(&Exact::default()), None).unwrap();
        assert_eq!(
            (hex(&cipher_key), hex(&mac_key)),
            (
                "bf51e9d75e0e31031051f82a2491ffc084fa298b7793bd9db620056febf45217".into(),
                "c6c77d6a73a354337a56435e34607dfe48e3ace14e77314dc6abc172e7a7030b".into()
            ),
            "the message keys, without the post-quantum ratchet's"
        );
        let next = chain.next(&Exact::default()).unwrap();
        assert_eq!(
            (hex(&next.key), next.index),
            ("28e8f8fee54b801eef7c5cfb2f17f32c7b334485bbb70fac6ec10342a246d15d".into(), 1)
        );
        let mut key = chain;
        assert_eq!(
            key.take_seed(&Exact::default(), 1, &mut Vec::new(), MAX_JUMP),
            Ok(next.seed(&Exact::default())),
            "the second message's seed is the next key's"
        );
    }

    #[test]
    fn seeds_passed_over_kept() {
        let crypto = Exact::default();
        // A put at the front for each step, as libsignal keeps them (`set_message_keys`).
        let by_steps = |mut chain: ChainKey, counter: u32, mut skipped: Vec<(u32, [u8; 32])>| {
            while chain.index < counter {
                skipped.insert(0, (chain.index, chain.seed(&crypto)));
                skipped.truncate(SKIPPED_KEYS);
                chain = chain.next(&crypto).unwrap();
            }
            (chain.seed(&crypto), chain.next(&crypto).unwrap(), skipped)
        };
        let start = ChainKey { key: [7; 32], index: 3 };
        let held = alloc::vec![(2, [2; 32]), (0, [0; 32])];
        let most = SKIPPED_KEYS as u32;
        for jump in [0, 1, most - 2, most, most + 500] {
            let (mut chain, mut skipped) = (start, held.clone());
            let seed = chain.take_seed(&crypto, start.index + jump, &mut skipped, MAX_JUMP);
            let (expected_seed, expected_chain, expected) = by_steps(start, start.index + jump, held.clone());
            assert!(
                seed == Ok(expected_seed) && chain == expected_chain && skipped == expected,
                "a jump of {jump}: the newest {SKIPPED_KEYS} seeds, newest first, as a put for each step leaves them"
            );
        }
    }

    #[test]
    fn bundle_signatures() {
        let crypto = Exact::default();
        let pair = || Pair::new(&crypto).unwrap();
        let (own, identity, other, signed) = (pair(), pair(), pair(), pair());
        let (_, kyber) = crypto.lattice_pair().unwrap();
        let by = |signer: &Pair, key: &[u8]| curve::sign(&crypto, &signer.private, &[key], &[0; 64]);
        let begin = |signer: &Pair, kyber_signer: &Pair| {
            let bundle = Bundle {
                registration: 7,
                identity: identity.public,
                pre_key: None,
                signed_pre_key: (2, signed.public, by(signer, &message::encode_key(&signed.public))),
                kyber_pre_key: (3, kyber.clone(), by(kyber_signer, &message::encode_kyber(&kyber))),
            };
            State::begin(&crypto, &own, 11, &bundle, false, 0).map(|state| state.remote_identity)
        };
        assert_eq!(begin(&identity, &identity), Ok(identity.public), "both pre-keys signed by the bundle's identity");
        assert_eq!(begin(&other, &identity), Err(Error::BadKey), "the signed pre-key by another key");
        assert_eq!(begin(&identity, &other), Err(Error::BadKey), "the Kyber pre-key by another key");
    }

    #[test]
    fn prekey_state_promotion() {
        let crypto = Exact::default();
        let pair = || Pair::new(&crypto).unwrap();
        let (own, identity) = (pair(), pair());
        let (_, kyber) = crypto.lattice_pair().unwrap();
        let by = |key: &[u8]| curve::sign(&crypto, &identity.private, &[key], &[0; 64]);
        let state = || {
            let signed = pair();
            let bundle = Bundle {
                registration: 7,
                identity: identity.public,
                pre_key: None,
                signed_pre_key: (2, signed.public, by(&message::encode_key(&signed.public))),
                kyber_pre_key: (3, kyber.clone(), by(&message::encode_kyber(&kyber))),
            };
            State::begin(&crypto, &own, 11, &bundle, false, 0).unwrap()
        };
        let (first, second) = (state(), state());
        let mut record = Record::default();
        record.promote(first.clone());
        record.promote(second.clone());
        let prekey = |base_key, identity| PreKeyMessage {
            registration: 7,
            pre_key: None,
            signed_pre_key: 2,
            kyber_pre_key: 3,
            kyber_ciphertext: Vec::new(),
            base_key,
            identity,
            message: Vec::new(),
        };
        let copy = record.clone();
        assert_eq!(record.promote_prekey_state(&prekey([9; 32], identity.public)), Ok(false), "a base key not held");
        assert_eq!(
            record.promote_prekey_state(&prekey(first.base_key, own.public)),
            Err(Error::BadKey),
            "an earlier state's base key under another identity"
        );
        assert_eq!(record, copy, "neither changes anything");
        assert_eq!(record.promote_prekey_state(&prekey(second.base_key, identity.public)), Ok(true));
        assert_eq!(record, copy, "the current state stays where it is");
        assert_eq!(record.promote_prekey_state(&prekey(first.base_key, identity.public)), Ok(true));
        assert_eq!(
            record.current.as_ref().map(|state| state.base_key),
            Some(first.base_key),
            "an earlier state is made the current one"
        );
    }
}
