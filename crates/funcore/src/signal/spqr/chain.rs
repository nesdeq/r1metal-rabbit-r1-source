use super::{Direction, Limits};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use crate::signal::hkdf;
use crate::signal::read_skipped;
use crate::signal::write_skipped;
use alloc::vec::Vec;

const START: &[u8] = b"Signal PQ Ratchet V1 Chain  Start";
const ADD_EPOCH: &[u8] = b"Signal PQ Ratchet V1 Chain Add Epoch";
const NEXT: &[u8] = b"Signal PQ Ratchet V1 Chain Next";
const PRIOR_EPOCHS: usize = 1;

#[derive(Clone, PartialEq, Eq)]
struct Lane {
    counter: u32,
    next: Option<[u8; 32]>,
    skipped: Vec<(u32, [u8; 32])>,
}

#[derive(Clone, PartialEq, Eq)]
struct Link {
    send: Lane,
    receive: Lane,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Chain {
    direction: Direction,
    current_epoch: u64,
    send_epoch: u64,
    links: Vec<Link>,
    next_root: [u8; 32],
    limits: Limits,
}

// The skipped keys at which a lane lets go of those behind its window: a tenth past the window, SPQR's `trim_size`.
const fn trim_size(window: u32) -> usize {
    window as usize * 11 / 10 + 1
}

fn to_key(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("thirty-two bytes")
}

impl Lane {
    fn new(key: [u8; 32]) -> Lane {
        Lane { counter: 0, next: Some(key), skipped: Vec::new() }
    }

    fn step(&mut self, crypto: &impl Crypto) -> Option<(u32, [u8; 32])> {
        let counter = self.counter.checked_add(1)?;
        let out: [u8; 64] = hkdf(crypto, Some(&[0; 32]), &self.next?, &[&counter.to_be_bytes(), NEXT])?;
        self.counter = counter;
        self.next = Some(to_key(&out[..32]));
        Some((counter, to_key(&out[32..])))
    }

    fn key(&mut self, at: u32, limits: &Limits, crypto: &impl Crypto) -> Option<[u8; 32]> {
        let window = limits.out_of_order;
        if at < self.counter {
            if u64::from(at) + u64::from(window) < u64::from(self.counter) {
                return None;
            }
            let position = self.skipped.iter().position(|(index, _)| *index == at)?;
            return Some(self.skipped.swap_remove(position).1);
        }
        if at == self.counter || at - self.counter > limits.jump {
            return None;
        }
        if u64::from(at) > u64::from(self.counter) + u64::from(window) {
            self.skipped.clear();
        }
        while at > self.counter + 1 {
            let entry = self.step(crypto)?;
            if u64::from(self.counter) + u64::from(window) >= u64::from(at) {
                self.skipped.push(entry);
            }
        }
        if self.skipped.len() >= trim_size(window) {
            let horizon = self.counter.saturating_sub(window);
            self.skipped.retain(|(index, _)| *index >= horizon);
        }
        Some(self.step(crypto)?.1)
    }

    fn write(&self) -> Writer {
        Writer::new()
            .varint(1, u64::from(self.counter))
            .maybe(self.next.map(|next| move |w: Writer| w.bytes(2, &next)))
            .bytes(3, &write_skipped(&self.skipped))
    }

    fn read(message: &Message) -> Option<Lane> {
        let next = match message.bytes(2) {
            Some(next) => Some(next.try_into().ok()?),
            None => None,
        };
        let skipped = read_skipped(message.bytes(3).unwrap_or_default())?;
        Some(Lane { counter: message.u32(1)?, next, skipped })
    }
}

impl Chain {
    fn link(direction: Direction, out: &[u8; 96]) -> Link {
        let (send, receive) = match direction {
            Direction::AliceToBob => (&out[32..64], &out[64..96]),
            Direction::BobToAlice => (&out[64..96], &out[32..64]),
        };
        Link { send: Lane::new(to_key(send)), receive: Lane::new(to_key(receive)) }
    }

    pub(super) fn new(crypto: &impl Crypto, key: &[u8], direction: Direction, limits: Limits) -> Option<Chain> {
        let out: [u8; 96] = hkdf(crypto, Some(&[0; 32]), key, &[START])?;
        Some(Chain {
            direction,
            current_epoch: 0,
            send_epoch: 0,
            links: alloc::vec![Chain::link(direction, &out)],
            next_root: to_key(&out[..32]),
            limits,
        })
    }

    pub(super) fn add_epoch(&mut self, crypto: &impl Crypto, epoch: u64, secret: &[u8]) -> Option<()> {
        if epoch != self.current_epoch + 1 {
            return None;
        }
        let out: [u8; 96] = hkdf(crypto, Some(&self.next_root), secret, &[ADD_EPOCH])?;
        self.current_epoch = epoch;
        self.next_root = to_key(&out[..32]);
        self.links.push(Chain::link(self.direction, &out));
        Some(())
    }

    fn link_index(&self, epoch: u64) -> Option<usize> {
        let offset = usize::try_from(self.current_epoch.checked_sub(epoch)?).ok()?;
        self.links.len().checked_sub(offset + 1)
    }

    pub(super) fn send_key(&mut self, crypto: &impl Crypto, epoch: u64) -> Option<(u32, [u8; 32])> {
        if epoch < self.send_epoch {
            return None;
        }
        let mut index = self.link_index(epoch)?;
        if epoch != self.send_epoch {
            self.send_epoch = epoch;
            while index > PRIOR_EPOCHS {
                self.links.remove(0);
                index -= 1;
            }
            self.links[..index].iter_mut().for_each(|link| link.send.next = None);
        }
        self.links[index].send.step(crypto)
    }

    pub(super) fn receive_key(&mut self, crypto: &impl Crypto, epoch: u64, index: u32) -> Option<Option<[u8; 32]>> {
        if epoch == 0 && index == 0 {
            return Some(None);
        }
        let link = self.link_index(epoch)?;
        let limits = self.limits;
        self.links[link].receive.key(index, &limits, crypto).map(Some)
    }

    pub(crate) fn write(&self) -> Writer {
        let mut writer = Writer::new()
            .varint(1, self.direction as u64)
            .varint(2, self.current_epoch)
            .varint(3, self.send_epoch)
            .bytes(4, &self.next_root)
            .varint(5, u64::from(self.limits.jump))
            .varint(6, u64::from(self.limits.out_of_order));
        for link in &self.links {
            writer = writer.message(7, Writer::new().message(1, link.send.write()).message(2, link.receive.write()));
        }
        writer
    }

    pub(crate) fn read(message: &Message) -> Option<Chain> {
        let links = message
            .repeated(7)
            .map(|link| {
                let link = Message::parse(link)?;
                Some(Link { send: Lane::read(&link.message(1)?)?, receive: Lane::read(&link.message(2)?)? })
            })
            .collect::<Option<Vec<Link>>>()?;
        (!links.is_empty()).then_some(())?;
        Some(Chain {
            direction: Direction::from_number(message.varint(1)?)?,
            current_epoch: message.varint(2)?,
            send_epoch: message.varint(3)?,
            links,
            next_root: message.bytes(4)?.try_into().ok()?,
            limits: Limits { jump: message.u32(5)?, out_of_order: message.u32(6)? },
        })
    }
}
