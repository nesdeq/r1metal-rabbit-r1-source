//! Raw frames read out of their packed lines, and the viewfinder picture made of them.

use crate::base::bits::BYTE_BITS;
use crate::base::pixel::PixelFormat;
use crate::base::sampling::Turn;
use crate::base::simd::{self, CODE_BITS, CODE_MASK, PACKED_BYTES, PACKED_CODES};
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::Frame;
use crate::hw::camera::frame::WHITE;
use crate::hw::camera::measure::Measure;
use crate::hw::camera::measure::ZONES;
use crate::hw::camera::shading::Axis;
use crate::hw::camera::shading::Correction;
use crate::hw::camera::shading::Lens;
use crate::hw::camera::shading::Span;
use crate::hw::camera::tone::RUN;
use crate::hw::camera::tone::Scratch;
use crate::hw::camera::tone::Tone;
use alloc::vec::Vec;
use core::ops::Range;

const LEVEL_ROWS: usize = 4;
// The viewfinder reads a quad's two codes of a line as one word.
const PAIR_BYTES: usize = 4;
/// The error text where `Finder::new` makes none.
pub const NO_FINDER: &str = "no viewfinder for this screen";

// The byte of a line where the group of packed codes that holds pixel `x` begins.
const fn group_byte(x: usize) -> usize {
    x / PACKED_CODES * PACKED_BYTES
}

// The byte and bit of a line the word of the pair of codes from pixel `x` on is read at: a byte early where the word would pass the line's end.
const fn pair_place(x: usize, line_bytes: usize) -> (usize, u32) {
    let (byte, shift) = (x * CODE_BITS / BYTE_BITS, (x * CODE_BITS % BYTE_BITS) as u32);
    if byte + PAIR_BYTES > line_bytes { (byte - 1, shift + u8::BITS) } else { (byte, shift) }
}

#[inline]
fn read_pair(line: &[u8], byte: usize, shift: u32) -> (u32, u32) {
    let (bytes, mask) = (&line[byte..byte + PAIR_BYTES], u32::from(CODE_MASK));
    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> shift;
    (word & mask, word >> CODE_BITS & mask)
}

#[derive(Clone, Copy)]
pub(super) struct Raw<'a> {
    bytes: &'a [u8],
    frame: Frame,
}

impl<'a> Raw<'a> {
    pub(super) fn new(bytes: &'a [u8], frame: Frame) -> Option<Raw<'a>> {
        let is_even = frame.width().is_multiple_of(2) && frame.lines.is_multiple_of(2);
        (is_even && bytes.len() >= frame.bytes()).then_some(Raw { bytes, frame })
    }

    #[cfg(test)]
    const fn width(&self) -> usize {
        self.frame.width() as usize
    }

    #[cfg(test)]
    const fn lines(&self) -> usize {
        self.frame.lines as usize
    }

    #[cfg(test)]
    pub(super) fn at(&self, x: usize, y: usize) -> u32 {
        let mut pair = [0u16; 2];
        self.line(y, x & !1, &mut pair);
        u32::from(pair[x % 2])
    }

    // Reads as many pixels of line `y` as `codes` holds, from the even place `left` on.
    pub(super) fn line(&self, y: usize, left: usize, codes: &mut [u16]) {
        let stride = self.frame.line_bytes() as usize;
        let line = &self.bytes[y * stride..(y + 1) * stride];
        let (byte, skip) = (group_byte(left), left % PACKED_CODES);
        if skip == 0 {
            return simd::unpack(&line[byte..], codes);
        }
        // A place inside a group: the group's codes from the place on, then whole groups.
        let mut group = [0u16; PACKED_CODES];
        simd::unpack(&line[byte..], &mut group);
        let head = codes.len().min(PACKED_CODES - skip);
        codes[..head].copy_from_slice(&group[skip..skip + head]);
        if head < codes.len() {
            simd::unpack(&line[byte + PACKED_BYTES..], &mut codes[head..]);
        }
    }
}

/// A square part of a frame, in pixels, with its corner on a colour quad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Square {
    pub(super) left: u32,
    pub(super) top: u32,
    pub(super) side: u32,
}

impl Square {
    /// The largest square in the middle of a frame.
    pub const fn middle_of(frame: Frame) -> Square {
        let side = if frame.width() < frame.lines { frame.width() } else { frame.lines } & !1;
        Square { left: ((frame.width() - side) / 2) & !1, top: ((frame.lines - side) / 2) & !1, side }
    }

    // The square as a window of the frame.
    pub(crate) const fn window(self) -> Window {
        Window { left: self.left, top: self.top, width: self.side, lines: self.side }
    }
}

/// A part of a frame of any shape, in pixels, with its corner on a colour quad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
    left: u32,
    top: u32,
    width: u32,
    lines: u32,
}

impl Window {
    // All of a frame.
    pub(crate) const fn whole(frame: Frame) -> Window {
        Window { left: 0, top: 0, width: frame.width(), lines: frame.lines }
    }

    fn on_quads_in(&self, frame: Frame) -> bool {
        let is_inside = self.left + self.width <= frame.width() && self.top + self.lines <= frame.lines;
        let is_even = [self.left, self.top, self.width, self.lines, frame.width(), frame.lines]
            .iter()
            .all(|length| length.is_multiple_of(2));
        is_inside && is_even && self.width != 0 && self.lines != 0
    }
}

/// Draws a part of raw frames as a picture of fewer pixels, each from one colour quad of the sensor.
pub struct Finder {
    frame: Frame,
    view: Window,
    correction: Option<Correction>,
    columns: Vec<(usize, u32)>,
    rows: Vec<usize>,
    width: usize,
    height: usize,
    turn: Turn,
    format: PixelFormat,
}

impl Finder {
    /// A finder that draws `view` as `width` by `height` pixels; none if the view is not inside the frame.
    pub fn new(
        frame: Frame,
        view: Window,
        width: usize,
        height: usize,
        turn: Turn,
        format: PixelFormat,
    ) -> Option<Finder> {
        if !view.on_quads_in(frame) || width == 0 || height == 0 {
            return None;
        }
        let (column_count, row_count) = if turn.is_sideways() { (height, width) } else { (width, height) };
        let spread = |start: u32, length: u32, pixels: usize| {
            (0..pixels).map(move |pixel| start as usize + 2 * (pixel * (length as usize / 2) / pixels))
        };
        let line_bytes = frame.line_bytes() as usize;
        let columns = spread(view.left, view.width, column_count).map(|x| pair_place(x, line_bytes)).collect();
        let rows = spread(view.top, view.lines, row_count).collect();
        Some(Finder { frame, view, correction: None, columns, rows, width, height, turn, format })
    }

    /// The same finder, making up for the lens's shading in every quad before it measures and draws it.
    pub fn with_lens(mut self, lens: Lens) -> Finder {
        let (area, view) = (lens.area, self.view);
        let first = area.left + area.scale * view.left;
        let span = Span::evenly(Axis::Horizontal, first, area.scale * view.width, self.columns.len(), false);
        // A quad counts by the sensor's row through its middle.
        let lines = self.rows.iter().map(|row| area.top + area.scale * (*row as u32 + 1)).collect();
        self.correction = Some(Correction { shading: lens.shading, axis: Axis::Horizontal, span, lines });
        self
    }

    /// The picture's width and height in pixels.
    pub const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[cfg(test)]
    fn draw(&self, raw: &[u8], tone: &Tone, picture: &mut [u32]) -> Measure {
        self.draw_lines(raw, tone, 0..self.height, picture)
    }

    /// Draws the picture's lines `lines` of one frame into `part`, which holds just those lines, and measures them.
    pub fn draw_lines(&self, raw: &[u8], tone: &Tone, lines: Range<usize>, part: &mut [u32]) -> Measure {
        let (width, height) = (self.width, self.height);
        assert!(lines.start <= lines.end && lines.end <= height, "lines {lines:?} of a picture of {height}");
        assert!(raw.len() >= self.frame.bytes() && part.len() >= lines.len() * width, "frame or picture too short");
        let stride = self.frame.line_bytes() as usize;
        let (start, column_step, row_step) = self.turn.walk(width, height);
        let is_by_column = self.turn.is_sideways();
        let is_rising = if is_by_column { column_step > 0 } else { row_step > 0 };
        let span = if is_rising { lines.clone() } else { height - lines.end..height - lines.start };
        let (columns, rows) = if is_by_column { (span, 0..self.rows.len()) } else { (0..self.columns.len(), span) };
        let first = start + columns.start as isize * column_step - (lines.start * width) as isize;
        let mut measure = Measure::default();
        // A row is worked run by run, each step of the work over a whole run before the next: red, green, blue and the brightest pixel of each quad, then the words.
        let (mut colours, mut peaks, mut words) = ([[0u16; RUN]; 3], [0u16; RUN], [0u32; RUN]);
        let (mut scratch, places) = (Scratch::new(), self.columns.len());
        for step in rows {
            let row = self.rows[step];
            let correction = self
                .correction
                .as_ref()
                .map(|correction| (correction, correction.shading.profile(correction.axis, correction.lines[step])));
            let upper = &raw[row * stride..(row + 1) * stride];
            let lower = &raw[(row + 1) * stride..(row + 2) * stride];
            let mut at = first + step as isize * row_step;
            let zones = &mut measure.zones[step * ZONES / self.rows.len()];
            let is_counted = step.is_multiple_of(LEVEL_ROWS);
            let mut next = columns.start;
            while next < columns.end {
                let end = columns.end.min(next + RUN);
                let count = end - next;
                let [reds, greens, blues] = colours.each_mut().map(|codes| codes[..count].iter_mut());
                let quads = self.columns[next..end].iter().zip(reds).zip(greens).zip(blues).zip(&mut peaks[..count]);
                // A quad's codes are read in place: a line's codes unpacked whole and picked after cost the A53 more, by llvm-mca's model.
                for ((((&(byte, shift), red), green), blue), peak) in quads {
                    let (green_b, blue_code) = read_pair(upper, byte, shift);
                    let (red_code, green_r) = read_pair(lower, byte, shift);
                    *red = red_code as u16;
                    *green = ((green_b + green_r) / 2) as u16;
                    *blue = blue_code as u16;
                    *peak = red_code.max(blue_code).max(green_b).max(green_r) as u16;
                }
                // The lens's shading is made up for before anything is measured or toned; what the sensor clipped is told by the peaks as they were read.
                if let Some((correction, profile)) = &correction {
                    for (codes, points) in colours.iter_mut().zip(profile) {
                        correction.span.lift(points, next, &mut codes[..count]);
                    }
                }
                // The run is measured zone by zone, each a stretch of it.
                let mut zone_start = next;
                while zone_start < end {
                    let zone = zone_start * ZONES / places;
                    let zone_stop = ((zone + 1) * places).div_ceil(ZONES).min(end);
                    let (zone, stretch) = (&mut zones[zone], zone_start - next..zone_stop - next);
                    for (total, codes) in zone.sums.iter_mut().zip(&colours) {
                        *total += codes[stretch.clone()]
                            .iter()
                            .map(|&code| u32::from(code).saturating_sub(BLACK))
                            .sum::<u32>();
                    }
                    zone.quads += stretch.len() as u32;
                    // Counted in lanes of 32 bits: a count of a filter takes 64.
                    zone.clipped += peaks[stretch].iter().map(|&peak| u32::from(u32::from(peak) >= WHITE)).sum::<u32>();
                    zone_start = zone_stop;
                }
                let [reds, greens, blues] = colours.each_ref().map(|codes| &codes[..count]);
                if is_counted {
                    for ((&red, &green), &blue) in reds.iter().zip(greens).zip(blues) {
                        measure.levels.count([red, green, blue].map(u32::from));
                    }
                }
                tone.words([reds, greens, blues], self.format, &mut words[..count], &mut scratch);
                for &word in &words[..count] {
                    part[at as usize] = word;
                    at += column_step;
                }
                next = end;
            }
        }
        measure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::support::RGBX;
    use crate::hw::camera::frame::fake::flat;
    use crate::hw::camera::frame::fake::packed;
    use crate::hw::camera::measure::DARK_CODES;
    use crate::hw::camera::measure::Levels;
    use crate::hw::camera::measure::Zone;
    use crate::hw::camera::shading::SensorArea;
    use crate::hw::camera::shading::fake;
    use crate::hw::camera::tone::fake::plain;
    use alloc::vec;

    const UPRIGHT: Turn = Turn { quarters: 0, is_mirrored: false };

    #[test]
    fn packed_pixels() {
        let frame = Frame::whole(8, 4);
        let bytes = packed(frame, |x, y| 100 * y + x + 500);
        let raw = Raw::new(&bytes, frame).unwrap();
        assert_eq!((raw.width(), raw.lines()), (8, 4));
        for y in 0..4 {
            for x in 0..8 {
                assert_eq!(raw.at(x, y), (100 * y + x + 500) as u32, "column {x} of line {y}");
            }
        }
        assert_eq!(&bytes[..5], [0xF4, 0xD5, 0x67, 0xDF, 0x7D], "four pixels in five bytes, lowest bits first");
        assert!(Raw::new(&bytes[..bytes.len() - 1], frame).is_none());
        assert!(Raw::new(&bytes, Frame::whole(8, 3)).is_none());
    }

    #[test]
    fn line_end_pixels() {
        let frame = Frame::whole(1632, 2);
        let bytes = packed(frame, |x, y| if y == 0 { x % 1024 } else { 1023 });
        let raw = Raw::new(&bytes, frame).unwrap();
        assert_eq!((raw.at(1630, 0), raw.at(1631, 0)), (1630 % 1024, 1631 % 1024));
        assert_eq!(raw.at(1631, 1), 1023);
    }

    #[test]
    fn middle_square() {
        assert_eq!(Square::middle_of(Frame::whole(1632, 1224)), Square { left: 204, top: 0, side: 1224 });
        assert_eq!(Square::middle_of(Frame::whole(3264, 2448)), Square { left: 408, top: 0, side: 2448 });
        assert_eq!(Square::middle_of(Frame::whole(1280, 720)), Square { left: 280, top: 0, side: 720 });
        assert_eq!(Square::middle_of(Frame::whole(10, 16)), Square { left: 0, top: 2, side: 10 });
    }

    #[test]
    fn flat_frame() {
        let frame = Frame::whole(16, 12);
        let bytes = flat(frame, [BLACK + 100, BLACK + 200, BLACK + 300]);
        let finder = Finder::new(frame, Square::middle_of(frame).window(), 6, 6, UPRIGHT, RGBX).unwrap();
        let mut picture = vec![0u32; 36];
        let measure = finder.draw(&bytes, &plain(), &mut picture);
        assert_eq!((measure.sums(), measure.quads(), measure.clipped()), ([3600, 7200, 10800], 36, 0));
        assert_eq!(measure.means(), Some([100, 200, 300]));
        let tone = plain();
        let [r, g, b] = tone.colour([BLACK + 100, BLACK + 200, BLACK + 300]);
        assert!(picture.iter().all(|&word| word == u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16));
        assert_eq!(Measure::default().means(), None);
    }

    #[test]
    fn lens_lift() {
        let frame = Frame::whole(64, 48);
        let lens = Lens {
            area: SensorArea { left: 0, top: 0, scale: 51 },
            ..fake::lens(|colour, row, column| 1024 + 64 * [column, row, 0][colour] as u16)
        };
        let finder = Finder::new(frame, Window::whole(frame), 32, 24, UPRIGHT, RGBX).unwrap().with_lens(lens);
        let mut picture = vec![0u32; 32 * 24];
        let measure = finder.draw(&flat(frame, [BLACK + 100; 3]), &plain(), &mut picture);
        let word = |lights: [u32; 3]| {
            let [r, g, b] = plain().colour(lights.map(|light| BLACK + light));
            u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16
        };
        assert_eq!(
            (picture[0], picture[32 * 24 - 1]),
            (word([101, 102, 100]), word([198, 197, 100])),
            "red rises along the sensor's rows and green down them, as this lens has it; blue stays"
        );
        assert_eq!(
            (measure.zones[0][0].means(), measure.zones[15][15].means()),
            (Some([102, 104, 100]), Some([196, 197, 100])),
            "and what is measured is what was lifted"
        );
        let bright = finder.draw(&flat(frame, [WHITE - 10; 3]), &plain(), &mut picture);
        assert_eq!(
            (bright.clipped(), picture[32 * 24 - 1]),
            (0, word([WHITE - BLACK, WHITE - BLACK, WHITE - BLACK - 10])),
            "a colour lifted to the limit stays there, and that is no clipping of the sensor's"
        );
        let cropped = Lens { area: SensorArea { left: 1632, top: 0, scale: 25 }, ..lens };
        let half = Finder::new(frame, Window::whole(frame), 32, 24, UPRIGHT, RGBX).unwrap().with_lens(cropped);
        half.draw(&flat(frame, [BLACK + 100; 3]), &plain(), &mut picture);
        assert_eq!(picture[0], word([150, 100, 100]), "frames that begin in the sensor's middle take its gains there");
    }

    #[test]
    fn clipped_quads() {
        let frame = Frame::whole(8, 8);
        let bytes = packed(frame, |x, y| if x < 2 && y < 2 { WHITE } else { BLACK });
        let finder = Finder::new(frame, Square::middle_of(frame).window(), 4, 4, UPRIGHT, RGBX).unwrap();
        let measure = finder.draw(&bytes, &plain(), &mut [0u32; 16]);
        assert_eq!((measure.quads(), measure.clipped()), (16, 1));
        assert_eq!(
            measure.zones[0][0],
            Zone { sums: [WHITE - BLACK; 3], quads: 1, clipped: 1 },
            "the quad in the sensor's first corner"
        );
    }

    #[test]
    fn zones_ignore_turn() {
        let frame = Frame::whole(64, 64);
        let bytes = packed(frame, |x, y| {
            BLACK
                + if y < 32 {
                    100
                } else if x < 32 {
                    300
                } else {
                    500
                }
        });
        for quarters in 0..4 {
            let turn = Turn { quarters, is_mirrored: quarters > 1 };
            let finder = Finder::new(frame, Square::middle_of(frame).window(), 32, 32, turn, RGBX).unwrap();
            let measure = finder.draw(&bytes, &plain(), &mut vec![0u32; 32 * 32]);
            assert!(
                measure.zones.iter().flatten().all(|zone| zone.quads == 4 && zone.clipped == 0),
                "two quads each way in every zone"
            );
            let means = |row: usize, column: usize| measure.zones[row][column].means().unwrap();
            assert_eq!(
                (means(0, 0), means(7, 15)),
                ([100; 3], [100; 3]),
                "{quarters} quarter turns: the sensor's upper rows"
            );
            assert_eq!((means(8, 0), means(15, 7)), ([300; 3], [300; 3]), "the lower rows' first columns");
            assert_eq!((means(8, 8), means(15, 15)), ([500; 3], [500; 3]));
            assert_eq!(measure.means(), Some([250; 3]));
        }
        assert_eq!(Zone::default().means(), None);
    }

    #[test]
    fn dark_and_bright_levels() {
        let frame = Frame::whole(64, 64);
        let bytes = packed(frame, |x, y| match (x / 2, y % 2, x % 2) {
            (0, 0, 1) => BLACK + 2,
            (0, 1, 0) => BLACK + 9,
            (0, _, _) => BLACK + 5,
            (31, _, _) => BLACK + 900,
            _ => BLACK + 200,
        });
        let finder = Finder::new(frame, Square::middle_of(frame).window(), 32, 32, UPRIGHT, RGBX).unwrap();
        let levels = finder.draw(&bytes, &plain(), &mut vec![0u32; 32 * 32]).levels;
        assert_eq!(levels.quads, 8 * 32, "one row in four is counted");
        assert_eq!(
            [0, 1, 2].map(|colour| levels.dark(colour, 100)),
            [Some(9), Some(5), Some(2)],
            "the darkest hundredth of red, green and blue"
        );
        assert_eq!(
            levels.dark(1, 2),
            Some(DARK_CODES as u32),
            "half of the quads lie over every code that is counted singly"
        );
        assert_eq!(levels.bright(100), Some(904), "the brightest hundredth, to the middle of its step of 16");
        assert_eq!(levels.bright(2), Some(200));
        assert_eq!((Levels::default().dark(1, 100), Levels::default().bright(100)), (None, None));
        let whole = finder.draw(&flat(frame, [WHITE; 3]), &plain(), &mut vec![0u32; 32 * 32]);
        assert_eq!(whole.levels.bright(100), Some(952), "the sensor's limit lies in the last step");
    }

    #[test]
    fn even_quad_skips() {
        let frame = Frame::whole(16, 16);
        let bytes = packed(frame, |x, y| BLACK + 16 * (x / 2) + 128 * (y / 2));
        let finder = Finder::new(frame, Square::middle_of(frame).window(), 4, 4, UPRIGHT, RGBX).unwrap();
        assert_eq!(
            (&finder.columns[..], &finder.rows[..]),
            (&[(0, 0), (5, 0), (10, 0), (15, 0)][..], &[0, 4, 8, 12][..])
        );
        assert_eq!(finder.size(), (4, 4));
        let measure = finder.draw(&bytes, &plain(), &mut [0u32; 16]);
        assert_eq!(measure.sums()[1], (0..4).flat_map(|y| (0..4).map(move |x| 32 * x + 256 * y)).sum::<u64>());
    }

    fn quad_order(turn: Turn) -> Vec<usize> {
        let frame = Frame::whole(8, 4);
        let bytes = packed(frame, |x, y| BLACK + 100 * (1 + x / 2 + 4 * (y / 2)));
        let (width, height) = if turn.is_sideways() { (2, 4) } else { (4, 2) };
        let finder = Finder::new(frame, Window::whole(frame), width, height, turn, RGBX).unwrap();
        assert_eq!(finder.size(), (width, height));
        let mut picture = vec![0u32; 8];
        assert_eq!(finder.draw(&bytes, &plain(), &mut picture).quads(), 8);
        let greys: Vec<u32> = (0..8).map(|quad| u32::from(plain().colour([BLACK + 100 * (1 + quad); 3])[1])).collect();
        picture
            .iter()
            .map(|word| greys.iter().position(|grey| word >> 8 & 0xFF == *grey).expect("a grey of the frame"))
            .collect()
    }

    #[test]
    fn turned_shapes() {
        let order = |quarters: u8, is_mirrored: bool| quad_order(Turn { quarters, is_mirrored });
        assert_eq!(order(0, false), [0, 1, 2, 3, 4, 5, 6, 7], "four across, two down");
        assert_eq!(order(1, false), [4, 0, 5, 1, 6, 2, 7, 3], "a quarter turn clockwise: two across, four down");
        assert_eq!(order(2, false), [7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(order(3, false), [3, 7, 2, 6, 1, 5, 0, 4]);
        assert_eq!(order(0, true), [3, 2, 1, 0, 7, 6, 5, 4], "mirrored left to right");
        assert_eq!(order(1, true), [0, 4, 1, 5, 2, 6, 3, 7]);
        assert_eq!(order(2, true), [4, 5, 6, 7, 0, 1, 2, 3]);
        assert_eq!(order(3, true), [7, 3, 6, 2, 5, 1, 4, 0]);
        assert_eq!(order(5, false), order(1, false), "turns go round");
    }

    #[test]
    fn parts_equal_whole() {
        let frame = Frame::whole(24, 20);
        let view = Window { left: 4, top: 2, width: 16, lines: 12 };
        let bytes = packed(frame, |x, y| (37 * x + 101 * y + x * y) % 1024);
        for quarters in 0..4 {
            for is_mirrored in [false, true] {
                let turn = Turn { quarters, is_mirrored };
                let (width, height) = if turn.is_sideways() { (6, 8) } else { (8, 6) };
                let finder = Finder::new(frame, view, width, height, turn, RGBX).unwrap();
                let (mut whole, mut parts) = ([0u32; 48], [0u32; 48]);
                let measure = finder.draw(&bytes, &plain(), &mut whole);
                let mut sum = Measure::default();
                for (index, part) in parts.chunks_mut(4 * width).enumerate() {
                    let lines = 4 * index..4 * index + part.len() / width;
                    sum = sum.merge(finder.draw_lines(&bytes, &plain(), lines, part));
                }
                assert_eq!((parts, sum), (whole, measure), "{quarters} quarter turns, mirrored {is_mirrored}");
                assert!(whole.iter().any(|&word| word != whole[0]));
            }
        }
    }

    #[test]
    fn half_size_quads() {
        let frame = Frame::whole(1280, 720);
        let finder =
            Finder::new(frame, Window::whole(frame), 360, 640, Turn { quarters: 1, is_mirrored: false }, RGBX).unwrap();
        assert!(finder.rows.iter().copied().eq((0..360).map(|quad| 2 * quad)));
        assert!(
            finder.columns[..639].iter().copied().eq((0..639).map(|quad| (5 * quad / 2, (4 * quad % 8) as u32))),
            "quad n begins at bit 20 n of its line"
        );
        assert_eq!(finder.columns[639], (1596, 12), "the last one is read a byte early: its word ends with the line");
        let measure =
            finder.draw(&flat(frame, [BLACK + 10, BLACK + 20, BLACK + 30]), &plain(), &mut vec![0u32; 360 * 640]);
        assert_eq!((measure.quads(), measure.means()), (230_400, Some([10, 20, 30])));
        assert_eq!(Square::middle_of(frame).window(), Window { left: 280, top: 0, width: 720, lines: 720 });
    }

    #[test]
    fn refused_views() {
        let frame = Frame::whole(16, 12);
        let finder = |view: Square, side: usize| Finder::new(frame, view.window(), side, side, UPRIGHT, RGBX).is_some();
        assert!(finder(Square { left: 4, top: 0, side: 12 }, 6));
        assert!(!finder(Square { left: 6, top: 0, side: 12 }, 6), "past the right edge");
        assert!(!finder(Square { left: 0, top: 2, side: 12 }, 6), "past the bottom");
        assert!(!finder(Square { left: 1, top: 0, side: 10 }, 5), "not on a quad");
        assert!(!finder(Square { left: 0, top: 0, side: 0 }, 5));
        assert!(!finder(Square { left: 0, top: 0, side: 12 }, 0));
        let whole = Window::whole(frame);
        assert!(Finder::new(frame, whole, 8, 6, UPRIGHT, RGBX).is_some());
        assert!(Finder::new(frame, whole, 8, 0, UPRIGHT, RGBX).is_none());
        assert!(Finder::new(frame, Window { lines: 14, ..whole }, 8, 6, UPRIGHT, RGBX).is_none(), "past the bottom");
        assert!(Finder::new(frame, Window { width: 0, ..whole }, 8, 6, UPRIGHT, RGBX).is_none());
    }
}
