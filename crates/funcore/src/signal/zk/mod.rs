// zkgroup's arithmetic, as libsignal computes it, read from libsignal's bincode.

pub(super) mod attributes;
pub(super) mod credential;
pub(super) mod endorsement;
mod proof;
pub(super) mod ristretto;

use super::curve::{ORDER_BYTES, multiply_add, reduce};
use crate::crypto::Primitives;
use ristretto::Element;

const BLOCK_TAG: u8 = 1;
const RATCHET_TAG: u8 = 2;
const ABSORB_TAG: u8 = 0;
const WIDE_BYTES: usize = 64;

#[derive(Clone, Copy)]
pub(super) struct Sho {
    chaining: [u8; 32],
}

impl Sho {
    pub(super) fn new(crypto: &impl Primitives, label: &[u8]) -> Sho {
        let mut sho = Sho { chaining: [0; 32] };
        sho.absorb(crypto, label);
        sho
    }

    pub(super) fn absorb(&mut self, crypto: &impl Primitives, input: &[u8]) {
        self.absorb_all(crypto, &[input]);
    }

    fn absorb_all(&mut self, crypto: &impl Primitives, parts: &[&[u8]]) {
        let mut parts = parts.to_vec();
        parts.push(&[ABSORB_TAG]);
        self.chaining = crypto.hmac_sha256(&self.chaining, &parts);
    }

    pub(super) fn squeeze(&mut self, crypto: &impl Primitives, out: &mut [u8]) {
        for (block, chunk) in out.chunks_mut(32).enumerate() {
            let digest = crypto.hmac_sha256(&self.chaining, &[&(block as u64).to_be_bytes(), &[BLOCK_TAG]]);
            chunk.copy_from_slice(&digest[..chunk.len()]);
        }
        self.chaining = crypto.hmac_sha256(&self.chaining, &[&(out.len() as u64).to_be_bytes(), &[RATCHET_TAG]]);
    }

    fn scalar(&mut self, crypto: &impl Primitives) -> Scalar {
        let mut bytes = [0u8; WIDE_BYTES];
        self.squeeze(crypto, &mut bytes);
        Scalar::from_wide(&bytes)
    }

    fn element(&mut self, crypto: &impl Primitives) -> Element {
        let mut bytes = [0u8; WIDE_BYTES];
        self.squeeze(crypto, &mut bytes);
        Element::from_uniform(&bytes)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(length)?;
        self.0 = rest;
        Some(head)
    }

    fn element(&mut self) -> Option<Element> {
        Element::decode(self.take(32)?)
    }

    fn scalar(&mut self) -> Option<Scalar> {
        Scalar::read(self.take(32)?)
    }

    fn number(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = self.number()?;
        self.take(usize::try_from(length).ok()?)
    }
}

const MINUS_ONE: [u8; 32] = {
    let mut minus_one = ORDER_BYTES;
    minus_one[0] -= 1;
    minus_one
};
const MINUS_TWO: [u8; 32] = {
    let mut minus_two = ORDER_BYTES;
    minus_two[0] -= 2;
    minus_two
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Scalar([u8; 32]);

impl Scalar {
    fn from_wide(bytes: &[u8; 64]) -> Scalar {
        Scalar(reduce(bytes))
    }

    pub(super) fn read(bytes: &[u8]) -> Option<Scalar> {
        let bytes: [u8; 32] = bytes.try_into().ok()?;
        (reduce(&bytes) == bytes).then_some(Scalar(bytes))
    }

    const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(super) fn mul(self, other: Scalar) -> Scalar {
        Scalar(multiply_add(&self.0, &other.0, &[0; 32]))
    }

    pub(super) fn neg(self) -> Scalar {
        Scalar(multiply_add(&MINUS_ONE, &self.0, &[0; 32]))
    }

    fn mul_add(self, factor: Scalar, addend: Scalar) -> Scalar {
        Scalar(multiply_add(&self.0, &factor.0, &addend.0))
    }

    fn invert(self) -> Scalar {
        let bits = MINUS_TWO.iter().rev().flat_map(|byte| (0..8).rev().map(move |bit| byte >> bit & 1 == 1));
        bits.fold(Scalar(reduce(&[1])), |power, is_set| {
            let square = power.mul(power);
            if is_set { square.mul(self) } else { square }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Exact;
    use super::*;
    use crate::base::code::unhex;

    #[test]
    fn sho_vectors() {
        let crypto = Exact::default();
        let squeeze = |length: usize| {
            let mut sho = Sho::new(&crypto, b"asd");
            sho.absorb(&crypto, b"asdasd");
            let mut out = alloc::vec![0u8; length];
            sho.squeeze(&crypto, &mut out);
            out
        };
        let first = unhex("392cb9449373037fa0c11aebed69cca3b7d3bc9790878f341729c65d5506442f04986cb5c9098f277c3ea640a4dc6e90372b433a90af9aea7072eaba3398c4fe").unwrap();
        assert_eq!(squeeze(64), first, "64 bytes, two whole blocks");
        assert_eq!(squeeze(65), [&first[..], &[0x7a]].concat(), "and one byte of a third");
        let mut sho = Sho::new(&crypto, b"");
        sho.absorb(&crypto, b"abc");
        for length in [63, 64, 65, 127, 128, 129] {
            sho.absorb_all(&crypto, &[&alloc::vec![0u8; length - 1], &[0]]);
        }
        for length in [63, 64, 65, 127, 128, 129] {
            sho.squeeze(&crypto, &mut alloc::vec![0u8; length]);
        }
        sho.absorb(&crypto, b"def");
        let mut last = [0u8; 63];
        sho.squeeze(&crypto, &mut last);
        let expected = "c5c13bcc6596c25fc4514eac9269dd6e3e57ef70f4bfb8d67fd3082ed9732d7790d8d2686f19eb2533a65c94bb8ceda0a068e1b615c81bb26e411889da9fb7";
        assert_eq!(
            last.to_vec(),
            unhex(expected).unwrap(),
            "absorbs in parts and squeezes of every length around a block"
        );
    }

    #[test]
    fn scalar_arithmetic() {
        let scalar = |byte: u8| Scalar::from_wide(&[byte; 64]);
        let (unit, a, b) = (Scalar(reduce(&[1])), scalar(3), scalar(200));
        assert!(unit.mul_add(a, a.neg()) == Scalar(reduce(&[0])), "a scalar and its negation make nought");
        assert!(unit.mul_add(a.mul(b), a) == a.mul_add(b, a), "a b + a");
        assert!(a.mul(b) == b.mul(a));
        assert!(Scalar(reduce(&[0])).neg() == Scalar([0; 32]), "nought is its own negation");
        assert!(a.mul(a.invert()) == unit && b.invert().invert() == b, "a scalar times its inverse is one");
        assert!(Scalar([0; 32]).invert() == Scalar([0; 32]), "nought has none");
        assert!(Scalar::read(&ORDER_BYTES).is_none(), "ℓ itself is no scalar");
        assert!(Scalar::read(&MINUS_ONE).is_some_and(|scalar| scalar == Scalar(reduce(&[1])).neg()));
        assert!(Scalar::read(&[0; 31]).is_none(), "31 bytes are none");
    }
}
