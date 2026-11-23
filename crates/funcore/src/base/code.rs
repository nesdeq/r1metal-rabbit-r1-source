//! Bytes as text: hexadecimal, and base64 as RFC 4648 defines it.

use alloc::string::String;
use alloc::vec::Vec;

const HEX: &[u8; 16] = b"0123456789abcdef";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';
const NO_SYMBOL: u8 = u8::MAX;
const UUID_VERSION_AT: usize = 6;
const UUID_VARIANT_AT: usize = 8;
const UUID_VARIANT: u8 = 0x80;
/// The version of a UUID of random bytes.
pub const UUID_RANDOM: u8 = 4;
pub(crate) const UUID_NAMED: u8 = 5;
const SEXTETS: [u8; 256] = {
    let (mut table, mut value) = ([NO_SYMBOL; 256], 0);
    while value < BASE64.len() {
        table[BASE64[value] as usize] = value as u8;
        value += 1;
    }
    table
};

/// Two lower case digits for each byte.
pub fn hex(bytes: &[u8]) -> String {
    let digits = bytes.iter().flat_map(|&byte| [HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
    digits.map(char::from).collect()
}

/// The bytes a string of hex digits stands for; none if it has an odd length or another character.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
    let pairs = text.as_bytes().chunks(2);
    pairs.map(|pair| Some(digit(*pair.first()?)? << 4 | digit(*pair.get(1)?)?)).collect()
}

pub(crate) fn base64_onto(bytes: &[u8], text: &mut String) {
    text.reserve(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let word = group.iter().fold(0u32, |word, &byte| word << 8 | byte as u32) << (8 * (3 - group.len()));
        for (place, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            let sextet = BASE64[(word >> shift) as usize & 63];
            text.push(char::from(if place <= group.len() { sextet } else { PAD }));
        }
    }
}

/// Four characters for every three bytes, padded with `=`.
pub fn base64(bytes: &[u8]) -> String {
    let mut text = String::new();
    base64_onto(bytes, &mut text);
    text
}

pub(crate) fn unbase64_onto(text: &str, bytes: &mut Vec<u8>) -> bool {
    let (text, start) = (text.as_bytes(), bytes.len());
    bytes.reserve(text.len() / 4 * 3);
    let mut is_valid = text.len().is_multiple_of(4);
    for (index, group) in text.as_chunks::<4>().0.iter().enumerate() {
        let is_last = index + 1 == text.len() / 4;
        let padding = group.iter().rev().take_while(|&&symbol| symbol == PAD).count();
        let mut sextets = group[..4 - padding].iter().map(|&symbol| SEXTETS[symbol as usize]);
        let word =
            sextets.try_fold(0u32, |word, sextet| (sextet != NO_SYMBOL).then_some(word << 6 | u32::from(sextet)));
        match word.filter(|_| padding <= 2 && (padding == 0 || is_last)) {
            Some(word) => bytes.extend_from_slice(&(word << (6 * padding)).to_be_bytes()[1..4 - padding]),
            None => is_valid = false,
        }
    }
    if !is_valid {
        bytes.truncate(start);
    }
    is_valid
}

/// The bytes a base64 text stands for; none unless it is whole groups of four with padding only at the end.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    unbase64_onto(text, &mut bytes).then_some(bytes)
}

/// Base64 without its padding.
pub fn base64_unpadded(bytes: &[u8]) -> String {
    let mut text = base64(bytes);
    text.truncate(text.trim_end_matches(char::from(PAD)).len());
    text
}

/// The bytes a base64 text stands for, with its padding or without.
pub fn unbase64_unpadded(text: &str) -> Option<Vec<u8>> {
    let mut padded = String::from(text);
    padded.extend(core::iter::repeat_n(PAD as char, (4 - text.len() % 4) % 4));
    unbase64(&padded)
}

// The text with each byte but letters, digits and those of `kept` written as `%` and two capital hex digits.
pub(crate) fn percent_encoded(text: &str, kept: &[u8]) -> String {
    let mut encoded = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || kept.contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.extend(
                [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]]
                    .map(|digit| char::from(digit.to_ascii_uppercase())),
            );
        }
    }
    encoded
}

/// A UUID of RFC 9562's variant out of 16 bytes, of a version.
pub const fn to_uuid(mut bytes: [u8; 16], version: u8) -> [u8; 16] {
    bytes[UUID_VERSION_AT] = bytes[UUID_VERSION_AT] & 0x0F | version << 4;
    bytes[UUID_VARIANT_AT] = bytes[UUID_VARIANT_AT] & 0x3F | UUID_VARIANT;
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn hex_reads_back() {
        assert_eq!(hex(&[0x00, 0x1f, 0xa0, 0xff]), "001fa0ff");
        assert_eq!(unhex("001FA0ff"), Some(vec![0x00, 0x1f, 0xa0, 0xff]));
        assert_eq!(unhex(""), Some(vec![]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn base64_rfc_4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, coded) in vectors {
            assert_eq!(base64(plain.as_bytes()), coded);
            assert_eq!(unbase64(coded).as_deref(), Some(plain.as_bytes()));
        }
    }

    #[test]
    fn base64_round_trips_every_byte() {
        let bytes: Vec<u8> = (0..=255).collect();
        for length in [254, 255, 256] {
            assert_eq!(unbase64(&base64(&bytes[..length])).as_deref(), Some(&bytes[..length]));
        }
    }

    #[test]
    fn unpadded_base64() {
        assert_eq!(
            ["", "f", "fo", "foo", "foob"].map(|plain| base64_unpadded(plain.as_bytes())),
            ["", "Zg", "Zm8", "Zm9v", "Zm9vYg"]
        );
        assert_eq!(
            ["Zg", "Zm8", "Zm9v", "Zg==", "Zm8="].map(unbase64_unpadded),
            [
                Some(b"f".to_vec()),
                Some(b"fo".to_vec()),
                Some(b"foo".to_vec()),
                Some(b"f".to_vec()),
                Some(b"fo".to_vec())
            ]
        );
        assert_eq!(
            ["Z", "Zm9vZ", "Zm9!", "Zg=="].map(|text| unbase64_unpadded(text).is_none()),
            [true, true, true, false]
        );
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encoded("WEJX/lXe+5Rm==", b"*-._"), "WEJX%2FlXe%2B5Rm%3D%3D");
        assert_eq!(percent_encoded("a b*-._!~'()", b"*-._"), "a%20b*-._%21%7E%27%28%29", "only the bytes kept stay");
        assert_eq!(percent_encoded("ab/c", b"-_.!~*'()"), "ab%2Fc");
        assert_eq!(percent_encoded("\u{e4}", b""), "%C3%A4", "a letter of more than one byte, byte by byte");
        assert_eq!(percent_encoded("", b"-"), "");
    }

    #[test]
    fn uuid_version_and_variant() {
        let id = to_uuid([0xff; 16], UUID_RANDOM);
        assert_eq!((id[6], id[8]), (0x4f, 0xbf), "the version in the high half of its byte, the variant in two bits");
        assert_eq!(to_uuid([0; 16], UUID_RANDOM)[6..9], [0x40, 0, 0x80]);
        assert_eq!(to_uuid([0xff; 16], UUID_NAMED)[6], 0x5f);
    }

    #[test]
    fn invalid_base64_refused() {
        assert_eq!(unbase64("Zm9"), None);
        assert_eq!(unbase64("Zm=v"), None);
        assert_eq!(unbase64("Zg==Zm9v"), None);
        assert_eq!(unbase64("Z==="), None);
        assert_eq!(unbase64("===="), None);
        assert_eq!(unbase64("Zm9!"), None);
        assert_eq!(unbase64("Zm9\u{E4}"), None, "a character of more than one byte");
    }

    #[test]
    fn base64_appends_and_refusal_adds_nothing() {
        let mut text = String::from("data:");
        base64_onto(b"foob", &mut text);
        assert_eq!(text, "data:Zm9vYg==");
        let mut bytes = vec![7u8];
        assert!(unbase64_onto("Zm9vYg==", &mut bytes) && bytes == b"\x07foob");
        assert!(
            !unbase64_onto("Zm9vZm9!", &mut bytes) && bytes == b"\x07foob",
            "the group before the bad one is taken back"
        );
        assert!(unbase64_onto("", &mut bytes) && bytes.len() == 5);
    }
}
