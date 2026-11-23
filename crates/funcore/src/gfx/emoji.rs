//! The table of Noto Emoji's pictures, as `examples/glyphs.rs` writes it: a count, then a record an emoji, in the order of their keys.

use crate::base::pixel::Color;
use core::cmp::Ordering;
use core::ops::RangeInclusive;

static TABLE: &[u8] = include_bytes!("../../assets/noto/emoji.bin");
const COUNT_BYTES: usize = 4;
/// The characters of the longest sequence a key holds.
pub const KEY_CODES: usize = 7;
/// The bytes a key holds a character's number in, highest first.
pub const CODE_BYTES: usize = 3;
const KEY_BYTES: usize = KEY_CODES * CODE_BYTES;
/// The colours of a picture's palette, beside none.
pub const COLOURS: usize = 15;
const PALETTE_BYTES: usize = COLOURS * 3;
/// The sides of the pictures a record holds, one for each font: as tall as its cell and two of its cells wide.
pub const SIDES: [usize; 2] = [24, 32];
/// The selector of a picture, which no key holds.
pub const PICTURE: char = '\u{FE0F}';
/// The tones of skin, which no key holds.
pub const TONES: RangeInclusive<char> = '\u{1F3FB}'..='\u{1F3FF}';
const RECORD_BYTES: usize = KEY_BYTES + 1 + PALETTE_BYTES + SIDES[0] * SIDES[0] / 2 + SIDES[1] * SIDES[1] / 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Emoji {
    record: &'static [u8],
}

impl core::fmt::Debug for Emoji {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_list().entries(self.record[..KEY_BYTES].chunks(CODE_BYTES).map(to_code)).finish()
    }
}

fn to_code(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |code, byte| code << u8::BITS | u32::from(*byte))
}

impl Emoji {
    // Whether it shows as a picture where nothing asks for one: Unicode's Emoji_Presentation.
    pub(super) fn is_shown_bare(&self) -> bool {
        self.record[KEY_BYTES] != 0
    }

    // The side of the largest picture that fills `height` a whole number of times, else of the largest no taller.
    pub(super) fn side(height: i32) -> usize {
        let sides = || SIDES.into_iter().rev().filter(|side| *side as i32 <= height);
        sides().find(|side| height % *side as i32 == 0).or_else(|| sides().next()).unwrap_or(SIDES[0])
    }

    // The colour at a place of its picture of that side, none where the picture leaves the ground.
    pub(super) fn pixel(&self, side: usize, x: usize, y: usize) -> Option<Color> {
        let before: usize = SIDES.into_iter().take_while(|other| *other != side).map(|other| other * other / 2).sum();
        let palette = &self.record[KEY_BYTES + 1..][..PALETTE_BYTES];
        let pair = self.record[KEY_BYTES + 1 + PALETTE_BYTES + before + (y * side + x) / 2];
        let index = usize::from(if x.is_multiple_of(2) { pair >> 4 } else { pair & 0xF });
        let colour = palette.chunks(3).nth(index.checked_sub(1)?)?;
        Some(Color(to_code(colour)))
    }
}

fn records() -> &'static [[u8; RECORD_BYTES]] {
    TABLE.get(COUNT_BYTES..).unwrap_or(&[]).as_chunks().0
}

// The emoji of these characters, as its sequence stands without the selector of a picture and the tones of skin.
pub(super) fn find(characters: &[char]) -> Option<Emoji> {
    let mut key = [0u8; KEY_BYTES];
    for (code, character) in key.chunks_mut(CODE_BYTES).zip(characters) {
        code.copy_from_slice(&(*character as u32).to_be_bytes()[1..]);
    }
    let records = records();
    let (mut low, mut high) = (0, records.len());
    while low < high && characters.len() <= KEY_CODES {
        let middle = low + (high - low) / 2;
        let record = records.get(middle)?.as_slice();
        match record[..KEY_BYTES].cmp(&key) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Some(Emoji { record }),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::font::{spleen_12x24, spleen_16x32};
    use alloc::vec::Vec;

    fn keys() -> impl Iterator<Item = Vec<char>> {
        records().iter().map(|record| {
            let codes = record[..KEY_BYTES].chunks(CODE_BYTES).map(to_code).take_while(|code| *code != 0);
            codes.map(|code| char::from_u32(code).expect("a character's number")).collect()
        })
    }

    #[test]
    fn table_emoji_found() {
        let (small, large) = (spleen_12x24(), spleen_16x32());
        let count = TABLE.get(..COUNT_BYTES).map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        assert_eq!(count, Some(records().len() as u32), "as many records as the file counts");
        assert_eq!(TABLE.len(), COUNT_BYTES + records().len() * RECORD_BYTES, "and nothing behind them");
        let mut before: Option<Vec<char>> = None;
        for key in keys() {
            let emoji = find(&key).unwrap_or_else(|| panic!("{key:?} is found"));
            assert!(before.as_ref().is_none_or(|before| *before < key), "{key:?} stands after the one before it");
            for side in SIDES {
                let lit = (0..side * side).filter(|at| emoji.pixel(side, at % side, at / side).is_some()).count();
                assert!(lit > 0, "{key:?} lights its picture of {side}");
            }
            if emoji.is_shown_bare() {
                assert!(key.len() == 1 && !small.has(key[0]) && !large.has(key[0]), "{key:?} is none Spleen draws");
            }
            assert!(!key.iter().any(|character| TONES.contains(character)), "{key:?}: no tone");
            before = Some(key);
        }
        assert!(keys().count() > 1900, "the emoji of today's phones, their flags among them");
    }

    #[test]
    fn sequences_and_bare_emoji() {
        let shown = |key: &[char]| find(key).map(|emoji| emoji.is_shown_bare());
        assert_eq!(shown(&['\u{1F600}']), Some(true), "a face shows by itself");
        assert_eq!(shown(&['\u{2764}']), Some(false), "a heart only where it is asked for as a picture");
        assert_eq!(shown(&['1', '\u{20E3}']), Some(false), "a key cap is a sequence");
        for sequence in [
            &['\u{1F1E9}', '\u{1F1EA}'][..],
            &['\u{1F468}', '\u{200D}', '\u{1F469}', '\u{200D}', '\u{1F467}'],
            &['\u{1F3F3}', '\u{200D}', '\u{1F308}'],
            &['\u{1F3F4}', '\u{E0067}', '\u{E0062}', '\u{E0073}', '\u{E0063}', '\u{E0074}', '\u{E007F}'],
        ] {
            assert!(find(sequence).is_some(), "{sequence:?}");
        }
        assert_eq!(find(&['a']), None);
        assert_eq!(find(&['\u{1F600}'; KEY_CODES + 1]), None, "a sequence longer than any kept");
        let face = find(&['\u{1F600}']).expect("a face");
        assert_eq!([24, 32, 48, 64, 40, 8].map(Emoji::side), [24, 32, 24, 32, 32, 24]);
        assert_eq!(face.pixel(24, 0, 0), None, "its corner is the ground's");
        let yellow = face.pixel(24, 12, 4).expect("its brow");
        assert!(yellow.r() > 200 && yellow.g() > 150 && yellow.b() < 120, "a yellow face: {yellow:?}");
    }
}
