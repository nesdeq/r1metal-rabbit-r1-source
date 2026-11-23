use alloc::vec;
use alloc::vec::Vec;

const SYMBOL_BITS: u32 = u8::BITS;
const SYMBOL_MAX: u32 = (1 << SYMBOL_BITS) - 1;
const CODE_BITS: u32 = u32::BITS;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOTTOM: u32 = CODE_TOP >> SYMBOL_BITS;
const CODE_SHIFT: u32 = CODE_BITS - SYMBOL_BITS - 1;
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYMBOL_BITS + 1;
// The bits of an integer the range coder takes; what an integer has past them goes as raw bits (4.1.5).
const UINT_BITS: u32 = 8;
const WINDOW_BITS: u32 = u32::BITS;
// The bits behind the point of a count of bits in eighths.
pub(super) const FRACTION_BITS: u32 = 3;

pub(super) const fn ilog(value: u32) -> u32 {
    u32::BITS - value.leading_zeros()
}

// The bits used so far in eighths, rounded up, as 4.1.6.2 reckons them from the range.
fn tell_frac(total_bits: u32, range: u32) -> u32 {
    let mut log = ilog(range);
    let mut scaled = range >> (log - 16);
    for _ in 0..FRACTION_BITS {
        scaled = (scaled * scaled) >> 15;
        let bit = scaled >> 16;
        log = log << 1 | bit;
        scaled >>= bit;
    }
    (total_bits << FRACTION_BITS) - log
}

// A range coder as the code that serves both directions sees it: a decoder reads each symbol and gives it, an encoder writes the symbol it is given and gives it back.
pub(super) trait RangeCoder {
    const IS_ENCODER: bool;
    fn bit(&mut self, is_one: bool, logp: u32) -> bool;
    fn icdf(&mut self, symbol: usize, table: &[u8], bits: u32) -> usize;
    fn uint(&mut self, value: u32, total: u32) -> u32;
    fn bits(&mut self, value: u32, count: u32) -> u32;
    // Where the next symbol's frequency lies in `0..total`, or in `0..2^bits`, for a decoder; an encoder finds its symbol from its value.
    fn locate(&mut self, total: u32) -> Option<u32>;
    fn locate_bin(&mut self, bits: u32) -> Option<u32>;
    // Takes the symbol of the frequencies `low..high`.
    fn take(&mut self, low: u32, high: u32, total: u32);
    fn take_bin(&mut self, low: u32, high: u32, bits: u32);
    fn tell(&self) -> u32;
    fn tell_frac(&self) -> u32;
    fn len(&self) -> usize;
    fn exhaust(&mut self);
}

pub(super) struct RangeDecoder<'a> {
    bytes: &'a [u8],
    front: usize,
    back: usize,
    window: u32,
    window_bits: u32,
    total_bits: u32,
    range: u32,
    value: u32,
    scale: u32,
    held: u32,
}

impl<'a> RangeDecoder<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> RangeDecoder<'a> {
        let mut decoder = RangeDecoder {
            bytes,
            front: 0,
            back: 0,
            window: 0,
            window_bits: 0,
            total_bits: CODE_BITS + 1 - (CODE_BITS - CODE_EXTRA) / SYMBOL_BITS * SYMBOL_BITS,
            range: 1 << CODE_EXTRA,
            value: 0,
            scale: 0,
            held: 0,
        };
        decoder.held = decoder.next_front();
        decoder.value = decoder.range - 1 - (decoder.held >> (SYMBOL_BITS - CODE_EXTRA));
        decoder.normalize();
        decoder
    }

    // A frame's bytes past its end read as noughts (4.1.1).
    fn next_front(&mut self) -> u32 {
        let byte = self.bytes.get(self.front).copied().unwrap_or(0);
        self.front += 1;
        u32::from(byte)
    }

    fn next_back(&mut self) -> u32 {
        self.back += 1;
        self.bytes.len().checked_sub(self.back).map_or(0, |at| u32::from(self.bytes[at]))
    }

    fn normalize(&mut self) {
        while self.range <= CODE_BOTTOM {
            self.total_bits += SYMBOL_BITS;
            self.range <<= SYMBOL_BITS;
            let before = self.held;
            self.held = self.next_front();
            let symbol = (before << SYMBOL_BITS | self.held) >> (SYMBOL_BITS - CODE_EXTRA);
            self.value = ((self.value << SYMBOL_BITS) + (SYMBOL_MAX & !symbol)) & (CODE_TOP - 1);
        }
    }

    // Where in `0..total` the next symbol's frequency lies; `update` then takes the symbol.
    fn decode(&mut self, total: u32) -> u32 {
        self.scale = self.range / total;
        total - (self.value / self.scale + 1).min(total)
    }

    fn decode_bin(&mut self, bits: u32) -> u32 {
        self.scale = self.range >> bits;
        (1 << bits) - (self.value / self.scale + 1).min(1 << bits)
    }

    pub(super) fn update(&mut self, low: u32, high: u32, total: u32) {
        let above = self.scale * (total - high);
        self.value -= above;
        self.range = if low > 0 { self.scale * (high - low) } else { self.range - above };
        self.normalize();
    }

    // A bit that is one with a probability of 1 in 2^`logp`.
    pub(super) fn bit(&mut self, logp: u32) -> bool {
        let split = self.range >> logp;
        let is_one = self.value < split;
        if !is_one {
            self.value -= split;
        }
        self.range = if is_one { split } else { self.range - split };
        self.normalize();
        is_one
    }

    // A symbol by a table of what lies above each, of a total of 2^`bits`, ending in nought.
    pub(super) fn icdf(&mut self, table: &[u8], bits: u32) -> usize {
        let step = self.range >> bits;
        let (mut high, mut symbol) = (self.range, 0);
        let low = loop {
            let low = step * u32::from(table[symbol]);
            if self.value >= low {
                break low;
            }
            (high, symbol) = (low, symbol + 1);
        };
        self.value -= low;
        self.range = high - low;
        self.normalize();
        symbol
    }

    // An integer of `0..total`, each as likely as any, the largest where the frame holds one past them; `total` is 2 or more.
    pub(super) fn uint(&mut self, total: u32) -> u32 {
        let largest = total - 1;
        let bits = ilog(largest);
        if bits <= UINT_BITS {
            let symbol = self.decode(total);
            self.update(symbol, symbol + 1, total);
            return symbol;
        }
        let raw = bits - UINT_BITS;
        let high_total = (largest >> raw) + 1;
        let symbol = self.decode(high_total);
        self.update(symbol, symbol + 1, high_total);
        (symbol << raw | self.bits(raw)).min(largest)
    }

    // Raw bits, up to 25, from the frame's end.
    pub(super) fn bits(&mut self, count: u32) -> u32 {
        if self.window_bits < count {
            while self.window_bits <= WINDOW_BITS - SYMBOL_BITS {
                self.window |= self.next_back() << self.window_bits;
                self.window_bits += SYMBOL_BITS;
            }
        }
        let value = self.window & ((1 << count) - 1);
        self.window >>= count;
        self.window_bits -= count;
        self.total_bits += count;
        value
    }

    // The whole bits used so far, rounded up.
    pub(super) const fn tell(&self) -> u32 {
        self.total_bits - ilog(self.range)
    }

    pub(super) fn tell_frac(&self) -> u32 {
        tell_frac(self.total_bits, self.range)
    }

    // Counts every bit of the frame as used, as a silent CELT frame has it (4.3).
    pub(super) fn exhaust(&mut self) {
        self.total_bits = self.bytes.len() as u32 * SYMBOL_BITS + ilog(self.range);
    }

    // The range as the symbols read so far leave it: after a frame's last, its final range (6).
    pub(super) const fn range(&self) -> u32 {
        self.range
    }

    pub(super) const fn len(&self) -> usize {
        self.bytes.len()
    }

    // Takes `count` bytes off the frame's end, which raw bits are then read before.
    pub(super) fn shorten(&mut self, count: usize) {
        self.bytes = &self.bytes[..self.bytes.len().saturating_sub(count)];
    }
}

impl RangeCoder for RangeDecoder<'_> {
    const IS_ENCODER: bool = false;

    fn bit(&mut self, _: bool, logp: u32) -> bool {
        RangeDecoder::bit(self, logp)
    }

    fn icdf(&mut self, _: usize, table: &[u8], bits: u32) -> usize {
        RangeDecoder::icdf(self, table, bits)
    }

    fn uint(&mut self, _: u32, total: u32) -> u32 {
        RangeDecoder::uint(self, total)
    }

    fn bits(&mut self, _: u32, count: u32) -> u32 {
        RangeDecoder::bits(self, count)
    }

    fn locate(&mut self, total: u32) -> Option<u32> {
        Some(self.decode(total))
    }

    fn locate_bin(&mut self, bits: u32) -> Option<u32> {
        Some(self.decode_bin(bits))
    }

    fn take(&mut self, low: u32, high: u32, total: u32) {
        self.update(low, high, total);
    }

    fn take_bin(&mut self, low: u32, high: u32, bits: u32) {
        self.update(low, high, 1 << bits);
    }

    fn tell(&self) -> u32 {
        RangeDecoder::tell(self)
    }

    fn tell_frac(&self) -> u32 {
        RangeDecoder::tell_frac(self)
    }

    fn len(&self) -> usize {
        RangeDecoder::len(self)
    }

    fn exhaust(&mut self) {
        RangeDecoder::exhaust(self);
    }
}

pub(super) struct RangeEncoder {
    bytes: Vec<u8>,
    front: usize,
    back: usize,
    window: u32,
    window_bits: u32,
    total_bits: u32,
    range: u32,
    value: u32,
    carries: u32,
    held: Option<u32>,
    is_broken: bool,
}

impl RangeEncoder {
    // An encoder of a frame of that many bytes.
    pub(super) fn new(len: usize) -> RangeEncoder {
        RangeEncoder {
            bytes: vec![0; len],
            front: 0,
            back: 0,
            window: 0,
            window_bits: 0,
            total_bits: CODE_BITS + 1,
            range: CODE_TOP,
            value: 0,
            carries: 0,
            held: None,
            is_broken: false,
        }
    }

    // Makes the encoder anew for a frame of that many bytes, in the room its bytes already have.
    pub(super) fn reset(&mut self, len: usize) {
        let mut bytes = core::mem::take(&mut self.bytes);
        bytes.clear();
        bytes.resize(len, 0);
        *self = RangeEncoder { bytes, ..RangeEncoder::new(0) };
    }

    fn put_front(&mut self, byte: u32) {
        if self.front + self.back >= self.bytes.len() {
            self.is_broken = true;
            return;
        }
        self.bytes[self.front] = byte as u8;
        self.front += 1;
    }

    fn put_back(&mut self, byte: u32) {
        if self.front + self.back >= self.bytes.len() {
            self.is_broken = true;
            return;
        }
        self.back += 1;
        let at = self.bytes.len() - self.back;
        self.bytes[at] = byte as u8;
    }

    // A byte of all ones waits, counted, until a later symbol tells whether a carry turns it over (5.1.1.2).
    fn carry_out(&mut self, symbol: u32) {
        if symbol == SYMBOL_MAX {
            self.carries += 1;
            return;
        }
        let carry = symbol >> SYMBOL_BITS;
        if let Some(held) = self.held {
            self.put_front(held + carry);
        }
        for _ in 0..core::mem::take(&mut self.carries) {
            self.put_front((SYMBOL_MAX + carry) & SYMBOL_MAX);
        }
        self.held = Some(symbol & SYMBOL_MAX);
    }

    fn normalize(&mut self) {
        while self.range <= CODE_BOTTOM {
            self.carry_out(self.value >> CODE_SHIFT);
            self.value = (self.value << SYMBOL_BITS) & (CODE_TOP - 1);
            self.range <<= SYMBOL_BITS;
            self.total_bits += SYMBOL_BITS;
        }
    }

    fn narrow(&mut self, low: u32, high: u32, total: u32, scale: u32) {
        if low > 0 {
            self.value += self.range - scale * (total - low);
            self.range = scale * (high - low);
        } else {
            self.range -= scale * (total - high);
        }
        self.normalize();
    }

    fn encode(&mut self, low: u32, high: u32, total: u32) {
        self.narrow(low, high, total, self.range / total);
    }

    pub(super) fn encode_bin(&mut self, low: u32, high: u32, bits: u32) {
        self.narrow(low, high, 1 << bits, self.range >> bits);
    }

    pub(super) fn bit(&mut self, is_one: bool, logp: u32) {
        let split = self.range >> logp;
        if is_one {
            self.value += self.range - split;
            self.range = split;
        } else {
            self.range -= split;
        }
        self.normalize();
    }

    pub(super) fn icdf(&mut self, symbol: usize, table: &[u8], bits: u32) {
        let step = self.range >> bits;
        let low = step * u32::from(table[symbol]);
        if symbol > 0 {
            let high = step * u32::from(table[symbol - 1]);
            self.value += self.range - high;
            self.range = high - low;
        } else {
            self.range -= low;
        }
        self.normalize();
    }

    pub(super) fn uint(&mut self, value: u32, total: u32) {
        let largest = total - 1;
        let bits = ilog(largest);
        if bits <= UINT_BITS {
            return self.encode(value, value + 1, total);
        }
        let raw = bits - UINT_BITS;
        let high = value >> raw;
        self.encode(high, high + 1, (largest >> raw) + 1);
        self.bits(value & ((1 << raw) - 1), raw);
    }

    pub(super) fn bits(&mut self, value: u32, count: u32) {
        if self.window_bits + count > WINDOW_BITS {
            while self.window_bits >= SYMBOL_BITS {
                self.put_back(self.window & SYMBOL_MAX);
                self.window >>= SYMBOL_BITS;
                self.window_bits -= SYMBOL_BITS;
            }
        }
        self.window |= value << self.window_bits;
        self.window_bits += count;
        self.total_bits += count;
    }

    pub(super) const fn tell(&self) -> u32 {
        self.total_bits - ilog(self.range)
    }

    pub(super) fn tell_frac(&self) -> u32 {
        tell_frac(self.total_bits, self.range)
    }

    pub(super) const fn len(&self) -> usize {
        self.bytes.len()
    }

    // The range as the symbols written so far leave it: after a frame's last, its final range (6).
    pub(super) const fn range(&self) -> u32 {
        self.range
    }

    // Counts every bit of the frame as used, as a silent CELT frame has it (4.3).
    pub(super) fn exhaust(&mut self) {
        self.total_bits = self.bytes.len() as u32 * SYMBOL_BITS + ilog(self.range);
    }

    // The frame, of the bytes the encoder was made for; none where they did not hold what was coded.
    pub(super) fn finish(&mut self) -> Option<&[u8]> {
        let mut spare = CODE_BITS - ilog(self.range);
        let mut mask = (CODE_TOP - 1) >> spare;
        let mut end = (self.value + mask) & !mask;
        if u64::from(end | mask) >= u64::from(self.value) + u64::from(self.range) {
            spare += 1;
            mask >>= 1;
            end = (self.value + mask) & !mask;
        }
        let mut left = spare as i32;
        while left > 0 {
            self.carry_out(end >> CODE_SHIFT);
            end = (end << SYMBOL_BITS) & (CODE_TOP - 1);
            left -= SYMBOL_BITS as i32;
        }
        if self.held.is_some() || self.carries > 0 {
            self.carry_out(0);
        }
        while self.window_bits >= SYMBOL_BITS {
            self.put_back(self.window & SYMBOL_MAX);
            self.window >>= SYMBOL_BITS;
            self.window_bits -= SYMBOL_BITS;
        }
        if self.is_broken {
            return None;
        }
        if self.window_bits > 0 {
            if self.back >= self.bytes.len() {
                return None;
            }
            // The last raw bits share a byte with the range coder's end only where its unused bits hold them.
            let unused = (-left) as u32;
            if self.front + self.back >= self.bytes.len() && unused < self.window_bits {
                return None;
            }
            let at = self.bytes.len() - self.back - 1;
            self.bytes[at] |= self.window as u8;
        }
        Some(&self.bytes)
    }
}

// A copy that goes into the room the target's bytes already have: a coder kept for a trial of two ways to code.
impl Clone for RangeEncoder {
    fn clone(&self) -> RangeEncoder {
        RangeEncoder { bytes: self.bytes.clone(), ..*self }
    }

    fn clone_from(&mut self, source: &RangeEncoder) {
        let mut bytes = core::mem::take(&mut self.bytes);
        bytes.clone_from(&source.bytes);
        *self = RangeEncoder { bytes, ..*source };
    }
}

impl RangeCoder for RangeEncoder {
    const IS_ENCODER: bool = true;

    fn bit(&mut self, is_one: bool, logp: u32) -> bool {
        RangeEncoder::bit(self, is_one, logp);
        is_one
    }

    fn icdf(&mut self, symbol: usize, table: &[u8], bits: u32) -> usize {
        RangeEncoder::icdf(self, symbol, table, bits);
        symbol
    }

    fn uint(&mut self, value: u32, total: u32) -> u32 {
        RangeEncoder::uint(self, value, total);
        value
    }

    fn bits(&mut self, value: u32, count: u32) -> u32 {
        RangeEncoder::bits(self, value, count);
        value
    }

    fn locate(&mut self, _: u32) -> Option<u32> {
        None
    }

    fn locate_bin(&mut self, _: u32) -> Option<u32> {
        None
    }

    fn take(&mut self, low: u32, high: u32, total: u32) {
        self.encode(low, high, total);
    }

    fn take_bin(&mut self, low: u32, high: u32, bits: u32) {
        self.encode_bin(low, high, bits);
    }

    fn tell(&self) -> u32 {
        RangeEncoder::tell(self)
    }

    fn tell_frac(&self) -> u32 {
        RangeEncoder::tell_frac(self)
    }

    fn len(&self) -> usize {
        RangeEncoder::len(self)
    }

    fn exhaust(&mut self) {
        RangeEncoder::exhaust(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;

    const TABLE: [u8; 5] = [200, 120, 40, 8, 0];
    const TABLE_BITS: u32 = 8;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Symbol {
        Frequency(u32, u32, u32),
        Bin(u32, u32),
        Bit(bool, u32),
        Icdf(usize),
        Uint(u32, u32),
        Raw(u32, u32),
    }

    fn symbols(count: usize, seed: u32) -> Vec<Symbol> {
        let mut state = seed;
        let mut next = move |below: u32| {
            state = xorshift32(state);
            state % below
        };
        (0..count)
            .map(|_| match next(6) {
                0 => {
                    let total = 2 + next(1000);
                    let low = next(total);
                    Symbol::Frequency(low, low + 1 + next(total - low), total)
                }
                1 => {
                    let bits = 1 + next(15);
                    Symbol::Bin(next(1 << bits), bits)
                }
                2 => {
                    let logp = 1 + next(15);
                    Symbol::Bit(next(1 << logp) == 0, logp)
                }
                3 => Symbol::Icdf(next(TABLE.len() as u32) as usize),
                4 => {
                    let bits = 1 + next(24);
                    let total = 2 + next(1 << bits);
                    Symbol::Uint(next(total), total)
                }
                _ => {
                    let count = 1 + next(25);
                    Symbol::Raw(next(1 << count), count)
                }
            })
            .collect()
    }

    fn encode(symbols: &[Symbol], len: usize) -> (Option<Vec<u8>>, Vec<(u32, u32)>) {
        let mut encoder = RangeEncoder::new(len);
        let mut tells = Vec::new();
        for symbol in symbols {
            match *symbol {
                Symbol::Frequency(low, high, total) => encoder.encode(low, high, total),
                Symbol::Bin(low, bits) => encoder.encode_bin(low, low + 1, bits),
                Symbol::Bit(is_one, logp) => encoder.bit(is_one, logp),
                Symbol::Icdf(symbol) => encoder.icdf(symbol, &TABLE, TABLE_BITS),
                Symbol::Uint(value, total) => encoder.uint(value, total),
                Symbol::Raw(value, count) => encoder.bits(value, count),
            }
            tells.push((encoder.tell(), encoder.tell_frac()));
        }
        (encoder.finish().map(<[u8]>::to_vec), tells)
    }

    #[test]
    fn symbols_round_trip() {
        for seed in 1..=40 {
            let symbols = symbols(400, seed);
            let (frame, tells) = encode(&symbols, 2000);
            let frame = frame.unwrap();
            let mut decoder = RangeDecoder::new(&frame);
            for (at, (symbol, tell)) in symbols.iter().zip(tells).enumerate() {
                let read = match *symbol {
                    Symbol::Frequency(low, high, total) => {
                        let found = decoder.decode(total);
                        assert!((low..high).contains(&found), "seed {seed}, symbol {at}");
                        decoder.update(low, high, total);
                        *symbol
                    }
                    Symbol::Bin(_, bits) => {
                        let low = decoder.decode_bin(bits);
                        decoder.update(low, low + 1, 1 << bits);
                        Symbol::Bin(low, bits)
                    }
                    Symbol::Bit(_, logp) => Symbol::Bit(decoder.bit(logp), logp),
                    Symbol::Icdf(_) => Symbol::Icdf(decoder.icdf(&TABLE, TABLE_BITS)),
                    Symbol::Uint(_, total) => Symbol::Uint(decoder.uint(total), total),
                    Symbol::Raw(_, count) => Symbol::Raw(decoder.bits(count), count),
                };
                assert_eq!(read, *symbol, "seed {seed}, symbol {at}");
                assert_eq!(
                    (decoder.tell(), decoder.tell_frac()),
                    tell,
                    "seed {seed}, symbol {at}: both sides count the same bits"
                );
            }
        }
    }

    #[test]
    fn bits_counted_from_one() {
        let encoder = RangeEncoder::new(8);
        assert_eq!((encoder.tell(), encoder.tell_frac(), encoder.len()), (1, 8, 8));
        let decoder = RangeDecoder::new(&[0; 8]);
        assert_eq!((decoder.tell(), decoder.tell_frac(), decoder.len()), (1, 8, 8));
        let mut encoder = RangeEncoder::new(8);
        encoder.bit(false, 1);
        assert_eq!((encoder.tell(), encoder.tell_frac()), (2, 16), "a bit of even odds is one bit");
        encoder.encode(0, 1, 3);
        assert_eq!((encoder.tell(), encoder.tell_frac()), (4, 29), "a third takes 1.585 bits, in eighths rounded up");
    }

    #[test]
    fn frame_too_short_or_past_its_end() {
        let symbols = symbols(400, 7);
        assert_eq!(encode(&symbols, 20).0, None, "what does not fit makes no frame");
        let mut decoder = RangeDecoder::new(&[]);
        assert_eq!(decoder.bits(8), 0, "past a frame's end the raw bits are noughts");
        assert!(!decoder.bit(1), "and the range coder's bytes too");
        let mut decoder = RangeDecoder::new(&[0xFF; 4]);
        assert_eq!(decoder.uint(1 << 20 | 1), 1 << 20, "an integer past its range is the largest");
    }

    #[test]
    fn raw_bits_from_the_end() {
        let mut encoder = RangeEncoder::new(4);
        encoder.bits(0b101, 3);
        encoder.bits(0xAB, 8);
        let frame = encoder.finish().unwrap();
        assert_eq!(frame[2..], [0b101, 0b0101_1101], "lowest bit first, last byte first");
        let mut decoder = RangeDecoder::new(frame);
        assert_eq!((decoder.bits(3), decoder.bits(8)), (0b101, 0xAB));
        let mut cut = RangeDecoder::new(frame);
        cut.shorten(1);
        assert_eq!((cut.len(), cut.bits(3)), (3, 0b101), "a frame cut short ends where it is cut");
    }
}
