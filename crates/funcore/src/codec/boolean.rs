use crate::base::bits::BYTE_BITS;
use crate::codec::Flaw;
use alloc::vec::Vec;

const WINDOW_BITS: u32 = u64::BITS;
const WINDOW_BYTES: usize = (WINDOW_BITS / u8::BITS) as usize;
// A split is the range scaled by a probability of 256 (RFC 6386, 7.3).
const PROBABILITY_BITS: u32 = 8;
// The range stays from 128 to 255 between bools.
const FULL_RANGE: u32 = 255;
// A flag and each bit of a literal is a bool of even odds (RFC 6386, 8).
pub(super) const EVEN: u8 = 128;
// The encoder keeps three bytes of the interval's low end behind the byte it shifts out next (RFC 6386, 7.3).
const LOW_BITS: i32 = 24;
const LOW_MASK: u32 = (1 << LOW_BITS) - 1;
// The deepest leaf of VP8's trees, which the encoder writes: B_HU_PRED's, seven nodes down (RFC 6386, 11.2).
const MOST_DEPTH: usize = 7;

// The boolean entropy decoder of one partition (RFC 6386, 7.3; VP9 9.2), reading the bytes past its end as zeros.
pub(super) struct BoolDecoder<'a> {
    data: &'a [u8],
    at: usize,
    // The bits not yet shifted out, highest first: the top byte is the one compared with a split.
    value: u64,
    bits: u32,
    range: u32,
    is_past_end: bool,
}

impl<'a> BoolDecoder<'a> {
    pub(super) const fn new(data: &'a [u8]) -> BoolDecoder<'a> {
        BoolDecoder { data, at: 0, value: 0, bits: 0, range: FULL_RANGE, is_past_end: false }
    }

    // A VP9 partition, whose first bool, the marker, is false (VP9 9.2.1); it holds at least one byte.
    pub(super) fn open(data: &'a [u8]) -> Result<BoolDecoder<'a>, Flaw> {
        if data.is_empty() {
            return Err(Flaw::Short);
        }
        let mut decoder = BoolDecoder::new(data);
        if decoder.flag() {
            return Err(Flaw::Invalid("marker bit"));
        }
        Ok(decoder)
    }

    // Loads whole bytes behind the bits held; only a bool about to be read is decided by zeros past the end.
    #[inline(never)]
    fn fill(&mut self) {
        let room = (WINDOW_BITS - self.bits) / u8::BITS;
        if let Some(&chunk) = self.data.get(self.at..).and_then(|rest| rest.first_chunk::<WINDOW_BYTES>()) {
            let loaded = u64::from_be_bytes(chunk) >> (WINDOW_BITS - room * u8::BITS);
            self.value |= loaded << (WINDOW_BITS - self.bits - room * u8::BITS);
            self.at += room as usize;
            self.bits += room * u8::BITS;
            return;
        }
        while self.bits <= WINDOW_BITS - u8::BITS {
            let Some(&byte) = self.data.get(self.at) else {
                break;
            };
            self.value |= u64::from(byte) << (WINDOW_BITS - u8::BITS - self.bits);
            self.at += 1;
            self.bits += u8::BITS;
        }
        if self.bits < u8::BITS {
            self.is_past_end = true;
            self.bits = WINDOW_BITS;
        }
    }

    // A bool whose chance of being false is `probability` in 256.
    #[inline]
    pub(super) fn read(&mut self, probability: u8) -> bool {
        if self.bits < u8::BITS {
            self.fill();
        }
        let split = 1 + (((self.range - 1) * u32::from(probability)) >> PROBABILITY_BITS);
        let big = u64::from(split) << (WINDOW_BITS - u8::BITS);
        let bit = self.value >= big;
        if bit {
            self.range -= split;
            self.value -= big;
        } else {
            self.range = split;
        }
        let shift = self.range.leading_zeros() - (u32::BITS - u8::BITS);
        self.range <<= shift;
        self.value <<= shift;
        self.bits -= shift;
        bit
    }

    pub(super) fn flag(&mut self) -> bool {
        self.read(EVEN)
    }

    // L(n): an unsigned number of `count` flags, highest bit first.
    pub(super) fn literal(&mut self, count: u32) -> u32 {
        (0..count).fold(0, |value, _| value << 1 | u32::from(self.flag()))
    }

    // A magnitude of `count` bits followed by its sign.
    pub(super) fn signed(&mut self, count: u32) -> i32 {
        let magnitude = self.literal(count) as i32;
        if self.flag() { -magnitude } else { magnitude }
    }

    // A flag that, when set, is followed by a signed number; nought otherwise (RFC 6386, 9.6).
    pub(super) fn delta(&mut self, count: u32) -> i32 {
        if self.flag() { self.signed(count) } else { 0 }
    }

    // A value of a tree laid out as RFC 6386, 8.1 lays trees out.
    #[inline]
    pub(super) fn tree(&mut self, tree: &[i8], probabilities: &[u8]) -> u8 {
        let mut node = 0usize;
        loop {
            let next = tree[node + usize::from(self.read(probabilities[node / 2]))];
            if next <= 0 {
                return next.unsigned_abs();
            }
            node = next as usize;
        }
    }

    // Whether a bool was decided by bits past the end of the partition, which only a cut or damaged frame has.
    pub(super) const fn is_past_end(&self) -> bool {
        self.is_past_end
    }

    // The bits a decoder has taken into its value: its start, then one a shift (VP9 9.2.2).
    const fn taken(&self) -> usize {
        self.at * BYTE_BITS + BYTE_BITS - self.bits as usize
    }

    // Whether the bits past those taken, to the partition's end, are noughts, as VP9's padding is (VP9 9.2.3).
    pub(super) fn is_padding_zero(&self) -> bool {
        !self.is_past_end && self.value << u8::BITS == 0 && self.data[self.at..].iter().all(|&byte| byte == 0)
    }

    // The byte after the last whose bits were taken: past it a frame's padding runs to the next frame (VP9 9.2.3).
    pub(super) fn end(&self) -> usize {
        if self.is_past_end {
            return self.data.len();
        }
        self.taken().div_ceil(BYTE_BITS).min(self.data.len())
    }
}

// The probability that fits `zeros` false and `ones` true bools best, held to 1 to 255 (RFC 6386, 13.4; VP9 8.4.2).
pub(super) fn probability_of([zeros, ones]: [u32; 2]) -> u8 {
    let total = u64::from(zeros) + u64::from(ones);
    if total == 0 {
        return EVEN;
    }
    (((u64::from(zeros) << PROBABILITY_BITS) + total / 2) / total).clamp(1, 255) as u8
}

// The entry of `tree` that is the leaf `value`, or with `is_node` the one that leads to the node at `value`.
const fn entry(tree: &[i8], value: usize, is_node: bool) -> Option<usize> {
    let mut index = 0;
    while index < tree.len() {
        let next = tree[index];
        if (is_node && next > 0 && next as usize == value)
            || (!is_node && next <= 0 && next.unsigned_abs() as usize == value)
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

// The nodes and branches from a tree's root to a leaf, as many as the second's count.
pub(super) type Path = ([(usize, bool); MOST_DEPTH], usize);

// The nodes and branches from the root of a tree laid out as RFC 6386, 8.1 lays trees out to the leaf `value`.
pub(super) const fn path(tree: &[i8], value: u8) -> Path {
    let mut steps = [(0, false); MOST_DEPTH];
    let mut depth = 0;
    let mut found = entry(tree, value as usize, false);
    while let Some(index) = found {
        let node = index & !1;
        steps[depth] = (node, index & 1 == 1);
        depth += 1;
        found = if node == 0 { None } else { entry(tree, node, true) };
    }
    let mut low = 0;
    while low < depth / 2 {
        let high = depth - 1 - low;
        let step = steps[low];
        steps[low] = steps[high];
        steps[high] = step;
        low += 1;
    }
    (steps, depth)
}

// The bool encoder of RFC 6386, 7.3: the low end of the interval is shifted out a byte at a time, a carry added back.
pub(super) struct BoolEncoder {
    output: Vec<u8>,
    range: u32,
    low: u32,
    // Shifts until the next byte of `low` is whole, less than nought between bools.
    count: i32,
}

impl BoolEncoder {
    pub(super) const fn new() -> BoolEncoder {
        BoolEncoder { output: Vec::new(), range: FULL_RANGE, low: 0, count: -LOW_BITS }
    }

    fn carry(&mut self) {
        for byte in self.output.iter_mut().rev() {
            let (sum, is_over) = byte.overflowing_add(1);
            *byte = sum;
            if !is_over {
                return;
            }
        }
    }

    // Writes a bool whose chance of being false is `probability` in 256; at most one byte is whole after it.
    #[inline]
    pub(super) fn write(&mut self, probability: u8, bit: bool) {
        let split = 1 + (((self.range - 1) * u32::from(probability)) >> PROBABILITY_BITS);
        let range = if bit {
            self.low += split;
            self.range - split
        } else {
            split
        };
        let mut shift = range.leading_zeros() - (u32::BITS - u8::BITS);
        self.range = range << shift;
        self.count += shift as i32;
        if self.count >= 0 {
            let before = shift - self.count as u32;
            if (self.low << (before - 1)) >> (u32::BITS - 1) == 1 {
                self.carry();
            }
            self.output.push((self.low >> (LOW_BITS as u32 - before)) as u8);
            self.low = (self.low << before) & LOW_MASK;
            shift = self.count as u32;
            self.count -= u8::BITS as i32;
        }
        self.low <<= shift;
    }

    pub(super) fn flag(&mut self, bit: bool) {
        self.write(EVEN, bit);
    }

    // L(n): `count` flags of `value`, highest bit first.
    pub(super) fn literal(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            self.flag(value >> bit & 1 == 1);
        }
    }

    // A magnitude of `count` bits followed by its sign.
    pub(super) fn signed(&mut self, value: i32, count: u32) {
        self.literal(value.unsigned_abs(), count);
        self.flag(value < 0);
    }

    // A flag, and where `value` is not nought its magnitude of `count` bits and its sign (RFC 6386, 9.6).
    pub(super) fn delta(&mut self, value: i32, count: u32) {
        self.flag(value != 0);
        if value != 0 {
            self.signed(value, count);
        }
    }

    pub(super) fn tree(&mut self, tree: &[i8], probabilities: &[u8], value: u8) {
        let (steps, depth) = path(tree, value);
        for &(node, bit) in &steps[..depth] {
            self.write(probabilities[node / 2], bit);
        }
    }

    // The bytes written, after enough even flags that every bool written is decided within them.
    pub(super) fn finish(mut self) -> Vec<u8> {
        for _ in 0..u32::BITS {
            self.flag(false);
        }
        self.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn bools_by_hand() {
        // 0xC0: the first bool at even odds splits 255 at 128; 0xC000 is past 0x8000, so it is true.
        let mut decoder = BoolDecoder::new(&[0xC0, 0x00, 0x00]);
        assert!(decoder.flag(), "0xC0 lies in the upper half of the first split");
        // The range is now 127, doubled to 254; the value 0x4000 doubled is 0x8000 and the split of 254 at 128 is 127.
        assert!(decoder.flag(), "0x8000 is at least 127 << 8");
        assert!(!decoder.flag(), "the zeros that follow are false");
        let mut low = BoolDecoder::new(&[0x00, 0x00]);
        assert_eq!(low.literal(8), 0, "eight false bools from zeros");
        let mut skewed = BoolDecoder::new(&[0x10, 0x00]);
        // At a probability of 1 in 256 of being false the split is 1: 0x10 is past it, so the bool is true.
        assert!(skewed.read(1), "a bool almost sure to be true");
        assert!(!skewed.is_past_end(), "two bytes hold the first bools");
    }

    #[test]
    fn encoder_round_trip() {
        let mut encoder = BoolEncoder::new();
        let mut written = Vec::new();
        for index in 0..5000u32 {
            let probability = (index * 37 % 255 + 1) as u8;
            let bit = index.wrapping_mul(2_654_435_761) >> 13 & 3 == 0;
            encoder.write(probability, bit);
            written.push((probability, bit));
        }
        encoder.literal(0x5A5, 11);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        for (index, &(probability, bit)) in written.iter().enumerate() {
            assert_eq!(decoder.read(probability), bit, "bool {index} at probability {probability}");
        }
        assert_eq!(decoder.literal(11), 0x5A5, "a literal after them");
        assert!(!decoder.is_past_end(), "the encoder's flush holds every bit the decoder decides by");
    }

    #[test]
    fn encoder_carries_at_extremes() {
        let mut encoder = BoolEncoder::new();
        let mut written = Vec::new();
        let mut state = 0x1234_5678u32;
        for index in 0..200_000u32 {
            state = crate::base::math::xorshift32(state);
            // Long runs of the unlikely value at the extreme probabilities carry into bytes written long before.
            let probability = [1, 255, 2, 254, 128][(index / 997 % 5) as usize];
            let bit = if index / 499 % 2 == 0 { probability < 128 } else { state & 7 == 0 };
            encoder.write(probability, bit);
            written.push((probability, bit));
        }
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let wrong = written.iter().position(|&(probability, bit)| decoder.read(probability) != bit);
        assert_eq!(wrong, None, "every bool read back, carries and all");
        assert!(!decoder.is_past_end(), "within the bytes written");
    }

    #[test]
    fn probabilities_of_counts() {
        assert_eq!(probability_of([3, 1]), 192, "three false in four");
        assert_eq!(probability_of([0, 9]), 1, "never false is held at 1");
        assert_eq!(probability_of([9, 0]), 255, "always false at 255");
        assert_eq!(probability_of([0, 0]), 128, "no bools: even");
    }

    #[test]
    fn past_end_only_when_used() {
        let mut empty = BoolDecoder::new(&[]);
        assert!(!empty.is_past_end(), "nothing read, nothing missing");
        assert!(!empty.flag(), "zeros stand for the bytes that are not there");
        assert!(empty.is_past_end(), "a bool decided by bits that are not in the partition");
        let mut short = BoolDecoder::new(&[0xFF]);
        assert_eq!(short.literal(4), 0b1111, "the bits of the one byte");
        assert!(short.is_past_end(), "the first bool takes the byte, the second reaches past it");
        let mut long = BoolDecoder::new(&[0x5A; 64]);
        for _ in 0..400 {
            long.flag();
        }
        assert!(!long.is_past_end(), "400 flags fit 512 bits");
        for _ in 0..200 {
            long.flag();
        }
        assert!(long.is_past_end(), "600 do not");
    }

    #[test]
    fn marker_padding_and_end() {
        let mut encoder = BoolEncoder::new();
        encoder.flag(false);
        for index in 0..40u32 {
            encoder.write(200, index % 3 == 0);
        }
        let mut bytes = encoder.finish();
        let mut decoder = BoolDecoder::open(&bytes).unwrap();
        for index in 0..40u32 {
            assert_eq!(decoder.read(200), index % 3 == 0, "bool {index} after the marker");
        }
        assert!(decoder.is_padding_zero(), "the encoder's flush pads with noughts");
        let end = decoder.end();
        assert!(end <= bytes.len() && bytes[end..].iter().all(|&byte| byte == 0), "only padding lies past the end");
        bytes.push(0x80);
        let mut followed = BoolDecoder::open(&bytes).unwrap();
        for _ in 0..40 {
            followed.read(200);
        }
        assert!(!followed.is_padding_zero(), "a byte that is not nought after the padding");
        assert_eq!(followed.end(), end, "the end lies where the bits taken end, whatever follows");
        assert_eq!(BoolDecoder::open(&[]).err(), Some(Flaw::Short), "a partition of no bytes");
        assert_eq!(BoolDecoder::open(&[0xFF, 0]).err(), Some(Flaw::Invalid("marker bit")), "a marker that is true");
    }
}
