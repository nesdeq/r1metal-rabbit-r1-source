use super::field::{divisor, remainder};
use super::{
    BYTES, Canvas, Level, MASKS, MODE_BITS, Mode, Symbol, VERSIONS, codewords, count_bits, data_codewords, is_masked,
    parity_layout, short_blocks,
};
use crate::base::bits::BitWriter;
use alloc::vec::Vec;

const PADDING: [u8; 2] = [0xEC, 0x11];
const LEAST_RUN: usize = 5;
const RUN_PENALTY: u32 = 3;
const SQUARE_PENALTY: u32 = 3;
const FINDER_PENALTY: u32 = 40;
const BALANCE_PENALTY: u32 = 10;
const FINDER_PATTERN: [bool; 11] = [true, false, true, true, true, false, true, false, false, false, false];

/// The symbol of `bytes` in the smallest version with room for them at `level`.
pub fn make(bytes: &[u8], level: Level) -> Option<Symbol> {
    let length = u32::try_from(bytes.len()).ok()?;
    let fits = |version: u8| {
        let count = count_bits(Mode::Bytes, version);
        length >> count == 0 && MODE_BITS + count + 8 * bytes.len() <= 8 * data_codewords(version, level)
    };
    let version = VERSIONS.clone().find(|&version| fits(version))?;
    let room = data_codewords(version, level);
    let mut bits = BitWriter::default();
    bits.put(BYTES, MODE_BITS as u32);
    bits.put(length, count_bits(Mode::Bytes, version) as u32);
    bytes.iter().for_each(|&byte| bits.put(byte.into(), 8));
    // The segment ends with as many of the four nought bits of the end as there is room for, and noughts to the byte.
    bits.put(0, (8 * room - bits.bit_count()).min(MODE_BITS) as u32);
    let mut data = bits.finish();
    data.extend(PADDING.iter().cycle().take(room - data.len()));
    let codewords = interleave(&data, version, level);
    let mut canvas = Canvas::new(version);
    let order = canvas.placement();
    for (at, &(x, y)) in order.iter().enumerate().take(8 * codewords.len()) {
        canvas.symbol.set(x, y, codewords[at / 8] >> (7 - at % 8) & 1 == 1);
    }
    let to_masked = |mask: u8| {
        let mut masked = canvas.clone();
        for &(x, y) in &order {
            if is_masked(mask, x, y) {
                masked.symbol.set(x, y, !masked.symbol.is_dark(x, y));
            }
        }
        masked.put_format(level, mask);
        masked.symbol
    };
    (0..MASKS).map(to_masked).min_by_key(penalty)
}

fn interleave(data: &[u8], version: u8, level: Level) -> Vec<u8> {
    let (parity, blocks) = parity_layout(version, level);
    let (total, (short, short_data)) = (codewords(version), short_blocks(version, level));
    let divisor = divisor(parity);
    let mut start = 0;
    let pieces: Vec<(&[u8], Vec<u8>)> = (0..blocks)
        .map(|block| {
            let piece = &data[start..start + short_data + usize::from(block >= short)];
            start += piece.len();
            (piece, remainder(piece, &divisor))
        })
        .collect();
    let mut codewords = Vec::with_capacity(total);
    for at in 0..=short_data {
        codewords.extend(pieces.iter().filter_map(|(piece, _)| piece.get(at)));
    }
    for at in 0..parity {
        codewords.extend(pieces.iter().map(|(_, parity)| parity[at]));
    }
    codewords
}

fn penalty(symbol: &Symbol) -> u32 {
    let side = symbol.side();
    let mut sum = 0;
    let reversed: [bool; 11] = core::array::from_fn(|at| FINDER_PATTERN[FINDER_PATTERN.len() - 1 - at]);
    for line in 0..side {
        for is_row in [true, false] {
            let is_dark = |at: usize| if is_row { symbol.is_dark(at, line) } else { symbol.is_dark(line, at) };
            let mut run = 1;
            for at in 1..=side {
                if at < side && is_dark(at) == is_dark(at - 1) {
                    run += 1;
                    continue;
                }
                if run >= LEAST_RUN {
                    sum += RUN_PENALTY + (run - LEAST_RUN) as u32;
                }
                run = 1;
            }
            for start in 0..=side - FINDER_PATTERN.len() {
                let matches =
                    |pattern: &[bool; 11]| pattern.iter().enumerate().all(|(at, &dark)| is_dark(start + at) == dark);
                if matches(&FINDER_PATTERN) || matches(&reversed) {
                    sum += FINDER_PENALTY;
                }
            }
        }
    }
    for y in 1..side {
        for x in 1..side {
            let dark = symbol.is_dark(x, y);
            if [(x - 1, y), (x, y - 1), (x - 1, y - 1)].iter().all(|&(x, y)| symbol.is_dark(x, y) == dark) {
                sum += SQUARE_PENALTY;
            }
        }
    }
    let (dark, total) = ((0..side * side).filter(|&at| symbol.is_dark(at % side, at / side)).count(), side * side);
    // A symbol's side is odd: its dark modules are never exactly half of all of them.
    let steps = (20 * dark).abs_diff(10 * total).div_ceil(total) - 1;
    sum + BALANCE_PENALTY * steps as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::qr::read;

    #[test]
    fn iso_example() {
        let mut bits = BitWriter::default();
        bits.put(0b0001, 4);
        bits.put(8, 10);
        for (digits, count) in [(12, 10), (345, 10), (67, 7)] {
            bits.put(digits, count);
        }
        assert_eq!(
            (bits.bit_count(), bits.finish()),
            (41, alloc::vec![0x10, 0x20, 0x0C, 0x56, 0x61, 0x80]),
            "annex I: 01234567 in the numeric mode, before its end"
        );
        let symbol = make(b"01234567", Level::Medium).unwrap();
        assert_eq!(symbol.side(), 21);
        assert_eq!(read(&symbol).as_deref(), Some(&b"01234567"[..]));
    }

    #[test]
    fn smallest_version() {
        let capacity = |version: u8, level: Level| {
            (8 * data_codewords(version, level) - MODE_BITS - count_bits(Mode::Bytes, version)) / 8
        };
        assert_eq!(
            [Level::Low, Level::Medium, Level::Quartile, Level::High].map(|level| capacity(1, level)),
            [17, 14, 11, 7],
            "version 1 holds 17, 14, 11 and 7 bytes"
        );
        assert_eq!(
            (capacity(9, Level::Medium), capacity(10, Level::Medium), capacity(40, Level::Low)),
            (180, 213, 2953)
        );
        let key = [b'k'; 164];
        assert_eq!(
            make(&key, Level::Medium).map(|symbol| symbol.side()),
            Some(53),
            "a project key at the middle level"
        );
        assert_eq!(make(&[0; 180], Level::Medium).map(|symbol| symbol.side()), Some(53));
        assert_eq!(make(&[0; 181], Level::Medium).map(|symbol| symbol.side()), Some(57));
        assert_eq!(make(&[0; 2953], Level::Low).map(|symbol| symbol.side()), Some(177));
        assert!(make(&[0; 2954], Level::Low).is_none(), "more than version 40 holds");
        assert!(make(&[], Level::High).is_some_and(|symbol| symbol.side() == 21));
    }

    #[test]
    fn round_trip_all_versions() {
        for level in Level::ALL {
            for version in VERSIONS {
                let room = (8 * data_codewords(version, level) - MODE_BITS - count_bits(Mode::Bytes, version)) / 8;
                let bytes: Vec<u8> = (0..room).map(|at| (at * 151 + usize::from(version) * 7) as u8).collect();
                let symbol = make(&bytes, level).unwrap();
                assert_eq!(symbol.side(), super::super::side(version), "{room} bytes at {level:?}");
                assert_eq!(read(&symbol), Some(bytes), "version {version} at {level:?}");
            }
        }
    }

    #[test]
    fn mask_penalty() {
        let symbol = |dark: fn(usize, usize) -> bool| {
            Symbol::new(21, (0..21 * 21).map(|at| dark(at % 21, at / 21)).collect()).unwrap()
        };
        assert_eq!(
            penalty(&symbol(|_, _| false)),
            42 * (RUN_PENALTY + 16) + 400 * SQUARE_PENALTY + 9 * BALANCE_PENALTY,
            "all light: a run of 21 along every row and column, every square alike, and none dark"
        );
        assert_eq!(penalty(&symbol(|x, y| (x + y).is_multiple_of(2))), 0, "a chequer has nothing against it");
        let finder_row =
            |x: usize, y: usize| if y == 0 { x < 11 && FINDER_PATTERN[x] } else { (x + y).is_multiple_of(2) };
        let with_finder = penalty(&symbol(finder_row));
        assert!(
            (FINDER_PENALTY..FINDER_PENALTY + 3 * RUN_PENALTY + 4 * SQUARE_PENALTY).contains(&with_finder),
            "a row that looks like a finder: {with_finder}"
        );
    }
}
