// Sender keys, as libsignal has them.

use super::curve::{self, Pair};
use super::message::{decode_key, encode_key};
use super::session::{ChainKey, Error, MAX_JUMP};
use super::{Crypto, hkdf, read_skipped, write_skipped};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::vec::Vec;

const VERSION: u8 = 3;
const CHAINS: usize = 5;
const MESSAGE_KEYS: &[u8] = b"WhisperGroup";
const SIGNATURE_BYTES: usize = 64;
const IV_BYTES: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Distribution {
    pub(super) id: [u8; 16],
    chain: u32,
    iteration: u32,
    chain_key: [u8; 32],
    signing_key: [u8; 32],
}

impl core::fmt::Debug for Distribution {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Distribution")
    }
}

impl Distribution {
    pub(super) fn read(bytes: &[u8]) -> Option<Distribution> {
        let (&version, rest) = bytes.split_first()?;
        if version >> 4 != VERSION {
            return None;
        }
        let message = Message::parse(rest)?;
        Some(Distribution {
            id: message.bytes(1)?.try_into().ok()?,
            chain: message.u32(2)?,
            iteration: message.u32(3)?,
            chain_key: message.bytes(4)?.try_into().ok()?,
            signing_key: decode_key(message.bytes(5)?)?,
        })
    }
}

struct SenderKeyMessage {
    id: [u8; 16],
    chain: u32,
    iteration: u32,
    ciphertext: Vec<u8>,
    signed_bytes: Vec<u8>,
    signature: [u8; SIGNATURE_BYTES],
}

impl SenderKeyMessage {
    fn read(bytes: &[u8]) -> Option<SenderKeyMessage> {
        if bytes.len() <= 1 + SIGNATURE_BYTES || bytes[0] >> 4 != VERSION {
            return None;
        }
        let (signed_bytes, signature) = bytes.split_at(bytes.len() - SIGNATURE_BYTES);
        let message = Message::parse(&signed_bytes[1..])?;
        Some(SenderKeyMessage {
            id: message.bytes(1)?.try_into().ok()?,
            chain: message.u32(2)?,
            iteration: message.u32(3)?,
            ciphertext: message.bytes(4)?.to_vec(),
            signed_bytes: signed_bytes.to_vec(),
            signature: signature.try_into().ok()?,
        })
    }
}

pub(super) fn parse_distribution_id(bytes: &[u8]) -> Option<[u8; 16]> {
    SenderKeyMessage::read(bytes).map(|message| message.id)
}

#[derive(Clone, PartialEq, Eq)]
struct Chain {
    id: u32,
    key: ChainKey,
    signing_key: [u8; 32],
    private: Option<[u8; 32]>,
    skipped: Vec<(u32, [u8; 32])>,
}

const CHAIN_ID_SHIFT: u32 = 1;

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct Record {
    chains: Vec<Chain>,
}

impl core::fmt::Debug for Record {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Record").field("chains", &self.chains.len()).finish_non_exhaustive()
    }
}

impl Record {
    pub(super) fn add_distribution(&mut self, distribution: &Distribution) {
        let existing = self
            .chains
            .iter()
            .position(|chain| chain.id == distribution.chain && chain.signing_key == distribution.signing_key)
            .map(|at| self.chains.remove(at));
        self.chains.retain(|chain| chain.id != distribution.chain);
        let chain = existing.unwrap_or_else(|| Chain {
            id: distribution.chain,
            key: ChainKey { key: distribution.chain_key, index: distribution.iteration },
            signing_key: distribution.signing_key,
            private: None,
            skipped: Vec::new(),
        });
        self.chains.insert(0, chain);
        self.chains.truncate(CHAINS);
    }

    pub(super) fn open(&mut self, crypto: &impl Crypto, bytes: &[u8]) -> Result<Vec<u8>, Error> {
        let message = SenderKeyMessage::read(bytes).ok_or(Error::Malformed)?;
        let at = self.chains.iter().position(|chain| chain.id == message.chain).ok_or(Error::NoSession)?;
        let mut chain = self.chains[at].clone();
        if !curve::verify(crypto, &chain.signing_key, &[&message.signed_bytes], &message.signature) {
            return Err(Error::BadKey);
        }
        let seed = chain.key.take_seed(crypto, message.iteration, &mut chain.skipped, MAX_JUMP)?;
        let keys: [u8; IV_BYTES + 32] = hkdf(crypto, None, &seed, &[MESSAGE_KEYS]).ok_or(Error::Library)?;
        let (iv, key) = keys.split_at(IV_BYTES);
        if message.ciphertext.is_empty() || !message.ciphertext.len().is_multiple_of(IV_BYTES) {
            return Err(Error::Malformed);
        }
        let iv = iv.try_into().expect("sixteen");
        let plain = crypto.cbc_decrypt(key.try_into().expect("thirty-two"), iv, &message.ciphertext);
        let plain = plain.ok_or(Error::Unopened)?;
        self.chains[at] = chain;
        Ok(plain)
    }

    pub(super) fn generate(crypto: &impl Crypto) -> Option<Record> {
        let mut bytes = [0u8; 4 + 32];
        crypto.random(&mut bytes).then_some(())?;
        let (id, key) = bytes.split_at(4);
        let signing_pair = Pair::new(crypto)?;
        let chain = Chain {
            id: u32::from_le_bytes(id.try_into().ok()?) >> CHAIN_ID_SHIFT,
            key: ChainKey { key: key.try_into().ok()?, index: 0 },
            signing_key: signing_pair.public,
            private: Some(signing_pair.private),
            skipped: Vec::new(),
        };
        Some(Record { chains: alloc::vec![chain] })
    }

    pub(super) fn distribution(&self, id: &[u8; 16]) -> Option<Vec<u8>> {
        let chain = self.chains.first().filter(|chain| chain.private.is_some())?;
        let message = Writer::new()
            .bytes(1, id)
            .varint(2, u64::from(chain.id))
            .varint(3, u64::from(chain.key.index))
            .bytes(4, &chain.key.key)
            .bytes(5, &encode_key(&chain.signing_key))
            .finish();
        Some([&[VERSION << 4 | VERSION][..], &message].concat())
    }

    pub(super) fn encrypt(&mut self, crypto: &impl Crypto, id: &[u8; 16], padded: &[u8]) -> Result<Vec<u8>, Error> {
        let chain = self.chains.first_mut().ok_or(Error::NoSession)?;
        let private = chain.private.ok_or(Error::NoSession)?;
        let next = chain.key.next(crypto).ok_or(Error::TooFar)?;
        let keys: [u8; IV_BYTES + 32] =
            hkdf(crypto, None, &chain.key.seed(crypto), &[MESSAGE_KEYS]).ok_or(Error::Library)?;
        let (iv, key) = keys.split_at(IV_BYTES);
        let iv = iv.try_into().expect("sixteen");
        let ciphertext = crypto.cbc_encrypt(key.try_into().expect("thirty-two"), iv, padded).ok_or(Error::Library)?;
        let message = Writer::new()
            .bytes(1, id)
            .varint(2, u64::from(chain.id))
            .varint(3, u64::from(chain.key.index))
            .bytes(4, &ciphertext)
            .finish();
        let signed_bytes = [&[VERSION << 4 | VERSION][..], &message].concat();
        let signature = curve::sign_fresh(crypto, &private, &[&signed_bytes]).ok_or(Error::Library)?;
        chain.key = next;
        Ok([signed_bytes, signature.to_vec()].concat())
    }

    pub(super) fn write(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        for chain in &self.chains {
            writer = writer.message(
                1,
                Writer::new()
                    .varint(1, u64::from(chain.id))
                    .bytes(2, &chain.key.key)
                    .varint(3, u64::from(chain.key.index))
                    .bytes(4, &chain.signing_key)
                    .bytes(5, &write_skipped(&chain.skipped))
                    .maybe(chain.private.map(|private| move |w: Writer| w.bytes(6, &private))),
            );
        }
        writer.finish()
    }

    pub(super) fn read(bytes: &[u8]) -> Option<Record> {
        let message = Message::parse(bytes)?;
        let chains = message
            .repeated(1)
            .map(|bytes| {
                let chain = Message::parse(bytes)?;
                Some(Chain {
                    id: chain.u32(1)?,
                    key: ChainKey { key: chain.bytes(2)?.try_into().ok()?, index: chain.u32(3)? },
                    signing_key: chain.bytes(4)?.try_into().ok()?,
                    private: chain.bytes(6).and_then(|private| private.try_into().ok()),
                    skipped: read_skipped(chain.bytes(5).unwrap_or_default())?,
                })
            })
            .collect::<Option<Vec<Chain>>>()?;
        Some(Record { chains })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::Standin;
    use super::*;

    pub(in super::super) fn distribution(id: [u8; 16], chain: u32, key: &ChainKey, signing: &Pair) -> Vec<u8> {
        let message = Writer::new()
            .bytes(1, &id)
            .varint(2, u64::from(chain))
            .varint(3, u64::from(key.index))
            .bytes(4, &key.key)
            .bytes(5, &encode_key(&signing.public))
            .finish();
        [&[VERSION << 4 | VERSION][..], &message].concat()
    }

    pub(in super::super) fn seal(
        crypto: &impl Crypto,
        (id, chain): ([u8; 16], u32),
        key: &mut ChainKey,
        signing: &Pair,
        plain: &[u8],
    ) -> Vec<u8> {
        let iteration = key.index;
        let keys: [u8; IV_BYTES + 32] = hkdf(crypto, None, &key.seed(crypto), &[MESSAGE_KEYS]).unwrap();
        if let Some(next) = key.next(crypto) {
            *key = next;
        }
        let (iv, cipher_key) = keys.split_at(IV_BYTES);
        let ciphertext = crypto.cbc_encrypt(cipher_key.try_into().unwrap(), iv.try_into().unwrap(), plain).unwrap();
        let message = Writer::new()
            .bytes(1, &id)
            .varint(2, u64::from(chain))
            .varint(3, u64::from(iteration))
            .bytes(4, &ciphertext)
            .finish();
        let signed_bytes = [&[VERSION << 4 | VERSION][..], &message].concat();
        let signature = curve::sign(crypto, &signing.private, &[&signed_bytes], &[0; 64]);
        [signed_bytes, signature.to_vec()].concat()
    }

    #[test]
    fn open_in_any_order() {
        let crypto = Standin::default();
        let signing = Pair::new(&crypto).unwrap();
        let mut sender_key = ChainKey { key: [7; 32], index: 4 };
        let id = [9; 16];
        let parsed = Distribution::read(&distribution(id, 42, &sender_key, &signing)).expect("a distribution");
        assert_eq!((parsed.id, parsed.chain, parsed.iteration), (id, 42, 4));
        let mut record = Record::default();
        let first = seal(&crypto, (id, 42), &mut sender_key, &signing, b"first");
        assert_eq!(record.open(&crypto, &first), Err(Error::NoSession), "no chain before its distribution");
        record.add_distribution(&parsed);
        let second = seal(&crypto, (id, 42), &mut sender_key, &signing, b"second");
        assert_eq!(parse_distribution_id(&second), Some(id));
        assert_eq!(record.open(&crypto, &second).as_deref(), Ok(&b"second"[..]), "one ahead, the first skipped");
        assert_eq!(record.open(&crypto, &first).as_deref(), Ok(&b"first"[..]), "the skipped one later");
        assert_eq!(record.open(&crypto, &first), Err(Error::Duplicate), "and once");
        assert_eq!(Record::read(&record.write()).as_ref(), Some(&record), "a record reads back as written");
        record.add_distribution(&parsed);
        let third = seal(&crypto, (id, 42), &mut sender_key, &signing, b"third");
        assert_eq!(
            (record.open(&crypto, &third).as_deref(), record.open(&crypto, &second)),
            (Ok(&b"third"[..]), Err(Error::Duplicate)),
            "a distribution sent again sets nothing back"
        );
    }

    #[test]
    fn foreign_signature_refused() {
        let crypto = super::super::tests::Exact::default();
        let (signing, other) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let key = ChainKey { key: [7; 32], index: 0 };
        let id = [9; 16];
        let mut record = Record::default();
        record.add_distribution(&Distribution::read(&distribution(id, 1, &key, &signing)).unwrap());
        let copy = record.clone();
        let forged = seal(&crypto, (id, 1), &mut key.clone(), &other, b"padded");
        assert_eq!(record.open(&crypto, &forged), Err(Error::BadKey), "signed by another key than the chain's");
        assert_eq!(record, copy, "a refusal changes nothing");
        let message = seal(&crypto, (id, 1), &mut key.clone(), &signing, b"padded");
        assert_eq!(record.open(&crypto, &message).as_deref(), Ok(&b"padded"[..]), "signed by the chain's key");
    }

    #[test]
    fn last_iteration_refused() {
        let crypto = Standin::default();
        let signing = Pair::new(&crypto).unwrap();
        let mut sender_key = ChainKey { key: [7; 32], index: u32::MAX - 1 };
        let id = [9; 16];
        let mut record = Record::default();
        record.add_distribution(&Distribution::read(&distribution(id, 1, &sender_key, &signing)).unwrap());
        let before_last = seal(&crypto, (id, 1), &mut sender_key, &signing, b"one before");
        let last = seal(&crypto, (id, 1), &mut sender_key, &signing, b"last");
        let copy = record.clone();
        assert_eq!(record.open(&crypto, &last), Err(Error::TooFar), "no key comes after it");
        assert_eq!(record, copy, "a refusal changes nothing");
        assert_eq!(record.open(&crypto, &before_last).as_deref(), Ok(&b"one before"[..]));
    }

    #[test]
    fn own_chain() {
        let crypto = Standin::default();
        let id = [3; 16];
        let mut own = Record::generate(&crypto).expect("a record");
        let chain = &own.chains[0];
        assert!(
            chain.id < 1 << 31 && chain.key.index == 0 && chain.private.is_some(),
            "an id of 31 bits, iteration nought"
        );
        let bytes = own.distribution(&id).expect("its distribution message");
        let parsed = Distribution::read(&bytes).expect("read as a member's distribution message is");
        assert_eq!(
            (parsed.id, parsed.chain, parsed.iteration, parsed.signing_key),
            (id, chain.id, 0, chain.signing_key)
        );
        let mut member = Record::default();
        member.add_distribution(&parsed);
        assert_eq!(member.distribution(&id), None, "a member's record is no own one");
        let first = own.encrypt(&crypto, &id, b"padded one").unwrap();
        let second = own.encrypt(&crypto, &id, b"padded two").unwrap();
        assert_eq!((first[0], parse_distribution_id(&second)), (0x33, Some(id)), "version 3, of the distribution");
        assert_eq!(member.open(&crypto, &second).as_deref(), Ok(&b"padded two"[..]), "the second, the first skipped");
        assert_eq!(member.open(&crypto, &first).as_deref(), Ok(&b"padded one"[..]));
        let record = Record::read(&own.write()).expect("read back");
        assert_eq!(record, own, "the private key kept");
        assert_eq!(
            record.distribution(&id).map(|bytes| Distribution::read(&bytes).unwrap().iteration),
            Some(2),
            "it stands two on"
        );
        own.chains[0].key.index = u32::MAX;
        let copy = own.clone();
        assert_eq!(own.encrypt(&crypto, &id, b"last"), Err(Error::TooFar), "the chain's last key is not used");
        assert_eq!(own, copy, "and nothing changed");
        assert_eq!(member.clone().encrypt(&crypto, &id, b"x"), Err(Error::NoSession), "only an own record encrypts");
    }

    #[test]
    fn newest_chains() {
        let crypto = Standin::default();
        let signing = Pair::new(&crypto).unwrap();
        let other = Pair::new(&crypto).unwrap();
        let key = ChainKey { key: [1; 32], index: 0 };
        let mut record = Record::default();
        for chain in 0..=CHAINS as u32 {
            record.add_distribution(&Distribution::read(&distribution([1; 16], chain, &key, &signing)).unwrap());
        }
        assert_eq!(
            record.chains.iter().map(|chain| chain.id).collect::<Vec<_>>(),
            [5, 4, 3, 2, 1],
            "the newest first, the oldest let go"
        );
        record.add_distribution(&Distribution::read(&distribution([1; 16], 3, &key, &other)).unwrap());
        assert_eq!(
            record.chains.iter().map(|chain| (chain.id, chain.signing_key == other.public)).collect::<Vec<_>>(),
            [(3, true), (5, false), (4, false), (2, false), (1, false)],
            "a chain of an id held under another key gives way to the new one"
        );
    }
}
