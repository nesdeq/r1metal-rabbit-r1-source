//! Pictures for the video encoders from raw frames, in the sensor's orientation: luma for every pixel from the four samples at and after it, chroma for every quad.

use crate::base::simd::{self, PACKED_CODES};
use crate::base::ycbcr::CHROMA_MIDDLE;
use crate::base::ycbcr::CHROMA_SPAN;
use crate::base::ycbcr::FULL;
use crate::base::ycbcr::LUMA_FLOOR;
use crate::base::ycbcr::LUMA_SPAN;
use crate::base::ycbcr::WEIGHTS_601;
use crate::base::ycbcr::WEIGHTS_709;
use crate::base::ycbcr::WHOLE;
use crate::base::ycbcr::div_round;
use crate::hw::camera::BLUE;
use crate::hw::camera::GREEN;
use crate::hw::camera::RED;
use crate::hw::camera::UNITY;
use crate::hw::camera::frame::Frame;
use crate::hw::camera::frame::RAW_CODES;
use crate::hw::camera::shading::Axis;
use crate::hw::camera::shading::Lens;
use crate::hw::camera::shading::Profile;
use crate::hw::camera::shading::Shading;
use crate::hw::camera::shading::Span;
use crate::hw::camera::tone::Tone;
use alloc::vec::Vec;
use core::ops::Range;

const SHARE_SHIFT: u32 = 6;
const HALF: i32 = 1 << (SHARE_SHIFT - 1);
const GREENS: i64 = 2;
// What a matrix takes past the range of video is cut off at its ends.
const LUMA_TOP: i32 = LUMA_FLOOR + LUMA_SPAN as i32;
const CHROMA_FLOOR: i32 = CHROMA_MIDDLE - (CHROMA_SPAN / 2) as i32;
const CHROMA_TOP: i32 = CHROMA_MIDDLE + (CHROMA_SPAN / 2) as i32;
/// The error text where `Planes::new` makes none.
pub const NO_PLANES: &str = "no planes for frames of this size";

struct Shares {
    luma: [[i16; RAW_CODES]; 3],
    blue: [[i16; RAW_CODES]; 3],
    red: [[i16; RAW_CODES]; 3],
}

fn saturate(share: i64) -> i16 {
    share.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// A thread's buffers from one part of a picture to the next; any thread's serves any planes.
#[derive(Default)]
pub struct Scratch {
    upper: Vec<i16>,
    lower: Vec<i16>,
    blue: Vec<i32>,
    red: Vec<i32>,
    codes: Vec<u16>,
    even: Vec<u16>,
    odd: Vec<u16>,
    profile: Option<(&'static Shading, u32, Profile)>,
}

/// Where a picture's chroma goes: the H.264 encoder's pairs of blue and red, or the two apart, as VP8 takes them.
pub enum Chroma<'a> {
    Pairs(&'a mut [u8]),
    Apart([&'a mut [u8]; 2]),
}

/// Turns raw frames of one size into the planes a video encoder reads: luma, and chroma as `Chroma` lays it out.
pub struct Planes {
    frame: Frame,
    stride: usize,
    // The share of red, green and blue in the luma, in 10000ths.
    weights: [i64; 3],
    shares: Shares,
    correction: Option<(Lens, Span)>,
}

impl Planes {
    /// Planes of `stride` bytes a line by the recipe of `tone`, in Rec. 709's colours; none unless the width is a multiple of four, the lines even and the stride fits.
    pub fn new(frame: Frame, stride: usize, tone: &Tone) -> Option<Planes> {
        let (width, lines) = (frame.width() as usize, frame.lines as usize);
        if width == 0 || !width.is_multiple_of(PACKED_CODES) || lines < 2 || !lines.is_multiple_of(2) || stride < width
        {
            return None;
        }
        let mut planes = Planes {
            frame,
            stride,
            weights: WEIGHTS_709,
            shares: Shares { luma: [[0; RAW_CODES]; 3], blue: [[0; RAW_CODES]; 3], red: [[0; RAW_CODES]; 3] },
            correction: None,
        };
        planes.set_tone(tone);
        Some(planes)
    }

    /// Planes whose lines are as long as the frame is wide, with no bytes between them.
    pub fn tight(frame: Frame, tone: &Tone) -> Option<Planes> {
        Planes::new(frame, frame.width() as usize, tone)
    }

    /// The bytes of a line of the planes, and the lines of the luma.
    pub const fn size(&self) -> (usize, usize) {
        (self.stride, self.frame.lines as usize)
    }

    /// The same planes in Rec. 601's colours, by which a stream that names no matrix is read: VP8's.
    pub fn with_rec_601(mut self, tone: &Tone) -> Planes {
        self.weights = WEIGHTS_601;
        self.set_tone(tone);
        self
    }

    /// The same planes, making up for the lens's shading in every sample before it is turned into shares.
    pub fn with_lens(mut self, lens: Lens) -> Planes {
        let (width, area) = (self.frame.width(), lens.area);
        let span = Span::evenly(Axis::Horizontal, area.left, area.scale * width, width as usize / 2, false);
        self.correction = Some((lens, span));
        self
    }

    /// Takes the recipe of `tone` from now on.
    pub fn set_tone(&mut self, tone: &Tone) {
        let (codes, mix, weights) = (&tone.codes, tone.recipe.matrix, self.weights);
        let unit = i64::from(UNITY);
        for colour in [RED, GREEN, BLUE] {
            let samples = if colour == GREEN { GREENS } else { 1 };
            // What a code of this colour of the sensor adds to the picture's luma and to its blue and red, in 10000ths of 1024ths.
            let weight: i64 = (0..3).map(|own| weights[own] * i64::from(mix[own][colour])).sum();
            let chroma_weight = |own: usize| WHOLE * i64::from(mix[own][colour]) - weight;
            let scale = (1 << SHARE_SHIFT) as i64;
            let code_shares: [(i16, i16, i16); FULL as usize + 1] = core::array::from_fn(|code| {
                let code = code as i64;
                let luma = div_round(code * weight * LUMA_SPAN * scale, unit * WHOLE * FULL * samples);
                let blue = div_round(
                    code * chroma_weight(BLUE) * CHROMA_SPAN * scale,
                    unit * 2 * (WHOLE - weights[BLUE]) * FULL * samples,
                );
                let red = div_round(
                    code * chroma_weight(RED) * CHROMA_SPAN * scale,
                    unit * 2 * (WHOLE - weights[RED]) * FULL * samples,
                );
                (saturate(luma), saturate(blue), saturate(red))
            });
            for (raw, &code) in codes[colour].iter().enumerate() {
                (self.shares.luma[colour][raw], self.shares.blue[colour][raw], self.shares.red[colour][raw]) =
                    code_shares[usize::from(code)];
            }
        }
    }

    #[cfg(test)]
    const fn bytes(&self) -> (usize, usize) {
        let luma = self.stride * self.frame.lines as usize;
        (luma, luma / 2)
    }

    fn profile<'a>(&self, y: usize, cached: &'a mut Option<(&'static Shading, u32, Profile)>) -> Option<&'a Profile> {
        let (lens, _) = self.correction.as_ref()?;
        let row = lens.area.top + lens.area.scale * (y as u32 | 1);
        if cached.as_ref().is_none_or(|(shading, at, _)| !core::ptr::eq(*shading, lens.shading) || *at != row) {
            *cached = Some((lens.shading, row, lens.shading.profile(Axis::Horizontal, row)));
        }
        cached.as_ref().map(|(_, _, profile)| profile)
    }

    fn line(
        &self,
        raw: &[u8],
        y: usize,
        pairs: &mut [i16],
        [blue, red]: [&mut [i32]; 2],
        [codes, even, odd]: [&mut [u16]; 3],
        profile: Option<&Profile>,
    ) {
        let width = self.frame.width() as usize;
        let stride = self.frame.line_bytes() as usize;
        let is_upper = y.is_multiple_of(2);
        let (first, second) = if is_upper { (GREEN, BLUE) } else { (RED, GREEN) };
        // The line's samples are read into two runs, one of each of its colours, so that each can be lifted in one go.
        simd::unpack(&raw[y * stride..(y + 1) * stride], codes);
        for ((even, odd), pair) in even.iter_mut().zip(odd.iter_mut()).zip(codes.as_chunks::<2>().0) {
            (*even, *odd) = (pair[0], pair[1]);
        }
        if let (Some((_, span)), Some(profile)) = (&self.correction, profile) {
            span.lift(&profile[first], 0, even);
            span.lift(&profile[second], 0, odd);
        }
        let shares = &self.shares;
        let (luma, to_blue, to_red) = (
            [&shares.luma[first], &shares.luma[second]],
            [&shares.blue[first], &shares.blue[second]],
            [&shares.red[first], &shares.red[second]],
        );
        let mut last_share = 0i16;
        let quads = blue
            .as_chunks_mut::<{ PACKED_CODES / 2 }>()
            .0
            .iter_mut()
            .zip(red.as_chunks_mut::<{ PACKED_CODES / 2 }>().0);
        let samples = even.as_chunks::<{ PACKED_CODES / 2 }>().0.iter().zip(odd.as_chunks::<{ PACKED_CODES / 2 }>().0);
        for ((pairs, (blue, red)), (even, odd)) in
            pairs.as_chunks_mut::<PACKED_CODES>().0.iter_mut().zip(quads).zip(samples)
        {
            // A code is cut to the tables' length by its bits, so that no step asks whether a table reaches that far.
            let pixels = [even[0], odd[0], even[1], odd[1]].map(|code| usize::from(code) & (RAW_CODES - 1));
            let lumas = [luma[0][pixels[0]], luma[1][pixels[1]], luma[0][pixels[2]], luma[1][pixels[3]]];
            pairs[0] = last_share.saturating_add(lumas[0]);
            pairs[1] = lumas[0].saturating_add(lumas[1]);
            pairs[2] = lumas[1].saturating_add(lumas[2]);
            pairs[3] = lumas[2].saturating_add(lumas[3]);
            last_share = lumas[3];
            for half in 0..PACKED_CODES / 2 {
                let (first_code, second_code) = (pixels[2 * half], pixels[2 * half + 1]);
                let (blue_share, red_share) = (
                    i32::from(to_blue[0][first_code]) + i32::from(to_blue[1][second_code]),
                    i32::from(to_red[0][first_code]) + i32::from(to_red[1][second_code]),
                );
                if is_upper {
                    (blue[half], red[half]) = (blue_share, red_share);
                } else {
                    (blue[half], red[half]) = (blue[half] + blue_share, red[half] + red_share);
                }
            }
        }
        // The pixel past the end of a line is taken to be the one two before it, which has its colour.
        pairs[width] = pairs[width - 1];
    }

    /// Makes the picture's lines `lines` (even ends) of a frame into `luma` and `chroma`, each holding just those lines.
    pub fn convert(
        &self,
        raw: &[u8],
        lines: Range<usize>,
        luma: &mut [u8],
        mut chroma: Chroma<'_>,
        scratch: &mut Scratch,
    ) {
        let (width, height, stride) = (self.frame.width() as usize, self.frame.lines as usize, self.stride);
        assert!(
            lines.start <= lines.end
                && lines.end <= height
                && lines.start.is_multiple_of(2)
                && lines.end.is_multiple_of(2),
            "lines {lines:?} of a frame of {height}"
        );
        let colours = lines.len() / 2 * stride;
        let is_chroma_long = match &chroma {
            Chroma::Pairs(pairs) => pairs.len() >= colours,
            Chroma::Apart([blues, reds]) => blues.len() >= colours / 2 && reds.len() >= colours / 2,
        };
        assert!(
            raw.len() >= self.frame.bytes() && luma.len() >= lines.len() * stride && is_chroma_long,
            "frame or planes too short"
        );
        if lines.is_empty() {
            return;
        }
        let Scratch { upper, lower, blue, red, codes, even, odd, profile } = scratch;
        for pairs in [&mut *upper, &mut *lower] {
            pairs.resize(width + 1, 0);
        }
        for shares in [&mut *blue, &mut *red] {
            shares.resize(width / 2, 0);
        }
        codes.resize(width, 0);
        for samples in [&mut *even, &mut *odd] {
            samples.resize(width / 2, 0);
        }
        let first = self.profile(lines.start, profile);
        self.line(raw, lines.start, upper, [&mut *blue, &mut *red], [&mut *codes, &mut *even, &mut *odd], first);
        for y in lines.clone() {
            // The line under the last one is taken to be the one above it, which has its colours.
            let below = if y + 1 < height { y + 1 } else { y - 1 };
            let below_profile = self.profile(below, profile);
            let samples = [&mut codes[..], &mut even[..], &mut odd[..]];
            self.line(raw, below, lower, [&mut *blue, &mut *red], samples, below_profile);
            let row = (y - lines.start) * stride;
            for ((out, &top), &bottom) in luma[row..row + width].iter_mut().zip(&upper[1..]).zip(&lower[1..]) {
                *out = to_luma(i32::from(top) + i32::from(bottom));
            }
            if y.is_multiple_of(2) {
                let row = (y - lines.start) / 2 * stride;
                let samples = blue.iter().zip(&*red).map(|(&to_blue, &to_red)| [to_chroma(to_blue), to_chroma(to_red)]);
                match &mut chroma {
                    Chroma::Pairs(pairs) => {
                        for (out, sample) in pairs[row..row + width].as_chunks_mut::<2>().0.iter_mut().zip(samples) {
                            *out = sample;
                        }
                    }
                    Chroma::Apart([blues, reds]) => {
                        let (row, end) = (row / 2, row / 2 + width / 2);
                        let outs = blues[row..end].iter_mut().zip(&mut reds[row..end]);
                        for ((to_blue, to_red), sample) in outs.zip(samples) {
                            (*to_blue, *to_red) = (sample[0], sample[1]);
                        }
                    }
                }
            }
            core::mem::swap(upper, lower);
        }
    }
}

// A pixel's luma out of the shares of its samples.
fn to_luma(share: i32) -> u8 {
    (LUMA_FLOOR + ((share + HALF) >> SHARE_SHIFT)).clamp(LUMA_FLOOR, LUMA_TOP) as u8
}

// A colour sample out of the shares of its samples.
fn to_chroma(share: i32) -> u8 {
    (CHROMA_MIDDLE + ((share + HALF) >> SHARE_SHIFT)).clamp(CHROMA_FLOOR, CHROMA_TOP) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::colour::DAYLIGHT;
    use crate::hw::camera::colour::gain;
    use crate::hw::camera::colour::matrix;
    use crate::hw::camera::frame::BLACK;
    use crate::hw::camera::frame::WHITE;
    use crate::hw::camera::frame::fake::flat;
    use crate::hw::camera::frame::fake::packed;
    use crate::hw::camera::mode::Mode;
    use crate::hw::camera::shading::SensorArea;
    use crate::hw::camera::shading::fake;
    use crate::hw::camera::tone::Recipe;
    use crate::hw::camera::tone::fake::balanced;
    use crate::hw::camera::tone::fake::plain;
    use alloc::vec;
    use alloc::vec::Vec;

    fn convert_whole(planes: &Planes, raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (luma_bytes, chroma_bytes) = planes.bytes();
        let (mut luma, mut chroma) = (vec![0u8; luma_bytes], vec![0u8; chroma_bytes]);
        let lines = 0..planes.frame.lines as usize;
        planes.convert(raw, lines, &mut luma, Chroma::Pairs(&mut chroma), &mut Scratch::default());
        (luma, chroma)
    }

    fn planes_of(frame: Frame, raw: &[u8], tone: &Tone) -> (Vec<u8>, Vec<u8>) {
        convert_whole(&Planes::new(frame, frame.width() as usize, tone).unwrap(), raw)
    }

    #[test]
    fn video_range_greys() {
        let frame = Frame::whole(16, 8);
        for (code, expected) in [(BLACK, 16), (WHITE, 235), (0, 16)] {
            let (luma, chroma) = planes_of(frame, &flat(frame, [code; 3]), &plain());
            assert!(luma.iter().all(|&value| value == expected), "raw {code}: {luma:?}");
            assert!(chroma.iter().all(|&value| value == 128), "raw {code}: {chroma:?}");
        }
        let (luma, chroma) = planes_of(frame, &flat(frame, [BLACK + 173; 3]), &plain());
        assert!(luma.iter().all(|&value| value == 117), "18 % grey is code 118 of 255, 117 in video range: {luma:?}");
        assert!(chroma.iter().all(|&value| value == 128));
    }

    #[test]
    fn rec_709_primaries() {
        let frame = Frame::whole(8, 4);
        let values = |colours: [u32; 3]| {
            let (luma, chroma) = planes_of(frame, &flat(frame, colours), &plain());
            assert!(luma.iter().all(|&value| value == luma[0]) && chroma.chunks(2).all(|pair| pair == &chroma[..2]));
            (luma[0], chroma[0], chroma[1])
        };
        assert_eq!(values([WHITE, BLACK, BLACK]), (63, 102, 240), "red");
        assert_eq!(values([BLACK, WHITE, BLACK]), (173, 42, 26), "green");
        assert_eq!(values([BLACK, BLACK, WHITE]), (32, 240, 118), "blue");
        assert_eq!(values([WHITE, WHITE, BLACK]), (219, 16, 138), "yellow");
    }

    #[test]
    fn rec_601_primaries() {
        let frame = Frame::whole(8, 4);
        let mut planes = Planes::tight(frame, &plain()).unwrap().with_rec_601(&plain());
        assert_eq!(planes.size(), (8, 4), "a line as long as the frame is wide");
        let values = |planes: &Planes, colours: [u32; 3]| {
            let (luma, chroma) = convert_whole(planes, &flat(frame, colours));
            (luma[0], chroma[0], chroma[1])
        };
        assert_eq!(values(&planes, [WHITE, BLACK, BLACK]), (81, 90, 240), "red");
        assert_eq!(values(&planes, [BLACK, WHITE, BLACK]), (145, 54, 34), "green");
        assert_eq!(values(&planes, [BLACK, BLACK, WHITE]), (41, 240, 110), "blue");
        assert_eq!(values(&planes, [WHITE; 3]), (235, 128, 128), "white");
        planes.set_tone(&plain());
        assert_eq!(values(&planes, [WHITE, BLACK, BLACK]), (81, 90, 240), "a recipe taken anew keeps the colours");
    }

    #[test]
    fn luma_and_chroma_sampling() {
        let frame = Frame::whole(8, 4);
        let bytes = packed(frame, |x, _| if x < 4 { BLACK } else { WHITE });
        let (luma, chroma) = planes_of(frame, &bytes, &plain());
        for line in luma.chunks(8) {
            assert_eq!(line[..3], [16; 3], "black up to the pixel before the edge");
            assert!(line[3] > 16 && line[3] < 235, "the pixel whose four samples straddle the edge: {}", line[3]);
            assert_eq!(line[4..], [235; 4]);
        }
        assert_eq!(chroma, [128; 16], "every quad is black or white");
        let rows = packed(frame, |_, y| if y < 2 { WHITE } else { BLACK });
        let (luma, _) = planes_of(frame, &rows, &plain());
        assert_eq!((luma[0], luma[8], luma[24]), (235, luma[8], 16));
        assert!(luma[8] > 16 && luma[8] < 235);
    }

    #[test]
    fn last_column_and_line() {
        let frame = Frame::whole(8, 4);
        let bytes = packed(frame, |x, y| BLACK + 100 * x + 60 * y);
        let (luma, _) = planes_of(frame, &bytes, &plain());
        let wider = Frame::whole(12, 6);
        let mirrored =
            packed(wider, |x, y| BLACK + 100 * if x < 8 { x } else { 14 - x } + 60 * if y < 4 { y } else { 6 - y });
        let (reference, _) = planes_of(wider, &mirrored, &plain());
        for y in 0..4 {
            assert_eq!(luma[8 * y..8 * y + 8], reference[12 * y..12 * y + 8], "line {y}");
        }
    }

    #[test]
    fn parts_equal_whole() {
        let frame = Frame::whole(24, 12);
        let bytes = packed(frame, |x, y| (37 * x + 101 * y + x * y) % 1024);
        let gains = [2 * UNITY, UNITY, UNITY + UNITY / 2];
        let tone = Tone::new(Recipe { gains, veil: 20, matrix: matrix(gains) });
        let (whole_luma, whole_chroma) = planes_of(frame, &bytes, &tone);
        let planes = Planes::new(frame, 24, &tone).unwrap();
        let (mut luma, mut chroma, mut scratch) = (vec![0u8; 24 * 12], vec![0u8; 24 * 6], Scratch::default());
        for lines in [0..4, 4..6, 6..12] {
            let (luma_part, chroma_part) = (24 * lines.start..24 * lines.end, 12 * lines.start..12 * lines.end);
            planes.convert(&bytes, lines, &mut luma[luma_part], Chroma::Pairs(&mut chroma[chroma_part]), &mut scratch);
        }
        assert_eq!((luma, chroma), (whole_luma.clone(), whole_chroma), "with a scratch that held the part before");
        assert!(whole_luma.iter().any(|&value| value != whole_luma[0]));
    }

    #[test]
    fn wide_stride() {
        let frame = Frame::whole(8, 4);
        let planes = Planes::new(frame, 16, &plain()).unwrap();
        assert_eq!(planes.bytes(), (64, 32));
        let (mut luma, mut chroma) = (vec![7u8; 64], vec![7u8; 32]);
        planes.convert(&flat(frame, [WHITE; 3]), 0..4, &mut luma, Chroma::Pairs(&mut chroma), &mut Scratch::default());
        for line in luma.chunks(16) {
            assert_eq!((&line[..8], &line[8..]), (&[235u8; 8][..], &[7u8; 8][..]));
        }
        for line in chroma.chunks(16) {
            assert_eq!((&line[..8], &line[8..]), (&[128u8; 8][..], &[7u8; 8][..]));
        }
    }

    #[test]
    fn tone_balance() {
        let frame = Frame::whole(8, 4);
        let bytes = flat(frame, [BLACK + 200, BLACK + 400, BLACK + 800]);
        let warm = balanced([2 * UNITY, UNITY, UNITY / 2]);
        let (_, chroma) = planes_of(frame, &bytes, &warm);
        assert!(chroma.iter().all(|&value| value.abs_diff(128) <= 1), "balanced to grey: {chroma:?}");
        let mut planes = Planes::new(frame, 8, &plain()).unwrap();
        let (_, tinted) = convert_whole(&planes, &bytes);
        assert!(tinted[0] > 140, "blue without the balance: {tinted:?}");
        planes.set_tone(&warm);
        assert_eq!(convert_whole(&planes, &bytes).1, chroma);
    }

    #[test]
    fn tone_matrix() {
        let frame = Frame::whole(8, 4);
        let daylight = [gain(DAYLIGHT.red), UNITY, gain(DAYLIGHT.blue)];
        let tuned = Tone::new(Recipe { matrix: matrix(daylight), ..Recipe::PLAIN });
        for code in [BLACK, BLACK + 20, BLACK + 173, BLACK + 600, WHITE] {
            let (luma, chroma) = planes_of(frame, &flat(frame, [code; 3]), &tuned);
            let (plain_luma, _) = planes_of(frame, &flat(frame, [code; 3]), &plain());
            assert!(
                luma.iter().zip(&plain_luma).all(|(a, b)| a.abs_diff(*b) <= 1),
                "raw {code}: luma {} against {}",
                luma[0],
                plain_luma[0]
            );
            assert!(chroma.iter().all(|&value| value.abs_diff(128) <= 1), "raw {code}: chroma {:?}", &chroma[..2]);
        }
        let reddish = flat(frame, [BLACK + 400, BLACK + 250, BLACK + 200]);
        let ((_, dull), (_, vivid)) = (planes_of(frame, &reddish, &plain()), planes_of(frame, &reddish, &tuned));
        assert!(
            vivid[1] > dull[1] + 10 && vivid[0] < dull[0],
            "a reddish thing is redder: {:?} against {:?}",
            &vivid[..2],
            &dull[..2]
        );
        let strong = Tone::new(Recipe {
            matrix: [[4 * UNITY, -3 * UNITY, 0], [0, UNITY, 0], [0, -3 * UNITY, 4 * UNITY]],
            ..Recipe::PLAIN
        });
        let (luma, chroma) = planes_of(frame, &flat(frame, [WHITE, BLACK, BLACK]), &strong);
        assert!(
            luma.iter().all(|&value| (16..=235).contains(&value))
                && chroma.iter().all(|&value| (16..=240).contains(&value)),
            "what a matrix takes past the range is cut off: {} {:?}",
            luma[0],
            &chroma[..2]
        );
    }

    #[test]
    fn lens_lift() {
        let frame = Frame::whole(16, 8);
        let area = SensorArea { left: 0, top: 0, scale: 204 };
        let shaded = |lens: Lens| Planes::new(frame, 16, &plain()).unwrap().with_lens(Lens { area, ..lens });
        let convert = |lens: Lens, colours: [u32; 3]| convert_whole(&shaded(lens), &flat(frame, colours));
        let doubling = fake::lens(|_, _, _| 2048);
        assert_eq!(
            convert(doubling, [BLACK + 100, BLACK + 150, BLACK + 200]),
            planes_of(frame, &flat(frame, [BLACK + 200, BLACK + 300, BLACK + 400]), &plain()),
            "a lens that halves all light everywhere: every sample is doubled"
        );
        let (luma, chroma) = convert(fake::lens(|_, _, column| 1024 + 64 * column as u16), [BLACK + 200; 3]);
        assert!(luma[..16].windows(2).all(|pair| pair[0] <= pair[1]) && luma[15] > luma[0] + 40, "{:?}", &luma[..16]);
        assert!(chroma.iter().all(|&value| value == 128), "a lens that shades all colours alike leaves grey grey");
        let (_, tinted) = convert(fake::lens(|colour, _, _| if colour == RED { 2048 } else { 1024 }), [BLACK + 200; 3]);
        assert!(tinted[1] > 150 && tinted[0] < 128, "red alone lifted: the picture turns red: {:?}", &tinted[..2]);
        let (rows, _) = convert(fake::lens(|_, row, _| 1024 + 64 * row as u16), [BLACK + 200; 3]);
        assert!(rows[7 * 16] > rows[0] + 20, "gains that rise down the sensor: {} and {}", rows[0], rows[7 * 16]);
        let raw = flat(frame, [BLACK + 100, BLACK + 150, BLACK + 200]);
        let (mut scratch, mut luma, mut chroma) = (Scratch::default(), vec![0u8; 16 * 8], vec![0u8; 16 * 4]);
        for lens in [doubling, fake::lens(|_, row, column| 1024 + 32 * (row + column) as u16)] {
            shaded(lens).convert(&raw, 0..8, &mut luma, Chroma::Pairs(&mut chroma), &mut scratch);
            let clean = convert_whole(&shaded(lens), &raw);
            assert_eq!((&luma, &chroma), (&clean.0, &clean.1), "a scratch that lifted by another lens");
        }
    }

    #[test]
    fn chroma_apart() {
        // A call's picture: the high speed mode's frame in Rec. 601, the lens made up for over that mode's window.
        let frame = Mode::HighSpeed.frame();
        let gains = [2 * UNITY, UNITY, UNITY + UNITY / 2];
        let tone = Tone::new(Recipe { gains, veil: 20, matrix: matrix(gains) });
        let planes = Planes::tight(frame, &tone).unwrap().with_rec_601(&tone).with_lens(Mode::HighSpeed.lens());
        let (width, height) = planes.size();
        let raw = packed(frame, |x, y| (37 * x + 101 * y + x * y) % 1024);
        let (luma, pairs) = convert_whole(&planes, &raw);
        // The picture made in parts from each of `cuts` to the next, its chroma apart.
        let apart = |cuts: &[usize]| {
            let mut made = [vec![0u8; width * height], vec![0u8; width * height / 4], vec![0u8; width * height / 4]];
            let mut scratch = Scratch::default();
            for ends in cuts.windows(2) {
                let [luma, blue, red] = made.each_mut();
                let colours = ends[0] / 2 * width / 2..ends[1] / 2 * width / 2;
                let chroma = Chroma::Apart([&mut blue[colours.clone()], &mut red[colours]]);
                planes.convert(
                    &raw,
                    ends[0]..ends[1],
                    &mut luma[ends[0] * width..ends[1] * width],
                    chroma,
                    &mut scratch,
                );
            }
            made
        };
        assert!(luma.iter().any(|&sample| sample != luma[0]), "a picture with something in it");
        let colour = |first: usize| pairs.iter().skip(first).step_by(2).copied().collect::<Vec<u8>>();
        let whole = apart(&[0, height]);
        assert!(whole == [luma, colour(0), colour(1)], "the samples of pairs, each colour in a plane of its own");
        assert!(apart(&[0, 60, 62, height]) == whole, "made in parts as in one piece");
    }

    #[test]
    fn refused_frames() {
        let planes =
            |width: u32, lines: u32, stride: usize| Planes::new(Frame::whole(width, lines), stride, &plain()).is_some();
        assert!(planes(1280, 720, 1280));
        assert!(!planes(1282, 720, 1296), "not a multiple of four pixels");
        assert!(!planes(1280, 721, 1280));
        assert!(!planes(1280, 720, 1276));
        assert!(!planes(0, 720, 16));
        assert!(!planes(8, 0, 8));
    }
}
