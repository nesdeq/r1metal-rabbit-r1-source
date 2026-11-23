//! The table of GNU Unifont's drawings, as `examples/glyphs.rs` writes it: a count for each width, then a record a drawing, each width's in the order of their characters.

use core::cmp::Ordering;
use core::ops::RangeInclusive;

static TABLE: &[u8] = include_bytes!("../../assets/unifont/glyphs.bin");
const COUNT_BYTES: usize = 4;
/// The bytes a record holds its character's number in, highest first.
pub const CODE_BYTES: usize = 3;
/// The rows of a drawing.
pub const ROWS: usize = 16;
// As tall as the tallest font.
pub(super) const GROWN_ROWS: usize = 32;
/// The widths of the drawings in pixels, a row of each a byte for every eight.
pub const WIDTHS: [u32; 2] = [8, 16];
/// The characters of private use, which the table holds no drawing of.
pub const PRIVATE: [RangeInclusive<char>; 2] = ['\u{E000}'..='\u{F8FF}', '\u{F0000}'..='\u{10FFFF}'];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Drawing {
    rows: [u16; ROWS],
    pub(super) width: u32,
}

impl Drawing {
    fn is_lit(&self, x: usize, y: usize) -> bool {
        x < self.width as usize && self.rows.get(y).is_some_and(|row| row & (0x8000 >> x) != 0)
    }

    // The drawing `side` rows tall, a row a word from its highest bit, its strokes as strong as Spleen's at that height: a pixel is lit where every row it covers lights a column it covers, so at one and a half times its size an upright stroke is two pixels and a level one is one, and at twice both are two. Where two pixels that touch by their corners alone come a row apart, the pixel between them is lit too, so no stroke breaks.
    pub(super) fn grown(&self, side: usize) -> [u32; GROWN_ROWS] {
        let side = side.clamp(ROWS, GROWN_ROWS);
        let covered = |at: usize| at * ROWS / side..=((at + 1) * ROWS - 1) / side;
        let (left_of, right_of) = (|x: usize| x * side / ROWS, |x: usize| ((x + 1) * side).div_ceil(ROWS) - 1);
        let (top_of, foot_of) = (|y: usize| (y * side).div_ceil(ROWS), |y: usize| (y + 1) * side / ROWS - 1);
        let mut rows = [0u32; GROWN_ROWS];
        let mut light = |x: usize, y: usize| rows[y] |= 0x8000_0000 >> x;
        for y in 0..side {
            for x in 0..self.width as usize * side / ROWS {
                if covered(y).all(|row| covered(x).any(|column| self.is_lit(column, row))) {
                    light(x, y);
                }
            }
        }
        for y in (0..ROWS - 1).filter(|y| top_of(y + 1) - foot_of(*y) == 2) {
            for x in (0..self.width as usize).filter(|x| self.is_lit(*x, y) && !self.is_lit(*x, y + 1)) {
                if self.is_lit(x + 1, y + 1) && !self.is_lit(x + 1, y) {
                    light(right_of(x), foot_of(y) + 1);
                }
                if x > 0 && self.is_lit(x - 1, y + 1) && !self.is_lit(x - 1, y) {
                    light(left_of(x), foot_of(y) + 1);
                }
            }
        }
        rows
    }
}

fn count(kind: usize) -> usize {
    let bytes = TABLE.get(kind * COUNT_BYTES..(kind + 1) * COUNT_BYTES);
    bytes.map_or(0, |bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

// The records of the drawings `width` wide: a character's number, then a row of the drawing a byte or two.
fn records(width: u32) -> core::slice::ChunksExact<'static, u8> {
    let length = |width: u32| CODE_BYTES + ROWS * (width / u8::BITS) as usize;
    let kind = WIDTHS.iter().position(|other| *other == width).unwrap_or(0);
    let start =
        WIDTHS.len() * COUNT_BYTES + (0..kind).map(|before| count(before) * length(WIDTHS[before])).sum::<usize>();
    let bytes = TABLE.get(start..start + count(kind) * length(width)).unwrap_or(&[]);
    bytes.chunks_exact(length(width))
}

fn to_drawing(record: &[u8], width: u32) -> Drawing {
    let row_bytes = (width / u8::BITS) as usize;
    let rows = core::array::from_fn(|row| {
        let bytes = &record[CODE_BYTES + row * row_bytes..][..row_bytes];
        let bits = bytes.iter().fold(0u16, |bits, byte| bits << u8::BITS | u16::from(*byte));
        bits << (u16::BITS - width)
    });
    Drawing { rows, width }
}

pub(super) fn find(character: char) -> Option<Drawing> {
    let code = &(character as u32).to_be_bytes()[1..];
    WIDTHS.into_iter().find_map(|width| {
        let records = records(width);
        let (mut low, mut high) = (0, records.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let record = records.clone().nth(middle)?;
            match record[..CODE_BYTES].cmp(code) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(to_drawing(record, width)),
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::font::{spleen_12x24, spleen_16x32};

    fn every(width: u32) -> impl Iterator<Item = (char, Drawing)> {
        records(width).map(move |record| {
            let number = u32::from_be_bytes([0, record[0], record[1], record[2]]);
            (char::from_u32(number).expect("a character's number"), to_drawing(record, width))
        })
    }

    #[test]
    fn table_drawings_found() {
        let (small, large) = (spleen_12x24(), spleen_16x32());
        for width in WIDTHS {
            let mut before = None;
            for (character, drawing) in every(width) {
                assert!(before < Some(character), "{character:?} stands after the one before it");
                assert_eq!(find(character), Some(drawing), "{character:?}");
                assert!(!small.has(character) && !large.has(character), "{character:?} is none Spleen draws");
                assert!(
                    !PRIVATE.iter().any(|private| private.contains(&character)),
                    "{character:?} is in no private use"
                );
                before = Some(character);
            }
        }
        assert_eq!(
            WIDTHS.map(|width| TABLE.len() >= WIDTHS.len() * COUNT_BYTES && every(width).count() > 10_000),
            [true, true],
            "the scripts of today's writing, narrow and wide"
        );
        let all: usize = WIDTHS.map(|width| records(width).len() * (CODE_BYTES + 2 * width as usize)).iter().sum();
        assert_eq!(TABLE.len(), WIDTHS.len() * COUNT_BYTES + all, "the file is its counts and its records");
    }

    #[test]
    fn widths_by_script() {
        for (character, width) in
            [('\u{4E2D}', 16), ('\u{3042}', 16), ('\u{D55C}', 16), ('\u{5D0}', 8), ('\u{2713}', 8)]
        {
            let drawing = find(character).unwrap_or_else(|| panic!("{character:?} is drawn"));
            assert_eq!(drawing.width, width, "{character:?}");
            assert!(drawing.rows.iter().any(|row| *row != 0), "{character:?} lights a pixel");
            assert!(drawing.rows.iter().all(|row| row.trailing_zeros() >= u16::BITS - width), "within its width");
        }
        for none in ['a', '\u{E4}', '\u{E000}', '\u{F0000}', '\u{10FFFF}'] {
            assert_eq!(find(none), None, "{none:?} is Spleen's or nobody's");
        }
    }

    // How many parts a picture falls into, pixels that touch by a side or a corner being of one part.
    fn parts(is_lit: impl Fn(usize, usize) -> bool, width: usize, height: usize) -> usize {
        let mut seen = alloc::vec![false; width * height];
        let mut count = 0;
        for start in (0..width * height).filter(|at| is_lit(at % width, at / width)) {
            if core::mem::replace(&mut seen[start], true) {
                continue;
            }
            count += 1;
            let mut open = alloc::vec![start];
            while let Some(at) = open.pop() {
                let (x, y) = (at % width, at / width);
                let around = |at: usize, end: usize| at.saturating_sub(1)..(at + 2).min(end);
                for (beside_x, beside_y) in around(y, height).flat_map(|y| around(x, width).map(move |x| (x, y))) {
                    let beside = beside_y * width + beside_x;
                    if is_lit(beside_x, beside_y) && !core::mem::replace(&mut seen[beside], true) {
                        open.push(beside);
                    }
                }
            }
        }
        count
    }

    #[test]
    fn grown_keeps_strokes() {
        let line = find('\u{4E00}').expect("the sign for one, a single stroke");
        let strong = |side: usize| line.grown(side).iter().filter(|row| **row != 0).count();
        assert_eq!(
            (line.rows.iter().filter(|row| **row != 0).count(), strong(24), strong(32)),
            (1, 1, 2),
            "a level stroke is as thin at one and a half times its size, and doubled at twice"
        );
        let cross = find('\u{5341}').expect("the sign for ten, a stroke upright through one level");
        let stem = |rows: &[u32]| rows.iter().filter(|row| **row != 0).map(|row| row.count_ones()).min();
        let source: [u32; ROWS] = cross.rows.map(u32::from);
        assert_eq!(
            (stem(&source), stem(&cross.grown(24)), stem(&cross.grown(32))),
            (Some(1), Some(2), Some(2)),
            "an upright stroke is two pixels at either size, as Spleen's are"
        );
        let doubled = find('\u{4E2D}').expect("a sign of many strokes");
        for (x, y) in (0..32).flat_map(|y| (0..32).map(move |x| (x, y))) {
            let is_lit = doubled.grown(32)[y] & (0x8000_0000 >> x) != 0;
            assert_eq!(is_lit, doubled.is_lit(x / 2, y / 2), "({x}, {y}): at twice the size each pixel is four");
        }
        for (character, drawing) in every(16).filter(|(character, _)| ('\u{3040}'..'\u{5200}').contains(character)) {
            for side in [24, 32] {
                let (grown, width) = (drawing.grown(side), 16 * side / ROWS);
                assert_eq!(
                    parts(|x, y| grown[y] & (0x8000_0000 >> x) != 0, width, side),
                    parts(|x, y| drawing.is_lit(x, y), 16, ROWS),
                    "{character:?} at {side}: no stroke breaks and none joins another"
                );
            }
        }
        let check = find('\u{2713}').expect("a check mark, all slants").grown(24);
        assert_eq!(parts(|x, y| check[y] & (0x8000_0000 >> x) != 0, 12, 24), 1, "its slants hold together");
    }
}
