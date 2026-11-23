// The pre-keys, on Signal-Desktop's schedule.

use super::Crypto;
use super::curve::{self, Pair};
use super::message::{encode_key, encode_kyber};
use crate::base::code::base64;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const ID_MASK: u32 = 0x00FF_FFFF;
const BATCH: usize = 100;
pub(super) const MIN_COUNT: u32 = 10;
pub(super) const MAX_COUNT: u32 = 200;
pub(super) const CHECK_PERIOD: u64 = 2 * 24 * 60 * 60 * 1000;
pub(super) const RENEW_AGE: u64 = 36 * 60 * 60 * 1000;
pub(super) const CHECK_RETRY: u64 = 5 * 60 * 1000;
pub(super) const MAX_CONFIRMED_AGE: u64 = 14 * 24 * 60 * 60 * 1000;
const NEWEST_KEYS: usize = 5;
pub(super) const QUEUE_TIME: u64 = 30 * 24 * 60 * 60 * 1000;
const UNUSED_AGE: u64 = 90 * 24 * 60 * 60 * 1000;

fn next_id(id: u32) -> u32 {
    ((id + 1) & ID_MASK).max(1)
}

fn first_id(crypto: &impl Crypto) -> Option<u32> {
    let mut bytes = [0u8; 4];
    crypto.random(&mut bytes).then_some(())?;
    Some((u32::from_le_bytes(bytes) & ID_MASK).max(1))
}

fn signed_json(id: u32, public: &[u8], signature: &[u8; 64]) -> String {
    format!(r#"{{"keyId":{id},"publicKey":"{}","signature":"{}"}}"#, base64(public), base64(signature))
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct SignedKey {
    pub(super) id: u32,
    pub(super) pair: Pair,
    pub(super) signature: [u8; 64],
    pub(super) created: u64,
    pub(super) confirmed: Option<u64>,
}

impl SignedKey {
    pub(super) fn new(crypto: &impl Crypto, identity: &Pair, id: u32, created: u64) -> Option<SignedKey> {
        let pair = Pair::new(crypto)?;
        let signature = curve::sign_fresh(crypto, &identity.private, &[&encode_key(&pair.public)])?;
        Some(SignedKey { id, pair, signature, created, confirmed: None })
    }

    pub(super) fn to_json(&self) -> String {
        signed_json(self.id, &encode_key(&self.pair.public), &self.signature)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct KyberKey {
    pub(super) id: u32,
    pub(super) private: Vec<u8>,
    pub(super) public: Vec<u8>,
    pub(super) signature: [u8; 64],
    pub(super) created: u64,
    pub(super) confirmed: Option<u64>,
}

impl core::fmt::Debug for KyberKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KyberKey").field("id", &self.id).field("made", &self.created).finish_non_exhaustive()
    }
}

impl KyberKey {
    pub(super) fn new(crypto: &impl Crypto, identity: &Pair, id: u32, created: u64) -> Option<KyberKey> {
        let (private, public) = crypto.lattice_pair()?;
        let signature = curve::sign_fresh(crypto, &identity.private, &[&encode_kyber(&public)])?;
        Some(KyberKey { id, private, public, signature, created, confirmed: None })
    }

    pub(super) fn to_json(&self) -> String {
        signed_json(self.id, &encode_kyber(&self.public), &self.signature)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct OneTimeKey {
    pub(super) id: u32,
    pub(super) pair: Pair,
    pub(super) created: u64,
}

impl OneTimeKey {
    fn to_json(&self) -> String {
        format!(r#"{{"keyId":{},"publicKey":"{}"}}"#, self.id, base64(&encode_key(&self.pair.public)))
    }
}

fn one_time_batch(crypto: &impl Crypto, first: u32, created: u64) -> Option<Vec<OneTimeKey>> {
    let mut id = first;
    (0..BATCH)
        .map(|_| {
            let key = OneTimeKey { id, pair: Pair::new(crypto)?, created };
            id = next_id(id);
            Some(key)
        })
        .collect()
}

fn kyber_batch(crypto: &impl Crypto, identity: &Pair, first: u32, created: u64) -> Option<Vec<KyberKey>> {
    let mut id = first;
    (0..BATCH)
        .map(|_| {
            let key = KyberKey::new(crypto, identity, id, created)?;
            id = next_id(id);
            Some(key)
        })
        .collect()
}

pub(super) fn write_upload(
    one_time: &[OneTimeKey],
    kyber: &[KyberKey],
    signed: Option<&SignedKey>,
    last_resort: Option<&KyberKey>,
) -> String {
    let mut members = Vec::new();
    if !one_time.is_empty() {
        members
            .push(format!(r#""preKeys":[{}]"#, one_time.iter().map(OneTimeKey::to_json).collect::<Vec<_>>().join(",")));
    }
    if !kyber.is_empty() {
        members.push(format!(r#""pqPreKeys":[{}]"#, kyber.iter().map(KyberKey::to_json).collect::<Vec<_>>().join(",")));
    }
    if let Some(signed) = signed {
        members.push(format!(r#""signedPreKey":{}"#, signed.to_json()));
    }
    if let Some(last_resort) = last_resort {
        members.push(format!(r#""pqLastResortPreKey":{}"#, last_resort.to_json()));
    }
    format!("{{{}}}", members.join(","))
}

const fn is_older(at: u64, age: u64, now: u64) -> bool {
    at.saturating_add(age) <= now
}

pub(super) fn drop_replaced<T>(keys: &mut Vec<T>, created: impl Fn(&T) -> u64, now: u64) {
    let mut at = 0;
    keys.retain(|key| {
        at += 1;
        at <= NEWEST_KEYS || !is_older(created(key), QUEUE_TIME, now)
    });
}

pub(super) fn drop_unused<T>(keys: &mut Vec<T>, created: impl Fn(&T) -> u64, now: u64) {
    let mut newest: Vec<usize> = (0..keys.len()).collect();
    newest.sort_by_key(|at| core::cmp::Reverse(created(&keys[*at])));
    let mut is_retained = alloc::vec![false; keys.len()];
    for (rank, at) in newest.into_iter().enumerate() {
        is_retained[at] = rank < MAX_COUNT as usize || !is_older(created(&keys[at]), UNUSED_AGE, now);
    }
    let mut at = 0;
    keys.retain(|_| {
        at += 1;
        is_retained[at - 1]
    });
}

pub(super) fn is_out_of_date(confirmed: impl IntoIterator<Item = Option<u64>>, now: u64) -> bool {
    confirmed.into_iter().flatten().max().is_none_or(|at| is_older(at, MAX_CONFIRMED_AGE, now))
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct Ring {
    pub(super) signed_keys: Vec<SignedKey>,
    pub(super) last_resort: Vec<KyberKey>,
    pub(super) one_time: Vec<OneTimeKey>,
    pub(super) kyber: Vec<KyberKey>,
    pub(super) next_signed: u32,
    next_one_time: u32,
    pub(super) next_kyber: u32,
    pub(super) checked: u64,
    pub(super) uses: Vec<(u32, u32, [u8; 32])>,
}

impl core::fmt::Debug for Ring {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ring")
            .field("next_signed", &self.next_signed)
            .field("next_once", &self.next_one_time)
            .field("next_kyber", &self.next_kyber)
            .field("checked", &self.checked)
            .finish_non_exhaustive()
    }
}

impl Ring {
    pub(super) fn generate(crypto: &impl Crypto, identity: &Pair, now: u64) -> Option<Ring> {
        let mut ring = Ring {
            next_signed: first_id(crypto)?,
            next_one_time: first_id(crypto)?,
            next_kyber: first_id(crypto)?,
            checked: now,
            ..Ring::default()
        };
        ring.signed_keys.push(SignedKey::new(crypto, identity, Ring::take_id(&mut ring.next_signed), now)?);
        ring.last_resort.push(KyberKey::new(crypto, identity, Ring::take_id(&mut ring.next_kyber), now)?);
        ring.top_up_one_time(crypto, now)?;
        ring.top_up_kyber(crypto, identity, now)?;
        Some(ring)
    }

    pub(super) fn from_keys(crypto: &impl Crypto, signed: SignedKey, last_resort: Option<KyberKey>) -> Option<Ring> {
        Some(Ring {
            next_signed: next_id(signed.id),
            next_one_time: first_id(crypto)?,
            next_kyber: match &last_resort {
                Some(key) => next_id(key.id),
                None => first_id(crypto)?,
            },
            signed_keys: alloc::vec![signed],
            last_resort: last_resort.into_iter().collect(),
            ..Ring::default()
        })
    }

    pub(super) fn top_up_one_time(&mut self, crypto: &impl Crypto, now: u64) -> Option<usize> {
        let batch = one_time_batch(crypto, self.next_one_time, now)?;
        let start = self.one_time.len();
        self.next_one_time = batch.last().map_or(self.next_one_time, |key| next_id(key.id));
        self.one_time.extend(batch);
        Some(start)
    }

    pub(super) fn top_up_kyber(&mut self, crypto: &impl Crypto, identity: &Pair, now: u64) -> Option<usize> {
        let batch = kyber_batch(crypto, identity, self.next_kyber, now)?;
        let start = self.kyber.len();
        self.next_kyber = batch.last().map_or(self.next_kyber, |key| next_id(key.id));
        self.kyber.extend(batch);
        Some(start)
    }

    pub(super) fn confirm(&mut self, (signed_id, last_resort_id): (Option<u32>, Option<u32>), now: u64) {
        let signed = self.signed_keys.iter_mut().filter(|key| Some(key.id) == signed_id).map(|key| &mut key.confirmed);
        let last_resort =
            self.last_resort.iter_mut().filter(|key| Some(key.id) == last_resort_id).map(|key| &mut key.confirmed);
        signed.chain(last_resort).for_each(|confirmed| *confirmed = Some(now));
    }

    pub(super) fn take_id(next: &mut u32) -> u32 {
        let id = *next;
        *next = next_id(id);
        id
    }

    pub(super) fn signed_key(&self, id: u32) -> Option<&SignedKey> {
        self.signed_keys.iter().find(|key| key.id == id)
    }

    pub(super) fn kyber_key(&self, id: u32) -> Option<(&KyberKey, bool)> {
        self.kyber
            .iter()
            .find(|key| key.id == id)
            .map(|key| (key, true))
            .or_else(|| self.last_resort.iter().find(|key| key.id == id).map(|key| (key, false)))
    }

    pub(super) fn one_time_key(&self, id: u32) -> Option<&OneTimeKey> {
        self.one_time.iter().find(|key| key.id == id)
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        // When the service took a key, nought for one it has not.
        let write_signed = |key: &SignedKey| {
            Writer::new()
                .varint(1, u64::from(key.id))
                .bytes(2, &key.pair.private)
                .bytes(3, &key.pair.public)
                .bytes(4, &key.signature)
                .varint(5, key.created)
                .varint(6, key.confirmed.unwrap_or(0))
        };
        let write_kyber = |key: &KyberKey| {
            Writer::new()
                .varint(1, u64::from(key.id))
                .bytes(2, &key.private)
                .bytes(3, &key.public)
                .bytes(4, &key.signature)
                .varint(5, key.created)
                .varint(6, key.confirmed.unwrap_or(0))
        };
        let mut writer = Writer::new()
            .varint(5, u64::from(self.next_signed))
            .varint(6, u64::from(self.next_one_time))
            .varint(7, u64::from(self.next_kyber))
            .varint(8, self.checked);
        for key in &self.signed_keys {
            writer = writer.message(1, write_signed(key));
        }
        for key in &self.last_resort {
            writer = writer.message(2, write_kyber(key));
        }
        for key in &self.one_time {
            writer = writer.message(
                3,
                Writer::new()
                    .varint(1, u64::from(key.id))
                    .bytes(2, &key.pair.private)
                    .bytes(3, &key.pair.public)
                    .varint(4, key.created),
            );
        }
        for key in &self.kyber {
            writer = writer.message(4, write_kyber(key));
        }
        for (kyber, signed, base) in &self.uses {
            writer = writer
                .message(9, Writer::new().varint(1, u64::from(*kyber)).varint(2, u64::from(*signed)).bytes(3, base));
        }
        writer.finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Ring> {
        let message = Message::parse(bytes)?;
        let pair = |key: &Message| -> Option<Pair> {
            Some(Pair { private: key.bytes(2)?.try_into().ok()?, public: key.bytes(3)?.try_into().ok()? })
        };
        let confirmed = |key: &Message, created: u64, last_resort: bool| match key.varint(6) {
            None => last_resort.then_some(created),
            Some(0) => None,
            Some(at) => Some(at),
        };
        let kyber = |bytes: &[u8], last_resort: bool| -> Option<KyberKey> {
            let key = Message::parse(bytes)?;
            let created = key.varint(5)?;
            Some(KyberKey {
                id: key.u32(1)?,
                private: key.bytes(2)?.to_vec(),
                public: key.bytes(3)?.to_vec(),
                signature: key.bytes(4)?.try_into().ok()?,
                created,
                confirmed: confirmed(&key, created, last_resort),
            })
        };
        Some(Ring {
            signed_keys: message
                .repeated(1)
                .map(|bytes| {
                    let key = Message::parse(bytes)?;
                    let created = key.varint(5)?;
                    Some(SignedKey {
                        id: key.u32(1)?,
                        pair: pair(&key)?,
                        signature: key.bytes(4)?.try_into().ok()?,
                        created,
                        confirmed: confirmed(&key, created, true),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            last_resort: message.repeated(2).map(|bytes| kyber(bytes, true)).collect::<Option<Vec<_>>>()?,
            one_time: message
                .repeated(3)
                .map(|bytes| {
                    let key = Message::parse(bytes)?;
                    Some(OneTimeKey { id: key.u32(1)?, pair: pair(&key)?, created: key.varint(4).unwrap_or(0) })
                })
                .collect::<Option<Vec<_>>>()?,
            kyber: message.repeated(4).map(|bytes| kyber(bytes, false)).collect::<Option<Vec<_>>>()?,
            next_signed: message.u32(5)?,
            next_one_time: message.u32(6)?,
            next_kyber: message.u32(7)?,
            checked: message.varint(8).unwrap_or(0),
            uses: message
                .repeated(9)
                .map(|bytes| {
                    let key_use = Message::parse(bytes)?;
                    Some((key_use.u32(1)?, key_use.u32(2)?, key_use.bytes(3)?.try_into().ok()?))
                })
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_wrap() {
        assert_eq!(next_id(5), 6);
        assert_eq!(next_id(ID_MASK), 1);
        assert_eq!(next_id(ID_MASK - 1), ID_MASK);
    }

    #[test]
    fn replaced_key_retention() {
        let now = 100 * QUEUE_TIME;
        let mut times = alloc::vec![now, now - 1, now - 2, now - 3, now - 4, 0, now - QUEUE_TIME + 1, now - QUEUE_TIME];
        drop_replaced(&mut times, |time| *time, now);
        assert_eq!(times, [now, now - 1, now - 2, now - 3, now - 4, now - QUEUE_TIME + 1]);
    }

    #[test]
    fn unused_key_retention() {
        let now = 10 * UNUSED_AGE;
        let mut times: Vec<u64> = alloc::vec![0, now - UNUSED_AGE, now - UNUSED_AGE + 1];
        times.extend(core::iter::repeat_n(now, MAX_COUNT as usize));
        drop_unused(&mut times, |time| *time, now);
        assert_eq!(
            times.len(),
            MAX_COUNT as usize + 1,
            "the newest two hundred kept, and past them one younger than ninety days"
        );
        assert_eq!(times[0], now - UNUSED_AGE + 1, "in the order they were made");
        let mut few: Vec<u64> = alloc::vec![0, 1, 2];
        drop_unused(&mut few, |time| *time, now);
        assert_eq!(few, [0, 1, 2], "no more than the newest two hundred: all kept, however old");
    }

    #[test]
    fn out_of_date_keys() {
        let now = 10 * MAX_CONFIRMED_AGE;
        assert!(
            !is_out_of_date([None, Some(now - MAX_CONFIRMED_AGE + 1), Some(0)], now),
            "an unconfirmed key is passed over"
        );
        assert!(is_out_of_date([Some(now - MAX_CONFIRMED_AGE)], now));
        assert!(is_out_of_date([None, None], now), "none confirmed");
    }

    #[test]
    fn upload_fields() {
        let one_time = [OneTimeKey { id: 3, pair: Pair { private: [1; 32], public: [2; 32] }, created: 0 }];
        let body = write_upload(&one_time, &[], None, None);
        assert!(body.starts_with(r#"{"preKeys":[{"keyId":3,"publicKey":"BQICAgIC"#), "{body}");
        assert_eq!(write_upload(&[], &[], None, None), "{}");
    }
}
