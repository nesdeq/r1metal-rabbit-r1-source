//! A 4:2:0 picture's planes: cut into parts of whole line pairs, shrunk to another size, and made of pixel words.

use super::pixel::PixelFormat;
use super::ycbcr::{Chroma, Colours, WEIGHTS_601, jfif_chroma, jfif_luma};
use alloc::vec::Vec;
use core::ops::Range;

/// Lines of each of `parts` parts of a 4:2:0 picture: whole line pairs, the last part maybe fewer.
pub const fn part_lines(lines: usize, parts: usize) -> usize {
    lines.div_ceil(parts).next_multiple_of(2)
}

/// The lines and the luma and chroma slices of each part of `height` lines of a 4:2:0 picture.
pub fn parts<'a>(
    luma: &'a mut [u8],
    chroma: &'a mut [u8],
    stride: usize,
    height: usize,
) -> impl Iterator<Item = (Range<usize>, &'a mut [u8], &'a mut [u8])> {
    let pieces = luma.chunks_mut(height * stride).zip(chroma.chunks_mut(height / 2 * stride));
    pieces
        .enumerate()
        .map(move |(index, (luma, chroma))| (index * height..index * height + luma.len() / stride, luma, chroma))
}

/// The lines and the Y, U and V slices of each part of `height` lines of a 4:2:0 picture of `width` by `lines` whose colours stand apart, each plane brought to the picture's size first.
pub fn parts_apart(
    planes: &mut [Vec<u8>; 3],
    (width, lines): (usize, usize),
    height: usize,
) -> impl Iterator<Item = (Range<usize>, &mut [u8], [&mut [u8]; 2])> {
    let [luma, blue, red] = planes;
    luma.resize(width * lines, 0);
    for colour in [&mut *blue, &mut *red] {
        colour.resize(width / 2 * (lines / 2), 0);
    }
    let colours = height / 2 * (width / 2);
    let pieces = luma.chunks_mut(height * width).zip(blue.chunks_mut(colours).zip(red.chunks_mut(colours)));
    pieces.enumerate().map(move |(index, (luma, (blue, red)))| {
        (index * height..index * height + luma.len() / width, luma, [blue, red])
    })
}

// The most places a side of a plane `Cover` takes may have, so that a place's share of a small one, never more than either side's places, is 16 bits.
const MOST_SIDE: usize = u16::MAX as usize;
// The most places times lines a plane `Cover` takes may have: every sum of a small place, 255 for each unit of its weight and half its weight, stays under 2^32, and so does the reciprocal it is divided by (`reciprocal`), under 511 times this.
const MOST_TOTAL: usize = 1 << 23;

// For each of `small` places, the first of `places` it covers and how much it covers of each of the `reach` places from that first one, in units of which a place of `places` is `small` long: as many shares for each, the most any covers, those past what it covers nought, and the first moved back where its reach would pass the last place.
struct Spans {
    places: usize,
    reach: usize,
    firsts: Vec<usize>,
    shares: Vec<u16>,
}

impl Spans {
    fn new(places: usize, small: usize) -> Spans {
        let covered = |at: usize| (at * places / small, ((at + 1) * places).div_ceil(small));
        let reach = (0..small).map(|at| covered(at).1 - covered(at).0).max().unwrap_or(0).max(1);
        let (mut firsts, mut shares) = (Vec::with_capacity(small), Vec::with_capacity(small * reach));
        for at in 0..small {
            let (start, end) = (at * places, (at + 1) * places);
            let first = covered(at).0.min(places.saturating_sub(reach));
            firsts.push(first);
            let share = |place: usize| end.min((place + 1) * small).saturating_sub(start.max(place * small)) as u16;
            shares.extend((first..first + reach).map(share));
        }
        Spans { places, reach, firsts, shares }
    }

    // The first place a small one covers, and its shares.
    fn of(&self, at: usize) -> (usize, &[u16]) {
        (self.firsts[at], &self.shares[at * self.reach..][..self.reach])
    }
}

// The reciprocal of a small place's weight `total`, rounded up, and the shift it is taken by: its product with any rounded sum of a mean, 255 times `total` and half of it at the most, shifted, is that sum divided by `total`, as the shift is the least past that most times one less than `total`, where the product's error stays under one (Granlund and Montgomery, "Division by invariant integers using multiplication", 1994).
fn reciprocal(total: u32) -> (u32, u32) {
    let most = u64::from(total) * u64::from(u8::MAX) + u64::from(total / 2);
    let shift = u64::BITS - (most * u64::from(total - 1)).leading_zeros();
    let inverse = (1u64 << shift).div_ceil(u64::from(total));
    (u32::try_from(inverse).unwrap_or(u32::MAX), shift)
}

// What each small place of a plane covers across and down, half the weight of a whole small place, and the reciprocal of that weight with its shift, by which each sum is divided.
pub(crate) struct Cover {
    across: Spans,
    down: Spans,
    half: u32,
    inverse: u32,
    shift: u32,
}

impl Cover {
    pub(crate) fn new((places, lines): (usize, usize), (small, small_lines): (usize, usize)) -> Cover {
        let total = places * lines;
        assert!(
            places.max(lines) <= MOST_SIDE && total <= MOST_TOTAL,
            "a plane of {places} by {lines} is larger than a cover takes"
        );
        let (inverse, shift) = reciprocal(total.max(1) as u32);
        Cover {
            across: Spans::new(places, small),
            down: Spans::new(lines, small_lines),
            half: total as u32 / 2,
            inverse,
            shift,
        }
    }

    // Shrinks sample `sample` of the `SAMPLES` each place of a plane holds side by side, a line `stride` bytes from the next: its small lines `lines` into that sample of each place of `out`, which holds as many. Each small line is summed down first, the whole width of the lines it covers into `columns`, then across.
    pub(crate) fn shrink<const SAMPLES: usize>(
        &self,
        plane: &[u8],
        (stride, sample): (usize, usize),
        lines: Range<usize>,
        (out, columns): (&mut [u8], &mut Vec<u32>),
    ) {
        let (small, reach) = (self.across.firsts.len(), self.across.reach);
        columns.resize(self.across.places, 0);
        for (line, out) in lines.zip(out.chunks_mut(small * SAMPLES)) {
            columns.fill(0);
            let (first_line, line_shares) = self.down.of(line);
            for (offset, &line_share) in line_shares.iter().enumerate().filter(|(_, share)| **share > 0) {
                let row = plane[(first_line + offset) * stride..].as_chunks::<SAMPLES>().0;
                for (column, place) in columns.iter_mut().zip(row) {
                    *column += u32::from(line_share) * u32::from(place[sample]);
                }
            }
            let places = self.across.firsts.iter().zip(self.across.shares.chunks_exact(reach));
            for (place, (&first, shares)) in out.as_chunks_mut::<SAMPLES>().0.iter_mut().zip(places) {
                let covered = columns[first..first + reach].iter().zip(shares);
                let sum: u32 = covered.map(|(&column, &share)| column * u32::from(share)).sum();
                place[sample] = ((u64::from(sum + self.half) * u64::from(self.inverse)) >> self.shift) as u8;
            }
        }
    }
}

/// A 4:2:0 picture of one size shrunk to another by the mean over each small pixel's area; what each small place covers is reckoned once, for every picture of those sizes.
pub struct Shrink {
    width: usize,
    luma: Cover,
    // A place holds the two colour samples of two by two pixels, side by side.
    chroma: Cover,
}

impl Shrink {
    /// A shrinking of pictures of the width and height `size` to those of `small`, each of them even.
    pub fn new(size: (usize, usize), small: (usize, usize)) -> Shrink {
        let halved = |(width, height): (usize, usize)| (width / 2, height / 2);
        Shrink { width: size.0, luma: Cover::new(size, small), chroma: Cover::new(halved(size), halved(small)) }
    }

    /// Shrinks a picture's small lines `lines` (even ends) into the parts given, the colours side by side as they came.
    pub fn shrink(
        &self,
        (luma, chroma): (&[u8], &[u8]),
        lines: Range<usize>,
        (small_luma, small_chroma): (&mut [u8], &mut [u8]),
    ) {
        let (halves, mut columns) = (lines.start / 2..lines.end / 2, Vec::new());
        self.luma.shrink::<1>(luma, (self.width, 0), lines, (small_luma, &mut columns));
        for colour in 0..2 {
            self.chroma.shrink::<2>(chroma, (self.width, colour), halves.clone(), (small_chroma, &mut columns));
        }
    }
}

/// The words in `format` of a square picture of `side` pixels from its JFIF planes, as `to_planes` made them.
pub fn to_words(luma: &[u8], chroma: &[u8], side: usize, format: PixelFormat, words: &mut [u32]) {
    let colours = const { Colours::new(WEIGHTS_601, true) };
    for (line, row) in words.chunks_exact_mut(side).enumerate() {
        let pairs = Chroma::Shared(&chroma[line / 2 * side..][..side]);
        colours.row(&luma[line * side..][..side], pairs, format, row);
    }
}

/// JPEG encoder planes of a square picture of 0x00RRGGBB words.
pub fn to_planes(words: &[u32], side: usize, luma: &mut [u8], chroma: &mut [u8]) {
    const WEIGHT_BITS: u32 = 16;
    let channels = |word: u32| [(word >> 16 & 0xFF) as i32, (word >> 8 & 0xFF) as i32, (word & 0xFF) as i32];
    for (sample, word) in luma.iter_mut().zip(words) {
        *sample = jfif_luma::<WEIGHT_BITS>(channels(*word));
    }
    for (line, pairs) in chroma.chunks_exact_mut(side).enumerate() {
        for (index, pair) in pairs.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let first = 2 * line * side + 2 * index;
            *pair = jfif_chroma::<WEIGHT_BITS>([0, 1, side, side + 1].map(|at| channels(words[first + at])));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn parts_are_whole_line_pairs() {
        assert_eq!(
            (part_lines(2448, 8), part_lines(1024, 8)),
            (306, 128),
            "a photo in eight parts, one for each of the chip's cores, and the pixel cam's picture shrunk from it in as many"
        );
        assert_eq!(part_lines(2048, 6), 342, "parts that cannot be even end in whole line pairs");
        assert_eq!(
            (part_lines(720, 8), part_lines(100, 3)),
            (90, 34),
            "a recording's frame in eight parts, and lines that do not share evenly"
        );
        assert_eq!(
            (part_lines(500, 8), 500usize.div_ceil(part_lines(500, 8)), part_lines(5, 8)),
            (64, 8, 2),
            "never more parts than asked for, nor a part of no lines"
        );
        let (mut luma, mut chroma) = (vec![0u8; 4 * 10], vec![0u8; 4 * 5]);
        let cut: Vec<(Range<usize>, usize, usize)> = parts(&mut luma, &mut chroma, 4, part_lines(10, 3))
            .map(|(lines, luma, chroma)| (lines, luma.len(), chroma.len()))
            .collect();
        assert_eq!(cut, [(0..4, 16, 8), (4..8, 16, 8), (8..10, 8, 4)], "the last part holds what is left");
        let mut apart = [Vec::new(), vec![9u8; 99], Vec::new()];
        let cut: Vec<(Range<usize>, usize, [usize; 2])> = parts_apart(&mut apart, (4, 10), part_lines(10, 3))
            .map(|(lines, luma, colours)| (lines, luma.len(), colours.map(|colour| colour.len())))
            .collect();
        assert_eq!(
            cut,
            [(0..4, 16, [4, 4]), (4..8, 16, [4, 4]), (8..10, 8, [2, 2])],
            "the same lines with the colours apart, each a quarter of the luma"
        );
        assert_eq!(apart.each_ref().map(Vec::len), [40, 10, 10], "each plane at the picture's size");
    }

    #[test]
    fn shrink_means_areas() {
        let luma = [10, 20, 0, 0, 30, 40, 0, 4, 255, 255, 1, 2, 255, 255, 3, 4];
        let chroma = [100, 200, 110, 210, 120, 220, 130, 230];
        let (mut small_luma, mut small_chroma) = ([0u8; 4], [0u8; 2]);
        Shrink::new((4, 4), (2, 2)).shrink((&luma, &chroma), 0..2, (&mut small_luma, &mut small_chroma));
        assert_eq!(small_luma, [25, 1, 255, 3], "halved, four samples of light make one, rounded to the nearer");
        assert_eq!(
            small_chroma,
            [115, 215],
            "and the two colours of four places make the two of one, each from its own"
        );
        let ramp: Vec<u8> = (0..36).map(|at| (30 * (at % 6)) as u8).collect();
        let (mut ramp_luma, mut ramp_chroma) = ([0u8; 16], [0u8; 8]);
        let ramps = Shrink::new((6, 6), (4, 4));
        ramps.shrink((&ramp, &[128; 18]), 0..4, (&mut ramp_luma, &mut ramp_chroma));
        assert_eq!(ramp_luma[..4], [10, 50, 100, 140], "a pixel and a half to each, the half shared");
        let (side, small) = (2 * 153, 2 * 64);
        let (mut luma, mut chroma) = (vec![0u8; small * small], vec![0u8; small * small / 2]);
        let (flat_luma, flat_chroma) = (vec![7; side * side], vec![9; side * side / 2]);
        Shrink::new((side, side), (small, small)).shrink(
            (&flat_luma, &flat_chroma),
            0..small,
            (&mut luma, &mut chroma),
        );
        assert!(
            luma.iter().all(|sample| *sample == 7) && chroma.iter().all(|sample| *sample == 9),
            "at a photo's ratio to the pixel cam's picture a flat picture stays what it is at every place"
        );
        let (mut upper, mut upper_colours) = ([0u8; 8], [0u8; 4]);
        ramps.shrink((&ramp, &[128; 18]), 0..2, (&mut upper, &mut upper_colours));
        assert_eq!(upper, ramp_luma[..8], "a picture can be shrunk in parts of whole line pairs: here its upper half");
    }

    #[test]
    fn shrink_wide() {
        // Eight by four pixels to four by two: each colour place of the small picture is the mean of two by two.
        let luma: Vec<u8> = (0..32).map(|at| (8 * at) as u8).collect();
        let chroma = [10, 200, 20, 210, 30, 220, 40, 230, 50, 240, 60, 250, 70, 100, 80, 110];
        let (mut pairs_luma, mut pairs) = ([0u8; 8], [0u8; 4]);
        Shrink::new((8, 4), (4, 2)).shrink((&luma, &chroma), 0..2, (&mut pairs_luma, &mut pairs));
        assert_eq!(pairs_luma, [36, 52, 68, 84, 164, 180, 196, 212], "a width and a height of their own");
        assert_eq!(pairs, [35, 225, 55, 165], "each colour from its own samples, side by side");
    }

    // A weight's reciprocal checked over the whole range of its rounded sums: as the product grows with the sum, it divides every sum exactly once it divides the first and the last sum of each quotient exactly.
    fn divides_exactly(total: u32) -> bool {
        let (inverse, shift) = reciprocal(total);
        let (whole, most) = (u64::from(total), u64::from(total) * u64::from(u8::MAX) + u64::from(total / 2));
        let divided = |sum: u64| (sum * u64::from(inverse)) >> shift;
        (0..=most / whole).all(|quotient| {
            let (first, last) = (quotient * whole, (quotient * whole + whole - 1).min(most));
            divided(first) == quotient && divided(last) == quotient
        })
    }

    #[test]
    fn reciprocals_divide_exactly() {
        // The proof for every weight a cover takes: the inverse is the ceiling of two to the shift over the weight and fits 32 bits, and the most rounded sum times one less than the weight stays under two to the shift, so that the product's error over a sum never reaches the next quotient.
        for total in 1..=MOST_TOTAL as u32 {
            let (inverse, shift) = reciprocal(total);
            let (whole, most) = (u64::from(total), u64::from(total) * u64::from(u8::MAX) + u64::from(total / 2));
            let ceiling = (1u64 << shift).div_ceil(whole);
            assert!(
                ceiling <= u64::from(u32::MAX) && u64::from(inverse) == ceiling && most * (whole - 1) < 1 << shift,
                "a weight of {total}"
            );
        }
        assert!((1..=1 << 16).all(divides_exactly), "every weight up to 65536");
        let mut state = 0x5EED_u32;
        let mut random = || {
            state = crate::base::math::xorshift32(state);
            state % MOST_TOTAL as u32 + 1
        };
        let picked = [640 * 480, 320 * 240, 2448 * 2448, MOST_TOTAL as u32 - 1, MOST_TOTAL as u32];
        assert!(
            picked.into_iter().chain((0..20_000).map(|_| random())).all(divides_exactly),
            "a decoded frame's planes, a photo's, the largest a cover takes, and weights at random up to it"
        );
    }

    #[test]
    fn to_planes_makes_420() {
        let (grey, red, blue, white) = (0x80_8080, 0xFF_0000, 0x00_00FF, 0xFF_FFFF);
        // Four by four pixels: a grey quarter, a red one, a blue one, and one of two whites over two blacks.
        #[rustfmt::skip]
        let words = [
            grey, grey, red, red,
            grey, grey, red, red,
            blue, blue, white, white,
            blue, blue, 0, 0,
        ];
        let (mut luma, mut chroma) = ([0u8; 16], [0u8; 8]);
        to_planes(&words, 4, &mut luma, &mut chroma);
        #[rustfmt::skip]
        assert_eq!(luma, [
            128, 128, 76, 76,
            128, 128, 76, 76,
            29, 29, 255, 255,
            29, 29, 0, 0,
        ], "the light of each pixel, by JFIF's weights");
        assert_eq!(
            chroma,
            [128, 128, 85, 255, 255, 107, 128, 128],
            "a pair for every two by two: grey and the mean of white and black have no colour, red and blue theirs, kept within a byte"
        );
        let mut colours = [0u8; 4];
        to_planes(&[0x00_FF00, 0x00_FF00, 0x00_FF00, 0x00_FF00, 0, 0, 0, 0], 4, &mut [0u8; 8], &mut colours);
        assert_eq!(colours, [86, 75, 86, 75], "half green and half black, the colour of the mean");
        let mut back = [0u32; 16];
        to_words(&luma, &chroma, 4, crate::base::pixel::PixelFormat::XRGB8888, &mut back);
        let near =
            |a: u32, b: u32| (0..3).all(|byte| ((a >> (8 * byte)) & 0xFF).abs_diff((b >> (8 * byte)) & 0xFF) <= 2);
        assert!(
            [0, 2, 8].iter().all(|&at| near(back[at], words[at])),
            "a quarter of one colour comes back as it was: {back:x?}"
        );
    }
}
