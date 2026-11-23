//! Words and numbers as the device's texts hold them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Display;
use core::str::FromStr;

const TENTHS_PER_WHOLE: u64 = 10;
const BYTES_PER_TENTH_GIB: u64 = (1 << 30) / 10;
const CAPITALS_BYTES: usize = 3 * char::MAX.len_utf8();
pub(crate) const ON: &str = "on";
pub(crate) const OFF: &str = "off";
pub(crate) const AND: &str = "and";
pub(crate) const OR: &str = "or";
/// The most digits a number of 32 bits has.
pub const DIGITS: usize = u32::MAX.ilog10() as usize + 1;

/// True for text that is not blank.
pub fn has_words(words: &str) -> bool {
    !words.trim().is_empty()
}

pub(crate) fn to_one_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    for (index, word) in text.split_whitespace().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

// Words as a sentence lists them for a model: `a, b and c`, the last behind the conjunction.
pub(crate) fn to_listing(words: impl IntoIterator<Item = impl Display>, conjunction: &str) -> String {
    let words: Vec<String> = words.into_iter().map(|word| format!("{word}")).collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} {conjunction} {last}", rest.join(", ")),
        _ => words.concat(),
    }
}

pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase)
}

pub(crate) fn capitalize_at(text: &mut String, start: usize) {
    let Some(first) = text[start..].chars().next() else {
        return;
    };
    let (mut upper, mut length) = ([0u8; CAPITALS_BYTES], 0);
    for capital in first.to_uppercase() {
        length += capital.encode_utf8(&mut upper[length..]).len();
    }
    if let Ok(upper) = core::str::from_utf8(&upper[..length]) {
        text.replace_range(start..start + first.len_utf8(), upper);
    }
}

pub(crate) fn to_capitalized(name: &str) -> String {
    let mut capitalized = String::from(name);
    capitalize_at(&mut capitalized, 0);
    capitalized
}

pub(crate) fn parse_switch(word: &str) -> Option<bool> {
    match word {
        ON => Some(true),
        OFF => Some(false),
        _ => None,
    }
}

/// The word of a switch.
pub const fn switch_word(is_on: bool) -> &'static str {
    if is_on { ON } else { OFF }
}

pub(crate) fn to_tenths(count: i64) -> String {
    let (sign, magnitude) = (if count < 0 { "-" } else { "" }, count.unsigned_abs());
    format!("{sign}{}.{}", magnitude / TENTHS_PER_WHOLE, magnitude % TENTHS_PER_WHOLE)
}

pub(crate) fn to_gib_text(bytes: u64) -> String {
    to_tenths((bytes / BYTES_PER_TENTH_GIB) as i64)
}

pub(crate) fn parse_number(word: &str) -> Option<u64> {
    match word.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => word.parse().ok(),
    }
}

/// A kernel file's text without surrounding whitespace.
pub fn to_kernel_text(text: &str) -> String {
    String::from(text.trim())
}

/// The number a kernel file holds.
pub fn parse_kernel_number<T: FromStr>(text: &str) -> Option<T> {
    text.trim().parse().ok()
}

/// Error text for a kernel file that holds no number.
pub fn to_no_number_text(path: &str, text: &str) -> String {
    format!("{path}: {:?} is not a number", text.trim())
}

/// A number's decimal digits, written into the end of `room` without allocating.
pub fn write_digits(number: u32, room: &mut [u8; DIGITS]) -> &[u8] {
    let (mut at, mut left) = (DIGITS, number);
    loop {
        at -= 1;
        room[at] = b'0' + (left % 10) as u8;
        left /= 10;
        if left == 0 {
            return &room[at..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_text_and_one_line() {
        assert_eq!(to_one_line("  Lives in\n\t Vienna.  "), "Lives in Vienna.");
        assert_eq!((to_one_line(""), to_one_line(" \n ")), (String::new(), String::new()));
        assert!(has_words(" Ja ") && !has_words(" \n") && !has_words(""));
        assert_eq!(
            [to_capitalized("october"), to_capitalized("friday"), to_capitalized("\u{E4}rger"), to_capitalized("")],
            ["October", "Friday", "\u{C4}rger", ""]
        );
    }

    #[test]
    fn word_listing() {
        assert_eq!(to_listing([""; 0], AND), "");
        assert_eq!(to_listing(["witty"], AND), "witty");
        assert_eq!(to_listing(["witty", "warm"], AND), "witty and warm");
        assert_eq!(to_listing(["witty", "warm", "dry"], AND), "witty, warm and dry");
        assert_eq!(to_listing([1, 2, 5], OR), "1, 2 or 5");
    }

    #[test]
    fn tenths_with_sign() {
        assert_eq!(
            [to_tenths(0), to_tenths(7), to_tenths(125), to_tenths(-125), to_tenths(-3)],
            ["0.0", "0.7", "12.5", "-12.5", "-0.3"]
        );
    }

    #[test]
    fn gib_in_tenths() {
        assert_eq!(
            [0, 1 << 30, 115_320_339_660, (1 << 30) - BYTES_PER_TENTH_GIB].map(to_gib_text),
            ["0.0", "1.0", "107.4", "0.9"]
        );
    }

    #[test]
    fn parses_hex_and_decimal() {
        assert_eq!(
            [parse_number("0x40000"), parse_number("4096"), parse_number("0xd03"), parse_number("0")],
            [Some(262_144), Some(4096), Some(0xd03), Some(0)]
        );
        assert_eq!(
            [parse_number("much"), parse_number("0x"), parse_number("0xfg"), parse_number(""), parse_number("-1")],
            [None; 5]
        );
        assert_eq!(
            [parse_kernel_number(" -352\n"), parse_kernel_number(""), parse_kernel_number("1.5")],
            [Some(-352), None, None],
            "what a kernel's file holds"
        );
        assert_eq!(parse_kernel_number::<u8>("300"), None, "and what it is read as cannot hold");
        assert_eq!(to_kernel_text(" 4.19\n"), "4.19");
    }
}
