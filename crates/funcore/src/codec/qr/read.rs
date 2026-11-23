use super::{
    ALPHANUMERIC, BYTES, CHARACTER_SET, Canvas, END, FIRST_VERSION_INFO, Level, MASKS, MODE_BITS, Mode, NUMERIC,
    Symbol, VERSIONS, WRONG_BITS, bits_at, codewords, count_bits, field, format_bits, format_places, is_masked,
    parity_layout, short_blocks, to_version, version_bits, version_places,
};
use crate::base::bits::BitReader;
use alloc::vec;
use alloc::vec::Vec;

const ALPHANUMERICS: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// The bytes of a symbol, as it is or as a mirror shows it.
pub fn read(symbol: &Symbol) -> Option<Vec<u8>> {
    read_direct(symbol).or_else(|| read_direct(&symbol.to_transposed()))
}

fn read_direct(symbol: &Symbol) -> Option<Vec<u8>> {
    let version = to_version(symbol.side())?;
    if version >= FIRST_VERSION_INFO && read_version(symbol)? != version {
        return None;
    }
    let (level, mask) = read_format(symbol)?;
    let mut bytes = vec![0u8; codewords(version)];
    for (at, (x, y)) in Canvas::new(version).placement().into_iter().enumerate().take(8 * bytes.len()) {
        if symbol.is_dark(x, y) != is_masked(mask, x, y) {
            bytes[at / 8] |= 0x80 >> (at % 8);
        }
    }
    read_segments(&correct_blocks(&bytes, version, level)?, version)
}

pub(super) fn read_version(symbol: &Symbol) -> Option<u8> {
    let copies = version_places(symbol.side()).map(|places| bits_at(symbol, &places));
    nearest((FIRST_VERSION_INFO..=*VERSIONS.end()).map(|version| (version_bits(version), version)), &copies)
}

fn read_format(symbol: &Symbol) -> Option<(Level, u8)> {
    let copies = format_places(symbol.side()).map(|places| bits_at(symbol, &places));
    let formats = Level::ALL.into_iter().flat_map(|level| (0..MASKS).map(move |mask| (level, mask)));
    nearest(formats.map(|(level, mask)| (format_bits(level, mask), (level, mask))), &copies)
}

fn nearest<T>(codes: impl Iterator<Item = (u32, T)>, copies: &[u32; 2]) -> Option<T> {
    let distance = |code: u32| copies.iter().map(|copy| (copy ^ code).count_ones()).min().unwrap_or(u32::MAX);
    codes
        .map(|(code, value)| (distance(code), value))
        .filter(|(distance, _)| *distance <= WRONG_BITS)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, value)| value)
}

fn correct_blocks(codewords: &[u8], version: u8, level: Level) -> Option<Vec<u8>> {
    let ((parity, blocks), (short, short_data)) = (parity_layout(version, level), short_blocks(version, level));
    let mut pieces: Vec<Vec<u8>> = (0..blocks).map(|_| Vec::with_capacity(short_data + 1 + parity)).collect();
    let mut source = codewords.iter();
    for at in 0..=short_data {
        for (block, piece) in pieces.iter_mut().enumerate() {
            if at < short_data || block >= short {
                piece.push(*source.next()?);
            }
        }
    }
    for _ in 0..parity {
        for piece in &mut pieces {
            piece.push(*source.next()?);
        }
    }
    let mut data = Vec::with_capacity(codewords.len() - parity * blocks);
    for mut piece in pieces {
        if !field::correct(&mut piece, parity) {
            return None;
        }
        data.extend_from_slice(&piece[..piece.len() - parity]);
    }
    Some(data)
}

fn read_segments(data: &[u8], version: u8) -> Option<Vec<u8>> {
    let mut bits = BitReader::new(data);
    let mut output = Vec::new();
    while bits.remaining() >= MODE_BITS {
        match bits.take(MODE_BITS as u32)? {
            END => break,
            NUMERIC => {
                let mut count = bits.take(count_bits(Mode::Numeric, version) as u32)? as usize;
                while count > 0 {
                    // Three digits in ten bits, two in seven, one in four.
                    let (digits, width) = match count {
                        1 => (1, 4),
                        2 => (2, 7),
                        _ => (3, 10),
                    };
                    let value = bits.take(width)?;
                    if value >= 10u32.pow(digits) {
                        return None;
                    }
                    for place in (0..digits).rev() {
                        output.push(b'0' + (value / 10u32.pow(place) % 10) as u8);
                    }
                    count -= digits as usize;
                }
            }
            ALPHANUMERIC => {
                let mut count = bits.take(count_bits(Mode::Alphanumeric, version) as u32)? as usize;
                let character = |value: u32| ALPHANUMERICS.get(value as usize).copied();
                while count >= 2 {
                    let pair = bits.take(11)?;
                    output.extend([character(pair / 45)?, character(pair % 45)?]);
                    count -= 2;
                }
                if count == 1 {
                    output.push(character(bits.take(6)?)?);
                }
            }
            BYTES => {
                let count = bits.take(count_bits(Mode::Bytes, version) as u32)?;
                for _ in 0..count {
                    output.push(bits.take(8)? as u8);
                }
            }
            CHARACTER_SET => {
                // One, two or three bytes, as the bits it begins with say.
                let first = bits.take(8)?;
                let more = match (first << 24).leading_ones() {
                    0 => 0,
                    1 => 8,
                    2 => 16,
                    _ => return None,
                };
                bits.take(more)?;
            }
            _ => return None,
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::qr::make;
    use crate::codec::qr::side;

    fn with_errors(symbol: &Symbol, errors: usize, seed: usize) -> Symbol {
        let mut damaged = symbol.clone();
        let order = Canvas::new(to_version(symbol.side()).unwrap()).placement();
        for at in 0..errors {
            let (x, y) = order[(seed * 7919 + at * 104_729) % order.len()];
            damaged.set(x, y, !damaged.is_dark(x, y));
        }
        damaged
    }

    #[test]
    fn damaged_and_mirrored() {
        let key = b"sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_abcdefghijklmnopqrstuvwxyzAB";
        assert_eq!(key.len(), 164);
        let symbol = make(key, Level::Medium).unwrap();
        for seed in 0..20 {
            assert_eq!(read(&with_errors(&symbol, 20, seed)).as_deref(), Some(&key[..]), "seed {seed}");
        }
        assert_eq!(read(&symbol.to_transposed()).as_deref(), Some(&key[..]), "a mirror shows rows as columns");
        assert_eq!(read(&with_errors(&symbol, 1500, 1)), None, "half of it wrong is no symbol");
    }

    #[test]
    fn format_and_version_errors() {
        let symbol = make(&[7; 300], Level::Quartile).unwrap();
        let version = to_version(symbol.side()).unwrap();
        assert!(version >= FIRST_VERSION_INFO);
        let mut damaged = symbol.clone();
        let ([first, second], [left, below]) = (format_places(symbol.side()), version_places(symbol.side()));
        for &(x, y) in first[..3].iter().chain(&second[5..8]).chain(&left[..3]).chain(&below[9..12]) {
            damaged.set(x, y, !damaged.is_dark(x, y));
        }
        assert_eq!((read_version(&damaged), read_format(&damaged)), (Some(version), read_format(&symbol)));
        assert_eq!(read(&damaged), Some(vec![7; 300]));
        for (x, y) in [first[4], second[0]] {
            damaged.set(x, y, !damaged.is_dark(x, y));
        }
        assert_ne!(read_format(&damaged), read_format(&symbol), "four bits wrong in each copy");
        assert_eq!(read(&damaged), None);
    }

    #[test]
    fn segment_modes() {
        // ISO/IEC 18004, 7.4.3 and 7.4.4: "01234567" and "AC-42", then a byte after the choice of set 26, UTF-8.
        let data = [0x10, 0x20, 0x0C, 0x56, 0x61, 0x90, 0x14, 0xE7, 0x73, 0x90, 0x9C, 0x69, 0x00, 0x58, 0x40];
        assert_eq!(read_segments(&data, 1).as_deref(), Some(&b"01234567AC-42a"[..]));
        assert_eq!(read_segments(&[0b0001_0000, 0b0000_0111, 0b1111_1111], 1), None, "a digit past 9");
        assert_eq!(read_segments(&[0b1000_0000, 0], 1), None, "a mode not read here");
        assert_eq!(read_segments(&[0b0100_0000, 0b1000_0110], 1), None, "a count past the data");
        assert_eq!(read_segments(&[0b0100_0000, 0b0000_0000], 1).as_deref(), Some(&[][..]));
        assert_eq!(side(1), 21);
    }
}
