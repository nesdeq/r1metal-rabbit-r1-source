//! The framing the modem's file service and RPC share (`ccci_fsd`, `ccci_rpcd`): a request in fragments into one of five slots, a reply in fragments back.

use crate::base::bytes::le32;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// The kernel's `ccci_header`.
const HEADER_BYTES: usize = 16;
// The header and the operation.
const HEAD_BYTES: usize = HEADER_BYTES + 4;
// The most a fragment carries after its head (`CCCI_MTU`).
const FRAGMENT_BYTES: usize = 3456;
/// The most a read of the node returns.
pub const PACKET_BYTES: usize = HEAD_BYTES + FRAGMENT_BYTES;
const SLOTS: usize = 5;
const MORE: u32 = 0x8000_0000;
const REPLY: u32 = 0xFFFF_0000;
const LENGTH_AT: usize = 4;
const CHANNEL_AT: usize = 8;
const SLOT_AT: usize = 12;
const OPERATION_AT: usize = 16;
const CHANNEL_BITS: u32 = 0xFFFF;
const WORD: usize = 4;

/// A request whose fragments are all in: its head, its operation and what follows them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    head: [u8; HEADER_BYTES],
    pub(super) operation: u32,
    payload: Vec<u8>,
}

impl Request {
    // The parameters, each its bytes; none where the count passes `most`, one runs past the end, or, with `allow_empty` false, one is empty.
    pub(super) fn params(&self, most: usize, allow_empty: bool) -> Option<Vec<&[u8]>> {
        let count = usize::try_from(le32(&self.payload, 0)?).ok().filter(|&count| count <= most)?;
        let (mut at, mut params) = (WORD, Vec::with_capacity(count));
        for _ in 0..count {
            let length = usize::try_from(le32(&self.payload, at)?).ok()?;
            let param = self.payload.get(at + WORD..at + WORD + length)?;
            if param.is_empty() && !allow_empty {
                return None;
            }
            params.push(param);
            at += WORD + length.next_multiple_of(WORD);
        }
        Some(params)
    }

    /// The fragments of the reply that carries `params`: the request's head, one channel higher, and the operation marked as answered.
    pub fn reply(&self, params: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(params.len() as u32).to_le_bytes());
        for param in params {
            payload.extend_from_slice(&(param.len() as u32).to_le_bytes());
            payload.extend_from_slice(param);
            payload.resize(payload.len().next_multiple_of(WORD), 0);
        }
        let first = le32(&self.head, 0).unwrap_or(0) & !MORE;
        let channel = le32(&self.head, CHANNEL_AT).unwrap_or(0).wrapping_add(1);
        let chunks: Vec<&[u8]> = payload.chunks(FRAGMENT_BYTES).collect();
        let last = chunks.len() - 1;
        let operation = self.operation | REPLY;
        chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                let mut fragment = Vec::with_capacity(HEAD_BYTES + chunk.len());
                fragment.extend_from_slice(&(if index < last { first | MORE } else { first }).to_le_bytes());
                fragment.extend_from_slice(&((HEAD_BYTES + chunk.len()) as u32).to_le_bytes());
                fragment.extend_from_slice(&channel.to_le_bytes());
                fragment.extend_from_slice(&self.head[SLOT_AT..HEADER_BYTES]);
                fragment.extend_from_slice(&operation.to_le_bytes());
                fragment.extend_from_slice(chunk);
                fragment
            })
            .collect()
    }
}

/// The flaw a packet read from the node is dropped for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flaw {
    // Shorter than its head, or than its header says.
    Short,
    // On another channel than the service's.
    Channel(u32),
    // In a slot past the five.
    Slot(u32),
    // Past what a slot holds.
    Overflow,
}

/// The log line of a packet dropped on the channel of that number.
pub fn to_drop_line(channel: u32, flaw: Flaw) -> String {
    format!("cell: a packet on channel {channel} was dropped: {flaw:?}")
}

/// The five slots requests arrive in.
#[derive(Default)]
pub struct Slots {
    // What came so far of each slot's request: the latest fragment's header, then every fragment's bytes past its head.
    parts: [Option<Vec<u8>>; SLOTS],
}

impl Slots {
    /// Takes a packet read from the node on `channel`: the request once its last fragment is in, nothing while more are due, or why it was dropped; a slot holds `capacity` bytes past the head.
    pub fn take(&mut self, packet: &[u8], channel: u32, capacity: usize) -> Result<Option<Request>, Flaw> {
        let (Some(first), Some(length), Some(on), Some(slot)) =
            (le32(packet, 0), le32(packet, LENGTH_AT), le32(packet, CHANNEL_AT), le32(packet, SLOT_AT))
        else {
            return Err(Flaw::Short);
        };
        if on & CHANNEL_BITS != channel {
            return Err(Flaw::Channel(on & CHANNEL_BITS));
        }
        let part = self.parts.get_mut(slot as usize).ok_or(Flaw::Slot(slot))?;
        let length = length as usize;
        if length < HEAD_BYTES || length > packet.len() {
            return Err(Flaw::Short);
        }
        let gathered = match part.take() {
            None => packet[..length].to_vec(),
            Some(mut gathered) => {
                gathered.extend_from_slice(&packet[HEAD_BYTES..length]);
                gathered[..HEADER_BYTES].copy_from_slice(&packet[..HEADER_BYTES]);
                gathered
            }
        };
        if gathered.len() - HEAD_BYTES > capacity {
            return Err(Flaw::Overflow);
        }
        if first & MORE != 0 {
            *part = Some(gathered);
            return Ok(None);
        }
        let mut head = [0u8; HEADER_BYTES];
        head.copy_from_slice(&gathered[..HEADER_BYTES]);
        let operation = le32(&gathered, OPERATION_AT).ok_or(Flaw::Short)?;
        Ok(Some(Request { head, operation, payload: gathered[HEAD_BYTES..].to_vec() }))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use alloc::vec;

    // The fragments of a request on `channel` in `slot`, its parameters laid out as the modem lays them.
    pub(crate) fn request(channel: u32, slot: u32, operation: u32, params: &[&[u8]]) -> Vec<Vec<u8>> {
        let head =
            Request { head: header(0, 0, channel - 1, slot), operation: operation & !REPLY, payload: Vec::new() };
        head.reply(params)
            .into_iter()
            .map(|mut fragment| {
                fragment[OPERATION_AT..HEAD_BYTES].copy_from_slice(&operation.to_le_bytes());
                fragment
            })
            .collect()
    }

    fn header(first: u32, length: u32, channel: u32, slot: u32) -> [u8; HEADER_BYTES] {
        let mut head = [0u8; HEADER_BYTES];
        for (at, word) in [(0, first), (LENGTH_AT, length), (CHANNEL_AT, channel), (SLOT_AT, slot)] {
            head[at..at + WORD].copy_from_slice(&word.to_le_bytes());
        }
        head
    }

    // The parameters of a reply's fragments, gathered on the channel its first one names.
    pub(crate) fn reply_params(fragments: &[Vec<u8>]) -> (u32, Vec<Vec<u8>>) {
        let mut slots = Slots::default();
        let channel = le32(&fragments[0], CHANNEL_AT).unwrap() & CHANNEL_BITS;
        let request = fragments
            .iter()
            .find_map(|fragment| slots.take(fragment, channel, usize::MAX).unwrap())
            .expect("a whole reply");
        let params = request.params(usize::MAX, true).unwrap().into_iter().map(<[u8]>::to_vec).collect();
        (request.operation, params)
    }

    #[test]
    fn one_fragment_request() {
        let fragments = request(14, 2, 0x1001, &[b"Z\0:\0", &0x100u32.to_le_bytes()]);
        assert_eq!(fragments.len(), 1);
        let mut slots = Slots::default();
        let request = slots.take(&fragments[0], 14, 4096).unwrap().unwrap();
        assert_eq!(request.operation, 0x1001);
        assert_eq!(request.params(6, false).unwrap(), [&b"Z\0:\0"[..], &[0, 1, 0, 0]]);
        assert_eq!(request.params(1, false), None, "more than the service takes");
    }

    #[test]
    fn fragments_gather_in_their_slot() {
        let big = vec![0x5Au8; FRAGMENT_BYTES + 100];
        let fragments = request(14, 3, 0x1004, &[&1u32.to_le_bytes(), &big, &(big.len() as u32).to_le_bytes()]);
        assert_eq!(fragments.len(), 2);
        let mut slots = Slots::default();
        let other = request(14, 1, 0x1005, &[&1u32.to_le_bytes()]);
        assert_eq!(slots.take(&fragments[0], 14, 16_584), Ok(None), "more is due");
        assert!(slots.take(&other[0], 14, 16_584).unwrap().is_some(), "another slot is its own");
        let request = slots.take(&fragments[1], 14, 16_584).unwrap().unwrap();
        let params = request.params(6, false).unwrap();
        assert_eq!((params.len(), params[1].len()), (3, big.len()));
        assert!(params[1].iter().all(|&byte| byte == 0x5A));
        assert_eq!(slots.take(&fragments[1], 14, 16_584).map(|request| request.is_some()), Ok(true));
    }

    #[test]
    fn drops() {
        let mut slots = Slots::default();
        let fragment = request(14, 2, 0x1001, &[b"x\0"]).remove(0);
        assert_eq!(slots.take(&fragment[..10], 14, 4096), Err(Flaw::Short));
        assert_eq!(slots.take(&fragment[..fragment.len() - 1], 14, 4096), Err(Flaw::Short));
        assert_eq!(slots.take(&fragment, 32, 4096), Err(Flaw::Channel(14)));
        let past = request(14, 5, 0x1001, &[b"x\0"]).remove(0);
        assert_eq!(slots.take(&past, 14, 4096), Err(Flaw::Slot(5)));
        assert_eq!(slots.take(&fragment, 14, 4), Err(Flaw::Overflow));
        assert_eq!(to_drop_line(14, Flaw::Slot(5)), "cell: a packet on channel 14 was dropped: Slot(5)");
    }

    #[test]
    fn empty_params() {
        let fragment = request(32, 0, 0x400F, &[b""]).remove(0);
        let request = Slots::default().take(&fragment, 32, 2048).unwrap().unwrap();
        assert_eq!(request.params(6, false), None, "the file service takes no empty parameter");
        assert_eq!(request.params(6, true), Some(vec![&b""[..]]), "RPC does");
    }

    #[test]
    fn reply_heads() {
        let request = Request { head: header(MORE | 7, 99, 14, 4), operation: 0x1003, payload: Vec::new() };
        let big = vec![1u8; FRAGMENT_BYTES * 2];
        let fragments = request.reply(&[&0u32.to_le_bytes(), &big]);
        assert_eq!(fragments.len(), 3, "the payload in pieces of {FRAGMENT_BYTES} at most");
        for (index, fragment) in fragments.iter().enumerate() {
            let words = [0, 4, 8, 12, 16].map(|at| le32(fragment, at).unwrap());
            let more = if index < 2 { MORE } else { 0 };
            assert_eq!(
                words,
                [more | 7, fragment.len() as u32, 15, 4, 0xFFFF_1003],
                "fragment {index}: marked while more follow, its length, the channel one higher, the slot, the answer"
            );
            assert!(fragment.len() <= PACKET_BYTES);
        }
        let (operation, params) = reply_params(&fragments);
        assert_eq!((operation, params.len(), params[1].len()), (0xFFFF_1003, 2, big.len()));
        let padded = request.reply(&[b"abc"]);
        assert_eq!(&padded[0][HEAD_BYTES..], &[1, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0], "padded to 4");
    }
}
