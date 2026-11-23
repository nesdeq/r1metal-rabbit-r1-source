use super::Rect;
use crate::base::pixel::Color;
use core::cell::Cell;

const SEED: u64 = 0x243F_6A88_85A3_08D3;
const SPREAD: [u64; 2] = [0xBF58_476D_1CE4_E5B9, 0x94D0_49BB_1331_11EB];

pub(crate) struct Ids(Cell<u64>);

impl Default for Ids {
    fn default() -> Self {
        Ids(Cell::new(1))
    }
}

impl Ids {
    pub(crate) fn next(&self) -> u64 {
        self.0.replace(self.0.get() + 1)
    }

    pub(crate) fn part(&self) -> Ids {
        Ids(Cell::new(self.next() << u32::BITS))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Key(u64);

impl Default for Key {
    fn default() -> Self {
        Key(SEED)
    }
}

impl Key {
    pub(crate) const fn word(self, word: u64) -> Key {
        let mut spread = self.0 ^ word;
        spread = (spread ^ spread >> 30).wrapping_mul(SPREAD[0]);
        spread = (spread ^ spread >> 27).wrapping_mul(SPREAD[1]);
        Key(spread ^ spread >> 31)
    }

    pub(crate) const fn number(self, number: i32) -> Key {
        self.word(number as u32 as u64)
    }

    pub(super) const fn point(self, (x, y): (f32, f32)) -> Key {
        self.word((x.to_bits() as u64) << 32 | y.to_bits() as u64)
    }

    pub(crate) const fn colour(self, colour: Color) -> Key {
        self.word(colour.0 as u64)
    }

    pub(super) const fn rect(self, rect: Rect) -> Key {
        self.number(rect.x).number(rect.y).number(rect.w).number(rect.h)
    }

    pub(crate) fn bytes(self, bytes: &[u8]) -> Key {
        let chunks = bytes.chunks(size_of::<u64>());
        chunks
            .fold(self, |key, chunk| {
                let mut word = [0u8; size_of::<u64>()];
                word[..chunk.len()].copy_from_slice(chunk);
                key.word(u64::from_le_bytes(word))
            })
            .word(bytes.len() as u64)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

impl core::hash::Hasher for Key {
    fn write(&mut self, bytes: &[u8]) {
        *self = self.bytes(bytes);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_separates_bits_and_order() {
        let base = Key::default();
        assert_eq!(base.number(3).number(4), base.number(3).number(4));
        assert_ne!(base.number(3).number(4), base.number(4).number(3), "order counts");
        assert_ne!(base.word(1), base.word(3), "one bit counts");
        assert_ne!(base.bytes(b"ab"), base.bytes(b"ab\0"), "and how many bytes there are");
        assert_ne!(base.bytes(b""), base, "even none");
        assert_eq!(base.rect(Rect::new(1, 2, 3, 4)), base.number(1).number(2).number(3).number(4));
        let ids = Ids::default();
        let (one, other) = (ids.next(), ids.next());
        assert!(other > one, "every content has a name of its own");
    }
}
