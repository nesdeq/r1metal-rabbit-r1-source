// The Sparse Post-Quantum Ratchet, version 1: the key of each message is mixed into the Double Ratchet's.

mod braid;
mod chain;
mod code;

use crate::base::proto;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use alloc::vec::Vec;
use braid::{Braid, Payload};
use chain::Chain;

const VERSION: u8 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Direction {
    AliceToBob = 0,
    BobToAlice = 1,
}

impl Direction {
    fn from_number(number: u64) -> Option<Direction> {
        match number {
            0 => Some(Direction::AliceToBob),
            1 => Some(Direction::BobToAlice),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Limits {
    pub(super) jump: u32,
    pub(super) out_of_order: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Ratchet {
    braid: Braid,
    chain: Option<Chain>,
    key: Option<[u8; 32]>,
    direction: Direction,
    limits: Limits,
    is_negotiating: bool,
}

impl core::fmt::Debug for Ratchet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ratchet")
            .field("direction", &self.direction)
            .field("negotiating", &self.is_negotiating)
            .finish_non_exhaustive()
    }
}

struct Preamble<'a> {
    version: u8,
    epoch: u64,
    index: u32,
    rest: &'a [u8],
}

fn parse_preamble(message: &[u8]) -> Option<Preamble<'_>> {
    let Some((&version, rest)) = message.split_first() else {
        return Some(Preamble { version: 0, epoch: 0, index: 0, rest: &[] });
    };
    let (epoch, length) = proto::read_varint(rest)?;
    let rest = &rest[length..];
    let (index, length) = proto::read_varint(rest)?;
    if epoch == 0 {
        return None;
    }
    Some(Preamble { version, epoch, index: u32::try_from(index).ok()?, rest: &rest[length..] })
}

fn parse_payload(bytes: &[u8]) -> Option<Payload> {
    let (&kind, rest) = bytes.split_first()?;
    let chunk = match kind {
        1 | 2 | 3 | 5 | 6 => Some(code::read_chunk(rest)?),
        _ => None,
    };
    Payload::from_parts(kind, chunk)
}

impl Ratchet {
    pub(super) fn new(crypto: &impl Crypto, key: [u8; 32], direction: Direction, limits: Limits) -> Option<Ratchet> {
        let braid = match direction {
            Direction::AliceToBob => Braid::alice(crypto, &key)?,
            Direction::BobToAlice => Braid::bob(crypto, &key)?,
        };
        Some(Ratchet { braid, chain: None, key: Some(key), direction, limits, is_negotiating: true })
    }

    fn chain(&self, crypto: &impl Crypto) -> Option<Chain> {
        match &self.chain {
            Some(chain) => Some(chain.clone()),
            None => Chain::new(crypto, self.key.as_ref()?, self.direction, self.limits),
        }
    }

    pub(crate) fn send(&self, crypto: &impl Crypto) -> Option<(Ratchet, Vec<u8>, [u8; 32])> {
        let (braid, payload, secret) = self.braid.clone().send(crypto)?;
        let epoch = braid.epoch;
        let mut chain = self.chain(crypto)?;
        if let Some(secret) = secret {
            chain.add_epoch(crypto, secret.epoch, &secret.secret)?;
        }
        let (index, key) = chain.send_key(crypto, epoch - 1)?;
        let mut message = alloc::vec![VERSION];
        proto::write_varint(&mut message, epoch);
        proto::write_varint(&mut message, u64::from(index));
        message.push(payload.kind());
        if let Some(chunk) = payload.chunk() {
            code::write_chunk(chunk, &mut message);
        }
        Some((Ratchet { braid, chain: Some(chain), ..self.clone() }, message, key))
    }

    pub(super) fn receive(&self, crypto: &impl Crypto, message: &[u8]) -> Option<(Ratchet, Option<[u8; 32]>)> {
        let preamble = parse_preamble(message)?;
        if preamble.version < VERSION || (preamble.version != VERSION && !self.is_negotiating) {
            return None;
        }
        let mut chain = self.chain(crypto)?;
        if preamble.version > VERSION {
            let key = chain.receive_key(crypto, 0, preamble.index)?;
            return Some((Ratchet { chain: Some(chain), ..self.clone() }, key));
        }
        let (braid, secret) = self.braid.clone().receive(crypto, preamble.epoch, &parse_payload(preamble.rest)?)?;
        if let Some(secret) = secret {
            chain.add_epoch(crypto, secret.epoch, &secret.secret)?;
        }
        let key = chain.receive_key(crypto, preamble.epoch - 1, preamble.index)?;
        Some((Ratchet { braid, chain: Some(chain), key: None, is_negotiating: false, ..self.clone() }, key))
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        Writer::new()
            .message(1, self.braid.write())
            .maybe(self.chain.as_ref().map(|chain| |w: Writer| w.message(2, chain.write())))
            .maybe(self.key.map(|key| move |w: Writer| w.bytes(3, &key)))
            .varint(4, self.direction as u64)
            .varint(5, u64::from(self.limits.jump))
            .varint(6, u64::from(self.limits.out_of_order))
            .bool(7, self.is_negotiating)
            .finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Ratchet> {
        let message = Message::parse(bytes)?;
        let chain = match message.message(2) {
            Some(chain) => Some(Chain::read(&chain)?),
            None => None,
        };
        let is_negotiating = message.bool(7)?;
        let key = match is_negotiating {
            true => Some(message.bytes(3)?.try_into().ok()?),
            false => None,
        };
        (chain.is_some() || key.is_some()).then_some(())?;
        Some(Ratchet {
            braid: Braid::read(&message.message(1)?)?,
            chain,
            key,
            direction: Direction::from_number(message.varint(4)?)?,
            limits: Limits { jump: message.u32(5)?, out_of_order: message.u32(6)? },
            is_negotiating,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::tests::Standin;

    #[test]
    fn debug_without_keys() {
        let limits = Limits { jump: 1, out_of_order: 1 };
        let ratchet = Ratchet::new(&Standin::default(), [7; 32], Direction::AliceToBob, limits).unwrap();
        assert_eq!(alloc::format!("{ratchet:?}"), "Ratchet { direction: AliceToBob, negotiating: true, .. }");
    }

    #[test]
    fn session_key_release() {
        let crypto = Standin::default();
        let limits = Limits { jump: 25_000, out_of_order: 2_000 };
        let alice = Ratchet::new(&crypto, [7; 32], Direction::AliceToBob, limits).unwrap();
        let bob = Ratchet::new(&crypto, [7; 32], Direction::BobToAlice, limits).unwrap();
        let (alice, message, send_key) = alice.send(&crypto).unwrap();
        assert_eq!(alice.key, Some([7; 32]), "sending keeps it");
        assert_eq!(Ratchet::read(&alice.write()).as_ref(), Some(&alice));
        let (bob, receive_key) = bob.receive(&crypto, &message).unwrap();
        assert_eq!(receive_key, Some(send_key));
        assert_eq!((bob.key, bob.is_negotiating), (None, false));
        assert!(!bob.write().windows(32).any(|window| window == [7; 32]), "nor is it written");
        let old_file = [bob.write(), Writer::new().bytes(3, &[7; 32]).finish()].concat();
        assert_eq!(Ratchet::read(&old_file).as_ref(), Some(&bob), "a file written while it was kept");
    }
}
