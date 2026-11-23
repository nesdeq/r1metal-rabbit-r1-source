use super::bits::Bits;
use alloc::vec;
use alloc::vec::Vec;

// A table's description (RFC 8878, 4.1.1): its log less 5 in 4 bits, and behind a share of nothing a count of up to 3 more in 2 bits.
const LEAST_LOG: u32 = 5;
const LOG_BITS: u32 = 4;
const REPEAT_BITS: u32 = 2;
const MOST_REPEATS: usize = 3;
pub(super) const LITERAL_LENGTHS: [i16; 36] =
    [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
pub(super) const LITERAL_LOG: u32 = 6;
pub(super) const MATCH_LENGTHS: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
pub(super) const MATCH_LOG: u32 = 6;
pub(super) const OFFSETS: [i16; 29] =
    [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];
pub(super) const OFFSET_LOG: u32 = 5;

fn spread(distribution: &[i16], log: u32) -> Vec<u8> {
    let size = 1usize << log;
    let (mask, step) = (size - 1, (size >> 1) + (size >> 3) + 3);
    let mut symbols = vec![0u8; size];
    let mut high = size - 1;
    for (symbol, &share) in distribution.iter().enumerate() {
        if share == -1 {
            symbols[high] = symbol as u8;
            high -= 1;
        }
    }
    let mut position = 0;
    for (symbol, &share) in distribution.iter().enumerate() {
        for _ in 0..share.max(0) {
            symbols[position] = symbol as u8;
            position = (position + step) & mask;
            while position > high {
                position = (position + step) & mask;
            }
        }
    }
    symbols
}

pub(super) struct Coder {
    log: u32,
    states: Vec<u16>,
    symbols: Vec<(u32, i32)>,
}

impl Coder {
    pub(super) fn new(distribution: &[i16], log: u32) -> Coder {
        let size = 1usize << log;
        let mut next = Vec::with_capacity(distribution.len());
        let mut first = 0;
        for &share in distribution {
            next.push(first);
            first += if share == -1 { 1 } else { share.max(0) as usize };
        }
        let mut states = vec![0u16; size];
        for (state, &symbol) in spread(distribution, log).iter().enumerate() {
            states[next[symbol as usize]] = (size + state) as u16;
            next[symbol as usize] += 1;
        }
        let mut total = 0i32;
        let symbols = distribution
            .iter()
            .map(|&share| match share {
                0 => (((log + 1) << 16) - size as u32, 0),
                -1 | 1 => {
                    total += 1;
                    ((log << 16) - size as u32, total - 2)
                }
                share => {
                    let share = share as u32;
                    let max_bits = log - (31 - (share - 1).leading_zeros());
                    let entry = ((max_bits << 16) - (share << max_bits), total - share as i32);
                    total += share as i32;
                    entry
                }
            })
            .collect();
        Coder { log, states, symbols }
    }

    pub(super) fn begin(&self, symbol: usize) -> u32 {
        let (delta, offset) = self.symbols[symbol];
        let count = (delta + (1 << 15)) >> 16;
        let value = (count << 16) - delta;
        u32::from(self.states[((value >> count) as i32 + offset) as usize])
    }

    pub(super) fn code(&self, state: &mut u32, symbol: usize, bits: &mut Bits) {
        let (delta, offset) = self.symbols[symbol];
        let count = (*state + delta) >> 16;
        bits.add(u64::from(*state), count);
        *state = u32::from(self.states[((*state >> count) as i32 + offset) as usize]);
    }

    pub(super) fn end(&self, state: u32, bits: &mut Bits) {
        bits.add(u64::from(state), self.log);
    }
}

pub(super) fn normalize(counts: &[u32], log: u32) -> Option<Vec<i16>> {
    let size = 1i64 << log;
    let total: u64 = counts.iter().map(|&count| u64::from(count)).sum();
    let present = counts.iter().filter(|&&count| count > 0).count() as i64;
    if present < 2 || present > size {
        return None;
    }
    let mut distribution: Vec<i16> = counts
        .iter()
        .map(|&count| if count == 0 { 0 } else { (u64::from(count) * size as u64 / total).max(1) as i16 })
        .collect();
    let mut sum: i64 = distribution.iter().map(|&share| i64::from(share)).sum();
    // What the floor of one gave beyond the table is taken back from the largest shares; what falls short goes to the commonest symbol.
    while sum > size {
        let largest = (0..distribution.len()).max_by_key(|&at| distribution[at])?;
        distribution[largest] -= 1;
        sum -= 1;
    }
    let commonest = (0..counts.len()).max_by_key(|&at| counts[at])?;
    distribution[commonest] += (size - sum) as i16;
    Some(distribution)
}

pub(super) fn describe(distribution: &[i16], log: u32) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.add(u64::from(log - LEAST_LOG), LOG_BITS);
    let size = 1i32 << log;
    let (mut remaining, mut threshold, mut width) = (size + 1, size, log + 1);
    let (mut symbol, mut is_after_zero) = (0, false);
    while symbol < distribution.len() && remaining > 1 {
        if is_after_zero {
            let mut start = symbol;
            while symbol < distribution.len() && distribution[symbol] == 0 {
                symbol += 1;
            }
            // A count of three says that another count follows.
            while symbol >= start + MOST_REPEATS {
                start += MOST_REPEATS;
                bits.add(MOST_REPEATS as u64, REPEAT_BITS);
            }
            bits.add((symbol - start) as u64, REPEAT_BITS);
        }
        let share = i32::from(distribution[symbol]);
        symbol += 1;
        let limit = (2 * threshold - 1) - remaining;
        remaining -= share.abs();
        let mut value = share + 1;
        if value >= threshold {
            value += limit;
        }
        bits.add(value as u64, width - u32::from(value < limit));
        is_after_zero = value == 1;
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
    }
    bits.into_bytes()
}

pub(super) fn code_in_two_states(coder: &Coder, symbols: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    let mut remaining = symbols.len();
    let (mut first, mut second);
    if remaining % 2 == 1 {
        first = coder.begin(symbols[remaining - 1].into());
        second = coder.begin(symbols[remaining - 2].into());
        coder.code(&mut first, symbols[remaining - 3].into(), &mut bits);
        remaining -= 3;
    } else {
        second = coder.begin(symbols[remaining - 1].into());
        first = coder.begin(symbols[remaining - 2].into());
        remaining -= 2;
    }
    while remaining > 0 {
        coder.code(&mut second, symbols[remaining - 1].into(), &mut bits);
        coder.code(&mut first, symbols[remaining - 2].into(), &mut bits);
        remaining -= 2;
    }
    coder.end(second, &mut bits);
    coder.end(first, &mut bits);
    bits.close()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LITERAL_SYMBOLS: [u8; 64] = [
        0, 0, 1, 3, 4, 6, 7, 9, 10, 12, 14, 16, 18, 19, 21, 22, 24, 25, 26, 27, 29, 31, 0, 1, 2, 4, 5, 7, 8, 10, 11,
        13, 16, 17, 19, 20, 22, 23, 25, 25, 26, 28, 30, 0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 15, 17, 18, 20, 21, 23, 24, 35,
        34, 33, 32,
    ];
    const MATCH_SYMBOLS: [u8; 64] = [
        0, 1, 2, 3, 5, 6, 8, 10, 13, 16, 19, 22, 25, 28, 31, 33, 35, 37, 39, 41, 43, 45, 1, 2, 3, 4, 6, 7, 9, 12, 15,
        18, 21, 24, 27, 30, 32, 34, 36, 38, 40, 42, 44, 1, 1, 2, 4, 5, 7, 8, 11, 14, 17, 20, 23, 26, 29, 52, 51, 50,
        49, 48, 47, 46,
    ];
    const OFFSET_SYMBOLS: [u8; 32] = [
        0, 6, 9, 15, 21, 3, 7, 12, 18, 23, 5, 8, 14, 20, 2, 7, 11, 17, 22, 4, 8, 13, 19, 1, 6, 10, 16, 28, 27, 26, 25,
        24,
    ];

    #[test]
    fn predefined_spreads() {
        assert_eq!(spread(&LITERAL_LENGTHS, LITERAL_LOG), LITERAL_SYMBOLS, "the literal lengths' table");
        assert_eq!(spread(&MATCH_LENGTHS, MATCH_LOG), MATCH_SYMBOLS, "the match lengths' table");
        assert_eq!(spread(&OFFSETS, OFFSET_LOG), OFFSET_SYMBOLS, "the offsets' table");
    }

    #[test]
    fn normalize_fills_table() {
        let mut counts = [0u32; 12];
        (counts[0], counts[7], counts[11]) = (40, 3, 1);
        let shares = normalize(&counts, 6).unwrap();
        assert_eq!(shares.iter().map(|&share| i32::from(share)).sum::<i32>(), 64, "the shares fill a table of 64");
        assert!(
            shares[11] >= 1 && shares[7] >= 1 && shares[1..7].iter().all(|&share| share == 0),
            "a symbol counted has a share, the rarest too, and one not counted none"
        );
        assert_eq!(normalize(&[5, 0, 0], 6), None, "one symbol is no distribution");
    }
}
