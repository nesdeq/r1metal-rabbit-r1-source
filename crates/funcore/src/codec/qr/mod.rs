//! QR codes of ISO/IEC 18004, all forty versions: made, read and found in a picture.

mod field;
mod make;
mod read;
mod scan;

pub use make::make;
pub use read::read;
pub use scan::{Scanner, to_luma};

use alloc::vec;
use alloc::vec::Vec;
use core::ops::RangeInclusive;

const VERSIONS: RangeInclusive<u8> = 1..=40;
const FIRST_VERSION_INFO: u8 = 7;
/// Light modules a symbol needs on every side.
pub const QUIET_ZONE: usize = 4;
const WRONG_BITS: u32 = 3;
const FORMAT_CODE: u32 = 0x537;
const VERSION_CODE: u32 = 0x1F25;
const FORMAT_PATTERN: u32 = 0x5412;
const FORMAT_BITS: usize = 15;
const VERSION_BITS: usize = 18;
const MASKS: u8 = 8;
const MODE_BITS: usize = 4;
const END: u32 = 0b0000;
const NUMERIC: u32 = 0b0001;
const ALPHANUMERIC: u32 = 0b0010;
const BYTES: u32 = 0b0100;
const CHARACTER_SET: u32 = 0b0111;

const PARITY: [[u8; 40]; 4] = [
    [
        7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28,
        28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    ],
    [
        13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
];
const BLOCKS: [[u8; 40]; 4] = [
    [
        1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18,
        19, 19, 20, 21, 22, 24, 25,
    ],
    [
        1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31,
        33, 35, 37, 38, 40, 43, 45, 47, 49,
    ],
    [
        1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40,
        43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
    ],
    [
        1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48,
        51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
    ],
];

/// The error correction level: about 7, 15, 25 and 30 percent of a symbol may be lost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Low,
    Medium,
    Quartile,
    High,
}

impl Level {
    const ALL: [Level; 4] = [Level::Low, Level::Medium, Level::Quartile, Level::High];

    const fn bits(self) -> u32 {
        match self {
            Level::Low => 1,
            Level::Medium => 0,
            Level::Quartile => 3,
            Level::High => 2,
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Numeric,
    Alphanumeric,
    Bytes,
}

const fn count_bits(mode: Mode, version: u8) -> usize {
    let class = match version {
        1..=9 => 0,
        10..=26 => 1,
        _ => 2,
    };
    match mode {
        Mode::Numeric => [10, 12, 14][class],
        Mode::Alphanumeric => [9, 11, 13][class],
        Mode::Bytes => [8, 16, 16][class],
    }
}

const fn side(version: u8) -> usize {
    17 + 4 * version as usize
}

fn to_version(side: usize) -> Option<u8> {
    VERSIONS.clone().find(|&version| self::side(version) == side)
}

fn data_modules(version: u8) -> usize {
    let version = usize::from(version);
    // The square of the side less the finders with their separators, the timing, both formats and the dark module; then less the alignment patterns where they leave the timing, and the version's two copies (ISO/IEC 18004, table 1).
    let mut modules = (16 * version + 128) * version + 64;
    if version >= 2 {
        let patterns = version / 7 + 2;
        modules -= (25 * patterns - 10) * patterns - 55;
        if version >= usize::from(FIRST_VERSION_INFO) {
            modules -= 2 * VERSION_BITS;
        }
    }
    modules
}

fn codewords(version: u8) -> usize {
    data_modules(version) / 8
}

fn parity_layout(version: u8, level: Level) -> (usize, usize) {
    let at = usize::from(version - 1);
    (usize::from(PARITY[level as usize][at]), usize::from(BLOCKS[level as usize][at]))
}

fn data_codewords(version: u8, level: Level) -> usize {
    let (parity, blocks) = parity_layout(version, level);
    codewords(version) - parity * blocks
}

fn short_blocks(version: u8, level: Level) -> (usize, usize) {
    let ((parity, blocks), total) = (parity_layout(version, level), codewords(version));
    (blocks - total % blocks, total / blocks - parity)
}

fn alignment_places(version: u8) -> Vec<usize> {
    if version == 1 {
        return Vec::new();
    }
    let count = usize::from(version) / 7 + 2;
    // One even step between every two from the last back, the first gap taking what is left: the table of ISO/IEC 18004, annex E, in one formula.
    let step = (usize::from(version) * 8 + count * 3 + 5) / (count * 4 - 4) * 2;
    let last = side(version) - 7;
    let mut places: Vec<usize> = (0..count - 1).map(|from_last| last - from_last * step).collect();
    places.push(6);
    places.reverse();
    places
}

fn with_bch(data: u32, code: u32) -> u32 {
    let degree = code.ilog2();
    let mut remainder = data;
    for _ in 0..degree {
        remainder = (remainder << 1) ^ ((remainder >> (degree - 1)) * code);
    }
    (data << degree) | remainder
}

fn format_bits(level: Level, mask: u8) -> u32 {
    with_bch((level.bits() << MASKS.ilog2()) | u32::from(mask), FORMAT_CODE) ^ FORMAT_PATTERN
}

fn version_bits(version: u8) -> u32 {
    with_bch(u32::from(version), VERSION_CODE)
}

fn format_places(side: usize) -> [[(usize, usize); FORMAT_BITS]; 2] {
    let first = core::array::from_fn(|bit| match bit {
        0..=5 => (8, bit),
        6 => (8, 7),
        7 => (8, 8),
        8 => (7, 8),
        _ => (14 - bit, 8),
    });
    let second = core::array::from_fn(|bit| if bit < 8 { (side - 1 - bit, 8) } else { (8, side - FORMAT_BITS + bit) });
    [first, second]
}

fn version_places(side: usize) -> [[(usize, usize); VERSION_BITS]; 2] {
    let right = |bit: usize| (side - 11 + bit % 3, bit / 3);
    [core::array::from_fn(right), core::array::from_fn(|bit| (right(bit).1, right(bit).0))]
}

fn bits_at(symbol: &Symbol, places: &[(usize, usize)]) -> u32 {
    places.iter().enumerate().fold(0, |bits, (bit, &(x, y))| bits | (u32::from(symbol.is_dark(x, y)) << bit))
}

const fn is_masked(mask: u8, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (x + y).is_multiple_of(3),
        4 => (x / 3 + y / 2).is_multiple_of(2),
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3).is_multiple_of(2),
        _ => ((x + y) % 2 + x * y % 3).is_multiple_of(2),
    }
}

/// A symbol's square of modules, dark or light, row by row from the top.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Symbol {
    side: usize,
    dark: Vec<bool>,
}

impl Symbol {
    /// A symbol of these modules; none unless they fill a version's square.
    pub fn new(side: usize, dark: Vec<bool>) -> Option<Symbol> {
        (to_version(side).is_some() && dark.len() == side * side).then_some(Symbol { side, dark })
    }

    pub const fn side(&self) -> usize {
        self.side
    }

    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.side + x]
    }

    fn set(&mut self, x: usize, y: usize, is_dark: bool) {
        self.dark[y * self.side + x] = is_dark;
    }

    fn to_transposed(&self) -> Symbol {
        let dark = (0..self.side * self.side).map(|at| self.is_dark(at / self.side, at % self.side)).collect();
        Symbol { side: self.side, dark }
    }
}

#[derive(Clone)]
struct Canvas {
    symbol: Symbol,
    is_function: Vec<bool>,
    version: u8,
}

impl Canvas {
    fn new(version: u8) -> Canvas {
        let side = side(version);
        let symbol = Symbol { side, dark: vec![false; side * side] };
        let mut canvas = Canvas { symbol, is_function: vec![false; side * side], version };
        for at in 0..side {
            canvas.fix(6, at, at % 2 == 0);
            canvas.fix(at, 6, at % 2 == 0);
        }
        // A finder is rings of dark, light and dark about a dark square of three, with a ring of light outside where the symbol goes on.
        for (x, y) in [(3, 3), (side - 4, 3), (3, side - 4)] {
            canvas.put_pattern((x, y), 4, |ring| ring != 2 && ring != 4);
        }
        let places = alignment_places(version);
        let last = places.len().saturating_sub(1);
        for (row, &y) in places.iter().enumerate() {
            for (column, &x) in places.iter().enumerate() {
                // The three corners the finders take have none.
                let is_finder = [(0, 0), (0, last), (last, 0)].contains(&(row, column));
                if !is_finder {
                    canvas.put_pattern((x, y), 2, |ring| ring != 1);
                }
            }
        }
        canvas.put_format(Level::Low, 0);
        if version >= FIRST_VERSION_INFO {
            let bits = version_bits(version);
            for places in version_places(side) {
                for (bit, (x, y)) in places.into_iter().enumerate() {
                    canvas.fix(x, y, bits >> bit & 1 == 1);
                }
            }
        }
        canvas
    }

    fn fix(&mut self, x: usize, y: usize, is_dark: bool) {
        self.symbol.set(x, y, is_dark);
        self.is_function[y * self.symbol.side + x] = true;
    }

    fn put_pattern(&mut self, (x, y): (usize, usize), radius: usize, is_dark: impl Fn(usize) -> bool) {
        for row in y.saturating_sub(radius)..=(y + radius).min(self.symbol.side - 1) {
            for column in x.saturating_sub(radius)..=(x + radius).min(self.symbol.side - 1) {
                self.fix(column, row, is_dark(column.abs_diff(x).max(row.abs_diff(y))));
            }
        }
    }

    fn put_format(&mut self, level: Level, mask: u8) {
        let bits = format_bits(level, mask);
        let side = self.symbol.side;
        for places in format_places(side) {
            for (bit, (x, y)) in places.into_iter().enumerate() {
                self.fix(x, y, bits >> bit & 1 == 1);
            }
        }
        self.fix(8, side - 8, true);
    }

    fn placement(&self) -> Vec<(usize, usize)> {
        let side = self.symbol.side;
        let mut order = Vec::with_capacity(data_modules(self.version));
        let mut right = side - 1;
        loop {
            if right == 6 {
                right = 5;
            }
            let is_upward = (right + 1) & 2 == 0;
            for step in 0..side {
                let y = if is_upward { side - 1 - step } else { step };
                for x in [right, right - 1] {
                    if !self.is_function[y * side + x] {
                        order.push((x, y));
                    }
                }
            }
            if right == 1 {
                return order;
            }
            right -= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_counts() {
        for version in VERSIONS {
            let canvas = Canvas::new(version);
            assert_eq!(canvas.placement().len(), data_modules(version), "version {version}");
            assert!(canvas.symbol.side % 4 == 1 && to_version(canvas.symbol.side) == Some(version));
            for level in Level::ALL {
                let (parity, blocks) = parity_layout(version, level);
                assert!(
                    data_codewords(version, level) > blocks && codewords(version) / blocks <= 255,
                    "version {version} at {level:?}: {parity} of parity in each of {blocks} blocks"
                );
            }
        }
        assert_eq!(
            [1, 2, 6, 7, 14, 21, 40].map(data_modules),
            [208, 359, 1383, 1568, 4651, 9252, 29648],
            "the counts of ISO/IEC 18004, table 1, the remainder bits with them"
        );
        assert_eq!([1, 7, 40].map(codewords), [26, 196, 3706]);
    }

    #[test]
    fn alignment_table() {
        assert_eq!(alignment_places(1), Vec::<usize>::new());
        assert_eq!(alignment_places(2), [6, 18]);
        assert_eq!(alignment_places(7), [6, 22, 38]);
        assert_eq!(alignment_places(9), [6, 26, 46]);
        assert_eq!(alignment_places(15), [6, 26, 48, 70]);
        assert_eq!(alignment_places(32), [6, 34, 60, 86, 112, 138]);
        assert_eq!(alignment_places(36), [6, 24, 50, 76, 102, 128, 154]);
        assert_eq!(alignment_places(40), [6, 30, 58, 86, 114, 142, 170]);
    }

    #[test]
    fn iso_format_and_version_bits() {
        assert_eq!(format_bits(Level::Medium, 5), 0b100_0000_1100_1110, "annex C: M and mask 101");
        assert_eq!(format_bits(Level::Low, 4), 0b110_0110_0010_1111);
        assert_eq!(format_bits(Level::High, 7), 0b000_1000_0011_1011);
        assert_eq!(version_bits(7), 0b000111_110010_010100, "annex D: version 7");
        assert_eq!(version_bits(40), 0b101000_110001_101001);
        let formats: Vec<u32> =
            Level::ALL.iter().flat_map(|&level| (0..MASKS).map(move |mask| format_bits(level, mask))).collect();
        for (at, first) in formats.iter().enumerate() {
            for second in &formats[at + 1..] {
                assert!((first ^ second).count_ones() > 2 * WRONG_BITS, "any two formats lie seven bits apart or more");
            }
        }
        for first in FIRST_VERSION_INFO..=40 {
            for second in first + 1..=40 {
                assert!((version_bits(first) ^ version_bits(second)).count_ones() > 2 * WRONG_BITS);
            }
        }
    }

    #[test]
    fn canvas_patterns_and_transpose() {
        let canvas = Canvas::new(1);
        let symbol = &canvas.symbol;
        let row: Vec<bool> = (0..8).map(|x| symbol.is_dark(x, 0)).collect();
        assert_eq!(row, [true, true, true, true, true, true, true, false], "a finder's top and its separator");
        let middle: Vec<bool> = (0..8).map(|x| symbol.is_dark(x, 3)).collect();
        assert_eq!(middle, [true, false, true, true, true, false, true, false]);
        assert!(symbol.is_dark(8, 21 - 8), "the dark module");
        assert!((8..13).all(|at| symbol.is_dark(at, 6) == (at % 2 == 0)), "the timing between the finders");
        let mirrored = symbol.to_transposed();
        assert!((0..21).all(|y| (0..21).all(|x| mirrored.is_dark(x, y) == symbol.is_dark(y, x))));
        assert_eq!(mirrored.to_transposed(), *symbol);
        assert!(Symbol::new(21, vec![false; 441]).is_some() && Symbol::new(22, vec![false; 484]).is_none());
        assert!(Symbol::new(21, vec![false; 440]).is_none());
    }

    fn full_payload(version: u8, level: Level) -> Vec<u8> {
        let room = (8 * data_codewords(version, level) - MODE_BITS - count_bits(Mode::Bytes, version)) / 8;
        (0..room).map(|at| b"abcdefghijklmnopqrstuvwxyz0123456789"[(at * 7 + usize::from(version)) % 36]).collect()
    }

    const LEVELS: [(Level, &str); 4] =
        [(Level::Low, "L"), (Level::Medium, "M"), (Level::Quartile, "Q"), (Level::High, "H")];

    #[test]
    #[ignore = "has the host's qrencode make a symbol of every version at every level"]
    fn reads_qrencode_symbols() {
        extern crate std;
        use std::io::Write;
        use std::process::{Command, Stdio};
        for version in VERSIONS {
            for (level, letter) in LEVELS {
                let bytes = full_payload(version, level);
                let mut child = Command::new("qrencode")
                    .args(["-t", "ASCII", "-m", "0", "-8", "-v", &std::format!("{version}"), "-l", letter])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .expect("qrencode on the host");
                child.stdin.take().expect("its input").write_all(&bytes).unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(output.status.success());
                let ascii = alloc::string::String::from_utf8(output.stdout).unwrap();
                let side = self::side(version);
                let mut dark = Vec::with_capacity(side * side);
                for line in ascii.lines().take(side) {
                    let line = line.as_bytes();
                    dark.extend((0..side).map(|x| line.get(2 * x) == Some(&b'#')));
                }
                let symbol = Symbol::new(side, dark).expect("a symbol of the version asked for");
                assert_eq!(read::read(&symbol), Some(bytes), "version {version} at {letter}");
            }
        }
    }

    #[test]
    #[ignore = "has the host's zbarimg read a symbol of every version at every level made here"]
    fn zbar_reads_symbols() {
        extern crate std;
        use std::process::Command;
        const PIXELS: usize = 4;
        let folder = std::env::temp_dir().join(std::format!("r1metal-qr-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for version in VERSIONS {
            for (level, letter) in LEVELS {
                let bytes = full_payload(version, level);
                let symbol = make(&bytes, level).unwrap();
                let width = (symbol.side() + 2 * QUIET_ZONE) * PIXELS;
                let picture: Vec<u8> = (0..width * width)
                    .map(|at| {
                        let module = |at: usize| (at / PIXELS).checked_sub(QUIET_ZONE).filter(|at| *at < symbol.side());
                        match (module(at % width), module(at / width)) {
                            (Some(x), Some(y)) if symbol.is_dark(x, y) => 0,
                            _ => 255,
                        }
                    })
                    .collect();
                let path = folder.join(std::format!("{version}{letter}.png"));
                let mut file = Vec::new();
                let mut encoder = png::Encoder::new(&mut file, width as u32, width as u32);
                encoder.set_color(png::ColorType::Grayscale);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().unwrap().write_image_data(&picture).unwrap();
                std::fs::write(&path, &file).unwrap();
                let output =
                    Command::new("zbarimg").args(["--raw", "-q", "-Sbinary"]).arg(&path).output().expect("zbarimg");
                assert!(
                    output.status.success(),
                    "version {version} at {letter}: {}",
                    alloc::string::String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout),
                    &bytes[..],
                    "version {version} at {letter}"
                );
            }
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
