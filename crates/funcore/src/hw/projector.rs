//! Decoded pictures drawn for the screen: scaled down in whole steps, fitted, turned and in the screen's colours.

use crate::base::pixel::PixelFormat;
use crate::base::sampling::Turn;
use crate::base::sampling::{Place, blend, pick};
use crate::base::simd::Sample;
use crate::base::ycbcr::Chroma;
use crate::base::ycbcr::Colours;
use crate::base::ycbcr::WEIGHTS_601;
use crate::base::ycbcr::WEIGHTS_709;
use crate::codec::h264::REC_709;
use crate::codec::h264::read::Sequence;
use crate::hw::vdec::BLOCK_COLUMNS;
use crate::hw::vdec::CHROMA_BLOCK_LINES;
use crate::hw::vdec::LUMA_BLOCK_LINES;
use crate::hw::vdec::Layout;
use alloc::vec::Vec;
use core::ops::Range;

const MAX_STEP: usize = 16;
const CHROMA_SHARE: usize = 2;
const CHROMA_KINDS: usize = 2;
const BLOCK: usize = BLOCK_COLUMNS as usize;
const BAND_SLOTS: usize = 2;
const PLANES: usize = 2;
const LUMA: usize = 0;
const CHROMA: usize = 1;
const MATRIX_601: [u32; 2] = [5, 6];
const HD_WIDTH: u32 = 1280;
const SD_HEIGHT: u32 = 576;

fn weights(sequence: &Sequence) -> [i64; 3] {
    let (width, height) = sequence.size();
    match sequence.matrix {
        Some(REC_709) => WEIGHTS_709,
        Some(matrix) if MATRIX_601.contains(&matrix) => WEIGHTS_601,
        _ if width >= HD_WIDTH || height > SD_HEIGHT => WEIGHTS_709,
        _ => WEIGHTS_601,
    }
}

type PlanePlaces = [Place; PLANES];

fn copy(stored: &[u8], at: impl Fn(usize) -> usize, mut column: usize, out: &mut [u8]) {
    let mut done = 0;
    while done < out.len() {
        let (count, start) = ((BLOCK - column % BLOCK).min(out.len() - done), at(column));
        out[done..done + count].copy_from_slice(&stored[start..start + count]);
        done += count;
        column += count;
    }
}

#[inline(always)]
fn add(sums: &mut [u16], line: &[u8], step: usize) {
    for (sum, group) in sums.iter_mut().zip(line.chunks_exact(step)) {
        *sum += group.iter().map(|&sample| u16::from(sample)).sum::<u16>();
    }
}

#[inline(always)]
fn add_pairs(sums: &mut [u16], line: &[u8], step: usize) {
    for (sum, group) in sums.as_chunks_mut::<CHROMA_KINDS>().0.iter_mut().zip(line.chunks_exact(CHROMA_KINDS * step)) {
        for pair in group.as_chunks::<CHROMA_KINDS>().0 {
            (sum[0], sum[1]) = (sum[0] + u16::from(pair[0]), sum[1] + u16::from(pair[1]));
        }
    }
}

#[inline(always)]
fn means(sums: &[u16], count: usize, out: &mut [u8]) {
    let (half, shift) = ((count / 2) as u16, count.trailing_zeros());
    for (mean, sum) in out.iter_mut().zip(sums) {
        *mean = ((sum + half) >> shift) as u8;
    }
}

#[inline(always)]
fn mean_squares<const N: usize, const IS_CHROMA: bool>(block: &[u8], out: &mut [u8]) {
    let mut sums = [0u16; BLOCK];
    let (sums, width) = (&mut sums[..out.len()], out.len() * N);
    for line in 0..N {
        let line = &block[line * BLOCK..line * BLOCK + width];
        if IS_CHROMA { add_pairs(sums, line, N) } else { add(sums, line, N) }
    }
    means(sums, N * N, out);
}

// Makes each column of a block `count` samples across a line of a band: from its top down, or from its foot up where the turn walks back.
fn turn_band(unturned: &[u8], (count, size, is_back): (usize, usize, bool), band: &mut [u8]) {
    let turn = Turn { quarters: 1, is_mirrored: !is_back };
    if size == CHROMA_KINDS {
        Sample::turn(unturned.as_chunks::<CHROMA_KINDS>().0, count, turn, band.as_chunks_mut::<CHROMA_KINDS>().0);
    } else {
        Sample::turn(unturned, count, turn, band);
    }
}

fn to_picks(
    places: &[PlanePlaces],
    plane: usize,
    (is_back, is_flipped): (bool, bool),
    (samples, size): (usize, usize),
) -> Vec<Place> {
    let last = samples.saturating_sub(1);
    let at = |sample: u32| (size * if is_flipped { last - sample as usize } else { sample as usize }) as u32;
    let ordered = (0..places.len()).map(|index| places[if is_back { places.len() - 1 - index } else { index }][plane]);
    ordered.map(|place| Place { low: at(place.low), high: at(place.high), fraction: place.fraction }).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Plane {
    is_chroma: bool,
    step: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

impl Plane {
    const fn sample_size(&self) -> usize {
        if self.is_chroma { CHROMA_KINDS } else { 1 }
    }

    const fn length(&self, is_down: bool) -> usize {
        self.sample_size() * if is_down { self.height } else { self.width }
    }

    const fn bands(&self, is_down: bool) -> Bands {
        if is_down {
            Bands { first: self.left, unit: self.sample_size() * self.step, block: BLOCK, lines: self.width }
        } else {
            Bands {
                first: self.top,
                unit: self.step,
                block: if self.is_chroma { CHROMA_BLOCK_LINES as usize } else { LUMA_BLOCK_LINES as usize },
                lines: self.height,
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Bands {
    first: usize,
    unit: usize,
    block: usize,
    lines: usize,
}

impl Bands {
    fn band_of(&self, line: usize) -> (usize, usize, usize) {
        let band = (self.first + line * self.unit) / self.block;
        let start = |end: usize| (end * self.block).saturating_sub(self.first).div_ceil(self.unit).min(self.lines);
        (band, start(band), start(band + 1) - start(band))
    }

    const fn max_lines(&self) -> usize {
        self.block.div_ceil(self.unit)
    }
}

fn band_line<'a>(bands: &'a [Vec<u8>; BAND_SLOTS], cut: &Bands, length: usize, line: u32) -> &'a [u8] {
    let (band, first, _) = cut.band_of(line as usize);
    &bands[band % BAND_SLOTS][(line as usize - first) * length..][..length]
}

#[derive(Default)]
struct RowScratch {
    run: Vec<u8>,
    sums: Vec<u16>,
    // A band's samples down the picture as its block rows give them, before the turn makes its lines of them.
    unturned: Vec<u8>,
}

/// A thread's buffers from one part of a picture to the next; any thread's serves any projector.
#[derive(Default)]
pub struct Scratch {
    bands: [[Vec<u8>; BAND_SLOTS]; PLANES],
    band_numbers: [[Option<usize>; BAND_SLOTS]; PLANES],
    blends: [Vec<u16>; PLANES],
    samples: [Vec<u8>; PLANES],
    words: Vec<u32>,
    row: RowScratch,
}

impl Scratch {
    fn resize(&mut self, planes: &[Plane; PLANES], is_down: bool, pixels: usize) {
        for (index, plane) in planes.iter().enumerate() {
            let (lines, length) = (plane.bands(is_down).max_lines(), plane.length(is_down));
            self.bands[index].iter_mut().for_each(|band| band.resize(lines * length, 0));
            self.blends[index].resize(length, 0);
            self.samples[index].resize(pixels * plane.sample_size(), 0);
        }
        self.words.resize(pixels, 0);
        self.band_numbers = [[None; BAND_SLOTS]; PLANES];
    }
}

/// Draws one stream's pictures at one size and turn from luma and chroma planes of square means.
pub struct Projector {
    layout: Layout,
    planes: [Plane; PLANES],
    is_aligned: bool,
    columns: Vec<PlanePlaces>,
    rows: Vec<PlanePlaces>,
    picks: [Vec<Place>; PLANES],
    width: usize,
    height: usize,
    turn: Turn,
    format: PixelFormat,
    colours: Colours,
    is_exact: bool,
}

impl Projector {
    pub(crate) fn new(
        sequence: &Sequence,
        turn: Turn,
        width: usize,
        height: usize,
        format: PixelFormat,
    ) -> Option<Projector> {
        let layout = Layout::from_sequence(sequence)?;
        let ((view_width, view_height), [left, _, top, _]) = (sequence.size(), sequence.crop);
        let is_even =
            [left, top, view_width, view_height].iter().all(|length| length.is_multiple_of(CHROMA_SHARE as u32));
        let is_inside = left + view_width <= layout.stride && top + view_height <= layout.lines;
        if !is_even || !is_inside || view_width == 0 || view_height == 0 || width == 0 || height == 0 {
            return None;
        }
        let (view_width, view_height, left, top) =
            (view_width as usize, view_height as usize, left as usize, top as usize);
        let (horizontal, vertical) = if turn.is_sideways() { (height, width) } else { (width, height) };
        let mut step = 1;
        while 2 * step <= MAX_STEP && view_width / (2 * step) >= horizontal && view_height / (2 * step) >= vertical {
            step *= 2;
        }
        let chroma_step = (step / CHROMA_SHARE).max(1);
        let luma = Plane { is_chroma: false, step, left, top, width: view_width / step, height: view_height / step };
        let chroma = Plane {
            is_chroma: true,
            step: chroma_step,
            left,
            top: top / CHROMA_SHARE,
            width: view_width / CHROMA_SHARE / chroma_step,
            height: view_height / CHROMA_SHARE / chroma_step,
        };
        let planes = [luma, chroma];
        let to_places = |pixels: usize, samples: fn(&Plane) -> usize| -> Vec<PlanePlaces> {
            let place = |pixel: usize, samples: usize| Place::new(pixel, pixels, (samples, 1), samples);
            (0..pixels).map(|pixel| planes.map(|plane| place(pixel, samples(&plane)))).collect()
        };
        let (columns, rows) = (to_places(horizontal, |plane| plane.width), to_places(vertical, |plane| plane.height));
        // A picture turned by a quarter has the planes' columns for its lines, and its pixels go against the planes' order where the turn walks back.
        let ((_, step_x, step_y), is_sideways) = (turn.walk(width, height), turn.is_sideways());
        let (inner, is_back) = if is_sideways { (&rows, step_y < 0) } else { (&columns, step_x < 0) };
        let picks = core::array::from_fn(|index| {
            let plane: Plane = planes[index];
            let samples = if is_sideways { plane.height } else { plane.width };
            to_picks(inner, index, (is_back, is_sideways && is_back), (samples, plane.sample_size()))
        });
        Some(Projector {
            layout,
            planes,
            is_aligned: left.is_multiple_of(step) && top.is_multiple_of(step),
            columns,
            rows,
            picks,
            width,
            height,
            turn,
            format,
            colours: Colours::new(weights(sequence), sequence.is_full_range),
            is_exact: (luma.width, luma.height) == (horizontal, vertical)
                && (chroma.width, chroma.height) == (horizontal, vertical),
        })
    }

    /// Width and height in pixels.
    pub const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn at(&self, is_chroma: bool, column: usize, line: usize) -> usize {
        self.layout.at(is_chroma, column as u32, line as u32)
    }

    fn fill_row(
        &self,
        plane: &Plane,
        stored: &[u8],
        row: usize,
        start: usize,
        out: &mut [u8],
        RowScratch { run, sums, .. }: &mut RowScratch,
    ) {
        let (step, line, column) =
            (plane.step, plane.top + row * plane.step, plane.left + start * plane.sample_size() * plane.step);
        if step == 1 {
            return copy(stored, |column| self.at(plane.is_chroma, column, line), column, out);
        }
        run.resize(out.len() * step, 0);
        sums.clear();
        sums.resize(out.len(), 0);
        for source in line..line + step {
            copy(stored, |column| self.at(plane.is_chroma, column, source), column, run);
            if plane.is_chroma { add_pairs(sums, run, step) } else { add(sums, run, step) }
        }
        means(sums, step * step, out);
    }

    fn fill<const N: usize, const IS_CHROMA: bool>(
        &self,
        plane: &Plane,
        stored: &[u8],
        (is_down, is_back): (bool, bool),
        (start, count): (usize, usize),
        out: &mut [u8],
        RowScratch { unturned, .. }: &mut RowScratch,
    ) {
        let (size, length) = (plane.sample_size(), plane.length(is_down));
        if is_down {
            let (column, across) = (plane.left + start * size * N, count * size);
            unturned.resize(plane.height * across, 0);
            for (row, samples) in unturned.chunks_exact_mut(across).enumerate() {
                let block = &stored[self.at(IS_CHROMA, column, plane.top + row * N)..];
                if across * N == BLOCK {
                    mean_squares::<N, IS_CHROMA>(&block[..N * BLOCK], samples);
                } else {
                    mean_squares::<N, IS_CHROMA>(block, samples);
                }
            }
            return turn_band(unturned, (count, size, is_back), out);
        }
        let mut sample = 0;
        while sample < plane.width {
            let column = plane.left + sample * size * N;
            let run = ((BLOCK - column % BLOCK) / (size * N)).min(plane.width - sample);
            for (line, out) in out.chunks_exact_mut(length).take(count).enumerate() {
                let block = &stored[self.at(IS_CHROMA, column, plane.top + (start + line) * N)..];
                if run * size * N == BLOCK {
                    mean_squares::<N, IS_CHROMA>(
                        &block[..N * BLOCK],
                        &mut out[sample * size..sample * size + BLOCK / N],
                    );
                } else {
                    mean_squares::<N, IS_CHROMA>(block, &mut out[sample * size..(sample + run) * size]);
                }
            }
            sample += run;
        }
    }

    fn fill_by_rows(
        &self,
        plane: &Plane,
        stored: &[u8],
        (is_down, is_back): (bool, bool),
        (start, count): (usize, usize),
        out: &mut [u8],
        row_scratch: &mut RowScratch,
    ) {
        let (size, length) = (plane.sample_size(), plane.length(is_down));
        if !is_down {
            return out
                .chunks_exact_mut(length)
                .take(count)
                .enumerate()
                .for_each(|(line, out)| self.fill_row(plane, stored, start + line, 0, out, row_scratch));
        }
        // Taken out while the rows are made, as making a row borrows the rest of the scratch.
        let mut unturned = core::mem::take(&mut row_scratch.unturned);
        unturned.resize(plane.height * count * size, 0);
        for (row, samples) in unturned.chunks_exact_mut(count * size).enumerate() {
            self.fill_row(plane, stored, row, start, samples, row_scratch);
        }
        turn_band(&unturned, (count, size, is_back), out);
        row_scratch.unturned = unturned;
    }

    fn fill_band(
        &self,
        plane: &Plane,
        stored: &[u8],
        turn: (bool, bool),
        lines: (usize, usize),
        out: &mut [u8],
        row_scratch: &mut RowScratch,
    ) {
        match (plane.step, self.is_aligned, plane.is_chroma) {
            (1, true, false) => self.fill::<1, false>(plane, stored, turn, lines, out, row_scratch),
            (2, true, false) => self.fill::<2, false>(plane, stored, turn, lines, out, row_scratch),
            (4, true, false) => self.fill::<4, false>(plane, stored, turn, lines, out, row_scratch),
            (8, true, false) => self.fill::<8, false>(plane, stored, turn, lines, out, row_scratch),
            (MAX_STEP, true, false) => self.fill::<MAX_STEP, false>(plane, stored, turn, lines, out, row_scratch),
            (1, true, true) => self.fill::<1, true>(plane, stored, turn, lines, out, row_scratch),
            (2, true, true) => self.fill::<2, true>(plane, stored, turn, lines, out, row_scratch),
            (4, true, true) => self.fill::<4, true>(plane, stored, turn, lines, out, row_scratch),
            (8, true, true) => self.fill::<8, true>(plane, stored, turn, lines, out, row_scratch),
            _ => self.fill_by_rows(plane, stored, turn, lines, out, row_scratch),
        }
    }

    /// Draws `lines` of a stored picture into `part`, which holds just those lines.
    pub fn draw(&self, stored: &[u8], lines: Range<usize>, part: &mut [u32], scratch: &mut Scratch) {
        let (width, height) = (self.width, self.height);
        assert!(lines.start <= lines.end && lines.end <= height, "lines {lines:?} of a picture of {height}");
        assert!(
            stored.len() >= self.layout.picture_bytes() && part.len() >= lines.len() * width,
            "stored picture or picture too short"
        );
        let (_, step_x, step_y) = self.turn.walk(width, height);
        // A picture turned by a quarter has the planes' columns for its lines; `is_forward` lines follow the planes' order, and `is_back` pixels go against it.
        let is_down = self.turn.is_sideways();
        let (is_forward, is_back) = if is_down { (step_x > 0, step_y < 0) } else { (step_y > 0, step_x < 0) };
        let outer = if is_down { &self.columns } else { &self.rows };
        let (cuts, lengths) =
            (self.planes.map(|plane| plane.bands(is_down)), self.planes.map(|plane| plane.length(is_down)));
        scratch.resize(&self.planes, is_down, width);
        let Scratch { bands, band_numbers, blends, samples, words, row } = scratch;
        // The lines are drawn in the order of the planes' lines they come from, so that the stored picture is read from its start to its end.
        for index in 0..lines.len() {
            let line = if is_forward { lines.start + index } else { lines.end - 1 - index };
            let (place, out) = (
                outer[if is_forward { line } else { outer.len() - 1 - line }],
                &mut part[(line - lines.start) * width..][..width],
            );
            let plane_lines = [
                (LUMA, place[LUMA].low),
                (CHROMA, place[CHROMA].low),
                (LUMA, place[LUMA].high),
                (CHROMA, place[CHROMA].high),
            ];
            for (plane, plane_line) in
                plane_lines.into_iter().take(if self.is_exact { PLANES } else { BAND_SLOTS * PLANES })
            {
                let (band, first, count) = cuts[plane].band_of(plane_line as usize);
                if band_numbers[plane][band % BAND_SLOTS].replace(band) != Some(band) {
                    self.fill_band(
                        &self.planes[plane],
                        stored,
                        (is_down, is_back),
                        (first, count),
                        &mut bands[plane][band % BAND_SLOTS][..count * lengths[plane]],
                        row,
                    );
                }
            }
            let line_at = |plane: usize, line: u32| band_line(&bands[plane], &cuts[plane], lengths[plane], line);
            if !self.is_exact {
                for (plane, Place { low, high, fraction }) in place.into_iter().enumerate() {
                    blend(&mut blends[plane], line_at(plane, low), line_at(plane, high), fraction);
                }
                pick::<1>(&blends[LUMA], &self.picks[LUMA], &mut samples[LUMA]);
                pick::<CHROMA_KINDS>(&blends[CHROMA], &self.picks[CHROMA], &mut samples[CHROMA]);
                self.colours.row(&samples[LUMA], Chroma::Beside(&samples[CHROMA]), self.format, out);
            } else if is_down || !is_back {
                let (luma, chroma) = (line_at(LUMA, place[LUMA].low), line_at(CHROMA, place[CHROMA].low));
                self.colours.row(luma, Chroma::Beside(chroma), self.format, out);
            } else {
                let (luma, chroma) = (line_at(LUMA, place[LUMA].low), line_at(CHROMA, place[CHROMA].low));
                self.colours.row(luma, Chroma::Beside(chroma), self.format, words);
                out.iter_mut().rev().zip(words.iter()).for_each(|(pixel, &word)| *pixel = word);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::sampling::{PLACE_BITS, PLACE_UNIT, turn_words};
    use crate::codec::h264::read::samples::sized;
    use alloc::vec;

    const RGB: PixelFormat = PixelFormat::XRGB8888;
    const GREY: (u8, u8) = (128, 128);

    fn stream(columns: u32, rows: u32, crop: [u32; 4]) -> Sequence {
        Sequence { crop, ..sized(columns, rows) }
    }

    fn turned(quarters: u8) -> Turn {
        Turn { quarters, is_mirrored: false }
    }

    fn upright(sequence: &Sequence, width: usize, height: usize) -> Projector {
        Projector::new(sequence, turned(0), width, height, RGB).unwrap()
    }

    fn word(colours: &Colours, luma: u8, blue: u8, red: u8, format: PixelFormat) -> u32 {
        colours.word(i32::from(luma), i32::from(blue), i32::from(red), format)
    }

    fn stored(sequence: &Sequence, luma: impl Fn(u32, u32) -> u8, chroma: impl Fn(u32, u32) -> (u8, u8)) -> Vec<u8> {
        let layout = Layout::from_sequence(sequence).unwrap();
        let mut bytes = vec![0u8; layout.picture_bytes()];
        for line in 0..layout.lines {
            for column in 0..layout.stride {
                bytes[layout.at(false, column, line)] = luma(column, line);
            }
        }
        for line in 0..layout.lines / 2 {
            for column in 0..layout.stride / 2 {
                let (blue, red) = chroma(column, line);
                (bytes[layout.at(true, 2 * column, line)], bytes[layout.at(true, 2 * column + 1, line)]) = (blue, red);
            }
        }
        bytes
    }

    fn varied(sequence: &Sequence) -> Vec<u8> {
        stored(
            sequence,
            |column, line| (37 * column + 101 * line + column * line) as u8,
            |column, line| ((11 * column + line) as u8, (column + 13 * line) as u8),
        )
    }

    fn planes(projector: &Projector, stored: &[u8], stretch: usize) -> [Vec<u8>; PLANES] {
        let mut row_scratch = RowScratch::default();
        projector.planes.map(|plane| {
            let mut bytes = vec![0u8; plane.length(false) * plane.height];
            for (row, line) in bytes.chunks_exact_mut(plane.length(false)).enumerate() {
                for (index, out) in line.chunks_mut(plane.sample_size() * stretch).enumerate() {
                    projector.fill_row(&plane, stored, row, index * stretch, out, &mut row_scratch);
                }
            }
            bytes
        })
    }

    fn drawn(projector: &Projector, stored: &[u8]) -> Vec<u32> {
        let (width, height) = projector.size();
        let mut picture = vec![0u32; width * height];
        projector.draw(stored, 0..height, &mut picture, &mut Scratch::default());
        picture
    }

    #[test]
    fn matrix_choice() {
        let weights_of = |matrix: Option<u32>, columns: u32, rows: u32| {
            weights(&Sequence { matrix, ..stream(columns, rows, [0; 4]) })
        };
        assert_eq!(
            (weights_of(Some(1), 40, 23), weights_of(Some(5), 80, 45), weights_of(Some(6), 80, 45)),
            (WEIGHTS_709, WEIGHTS_601, WEIGHTS_601)
        );
        assert_eq!(
            (weights_of(None, 80, 45), weights_of(None, 40, 23), weights_of(Some(2), 45, 36)),
            (WEIGHTS_709, WEIGHTS_601, WEIGHTS_601),
            "unnamed: by the size"
        );
        assert_eq!(weights_of(None, 64, 48), WEIGHTS_709, "1024 by 768 is taller than standard definition");
    }

    #[test]
    fn scale_steps() {
        let plan = |columns: u32, rows: u32, crop: [u32; 4], quarters: u8, width: usize, height: usize| {
            let projector = Projector::new(&stream(columns, rows, crop), turned(quarters), width, height, RGB).unwrap();
            let [luma, chroma] = projector.planes;
            (
                luma.step,
                (luma.width, luma.height),
                (chroma.width, chroma.height),
                projector.is_exact,
                projector.is_aligned,
            )
        };
        assert_eq!(
            plan(80, 45, [0; 4], 1, 360, 640),
            (2, (640, 360), (640, 360), true, true),
            "a recording of ours as it was held: every second sample each way, and nothing to fit"
        );
        assert_eq!(plan(80, 45, [0; 4], 0, 640, 360), (2, (640, 360), (640, 360), true, true));
        assert_eq!(
            plan(80, 45, [0; 4], 0, 480, 270),
            (2, (640, 360), (640, 360), false, true),
            "held the other way it is fitted from the same planes"
        );
        assert_eq!(plan(120, 68, [0, 0, 0, 8], 0, 480, 270), (4, (480, 270), (480, 270), true, true));
        assert_eq!(plan(120, 68, [0, 0, 0, 8], 3, 360, 640), (2, (960, 540), (960, 540), false, true));
        assert_eq!(
            plan(40, 23, [0, 0, 0, 8], 0, 640, 360),
            (1, (640, 360), (320, 180), false, true),
            "nothing to leave out: chroma stays as coarse as it is stored"
        );
        assert_eq!(plan(54, 30, [0, 10, 0, 0], 2, 480, 270), (1, (854, 480), (427, 240), false, true));
        assert_eq!(plan(128, 4, [0; 4], 0, 480, 15).0, 4);
        assert_eq!(
            plan(4, 4, [0; 4], 0, 480, 480),
            (1, (64, 64), (32, 32), false, true),
            "a picture shown larger than it is"
        );
        assert_eq!(plan(128, 60, [0; 4], 0, 1, 1).0, MAX_STEP, "no step beyond the largest");
        assert_eq!(
            plan(8, 8, [2, 2, 0, 0], 0, 31, 32),
            (4, (31, 32), (31, 32), true, false),
            "squares that begin two columns into a block reach into the next"
        );
        assert_eq!(plan(8, 8, [0, 0, 6, 2], 0, 30, 30), (4, (32, 30), (32, 30), false, false));
        assert_eq!(
            plan(8, 8, [2, 2, 6, 2], 0, 62, 60),
            (2, (62, 60), (62, 60), true, true),
            "steps of two never part a pair of samples"
        );
        let planes = upright(&stream(8, 8, [2, 4, 6, 2]), 61, 60).planes;
        assert_eq!(
            planes,
            [
                Plane { is_chroma: false, step: 2, left: 2, top: 6, width: 61, height: 60 },
                Plane { is_chroma: true, step: 1, left: 2, top: 3, width: 61, height: 60 }
            ]
        );
    }

    const CROPS: [([u32; 4], (usize, usize)); 11] = [
        ([0; 4], (128, 128)),
        ([0; 4], (64, 64)),
        ([0; 4], (32, 32)),
        ([0; 4], (16, 16)),
        ([0; 4], (8, 8)),
        ([2, 4, 2, 6], (61, 60)),
        ([2, 4, 2, 6], (30, 30)),
        ([6, 2, 10, 2], (15, 14)),
        ([16, 12, 32, 0], (25, 24)),
        ([4, 0, 4, 0], (7, 7)),
        ([18, 0, 34, 2], (110, 92)),
    ];

    #[test]
    fn plane_means() {
        let mean = |sum: u32, count: u32| ((sum + count / 2) / count) as u8;
        let (light, colour) = (
            |column: u32, line: u32| (3 * column + 5 * line + column * line / 7) as u8,
            |column: u32, line: u32| ((7 * column + line) as u8, (column + 9 * line + column * line / 5) as u8),
        );
        for (crop, (width, height)) in CROPS {
            let sequence = stream(8, 8, crop);
            let (picture, projector) = (stored(&sequence, light, colour), upright(&sequence, width, height));
            let ([luma_plane, chroma_plane], [left, _, top, _]) = (projector.planes, crop);
            let (step, chroma_step) = (luma_plane.step as u32, chroma_plane.step as u32);
            let square = |step: u32, value: &dyn Fn(u32, u32) -> u8, column: u32, line: u32| {
                mean(
                    (0..step * step).map(|at| u32::from(value(column + at % step, line + at / step))).sum(),
                    step * step,
                )
            };
            for stretch in [1000, 5, 16] {
                let [luma, chroma] = planes(&projector, &picture, stretch);
                for (at, &actual) in luma.iter().enumerate() {
                    let (column, line) =
                        (left + (at % luma_plane.width) as u32 * step, top + (at / luma_plane.width) as u32 * step);
                    assert_eq!(
                        actual,
                        square(step, &light, column, line),
                        "luma {at} of {width}x{height} cut {crop:?} in stretches of {stretch}"
                    );
                }
                for (at, &actual) in chroma.as_chunks::<2>().0.iter().enumerate() {
                    let (column, line) = (
                        left / 2 + (at % chroma_plane.width) as u32 * chroma_step,
                        top / 2 + (at / chroma_plane.width) as u32 * chroma_step,
                    );
                    let expected = [
                        square(chroma_step, &|column, line| colour(column, line).0, column, line),
                        square(chroma_step, &|column, line| colour(column, line).1, column, line),
                    ];
                    assert_eq!(
                        actual, expected,
                        "chroma {at} of {width}x{height} cut {crop:?} in stretches of {stretch}"
                    );
                }
            }
        }
        let coarse = stream(16, 16, [0; 4]);
        let projector = upright(&coarse, 16, 16);
        let [luma, chroma] = planes(
            &projector,
            &stored(
                &coarse,
                |column, line| if (column / 16 + line / 16) % 2 == 0 { 255 } else { 1 },
                |column, _| if column < 64 { (255, 0) } else { (3, 200) },
            ),
            1000,
        );
        assert_eq!(projector.planes.map(|plane| plane.step), [16, 8]);
        assert!(
            (0..256).all(|at| luma[at] == if (at % 16 + at / 16) % 2 == 0 { 255 } else { 1 }),
            "256 samples of the largest value add up without loss"
        );
        assert!((0..256).all(|at| chroma[2 * at..2 * at + 2] == if at % 16 < 8 { [255, 0] } else { [3, 200] }));
    }

    #[test]
    fn band_lines() {
        for (crop, (width, height)) in CROPS {
            let sequence = stream(8, 8, crop);
            let (picture, projector) = (varied(&sequence), upright(&sequence, width, height));
            let references = planes(&projector, &picture, 1000);
            for (plane, reference) in projector.planes.iter().zip(&references) {
                let size = plane.sample_size();
                for (is_down, is_back) in [(false, false), (true, false), (true, true)] {
                    let (cut, length, mut line, mut bands) = (plane.bands(is_down), plane.length(is_down), 0, 0);
                    while line < cut.lines {
                        let (band, first, count) = cut.band_of(line);
                        assert_eq!(
                            (first, cut.band_of(first + count - 1).0, count <= cut.max_lines()),
                            (line, band, true),
                            "the bands follow each other"
                        );
                        let mut out = vec![0u8; count * length];
                        projector.fill_band(
                            plane,
                            &picture,
                            (is_down, is_back),
                            (first, count),
                            &mut out,
                            &mut RowScratch::default(),
                        );
                        for (index, actual) in out.chunks_exact(length).enumerate() {
                            let expected: Vec<u8> = if is_down {
                                let column = (0..plane.height)
                                    .flat_map(|row| &reference[(row * plane.width + first + index) * size..][..size])
                                    .copied()
                                    .collect::<Vec<u8>>();
                                if is_back {
                                    column.chunks_exact(size).rev().flatten().copied().collect()
                                } else {
                                    column
                                }
                            } else {
                                reference[(first + index) * length..][..length].to_vec()
                            };
                            assert!(
                                actual == expected,
                                "line {} of {width}x{height} cut {crop:?}, chroma {}, down {is_down}, back {is_back}",
                                first + index,
                                plane.is_chroma
                            );
                        }
                        (line, bands) = (first + count, bands + 1);
                    }
                    assert!(bands >= cut.lines.div_ceil(cut.max_lines()), "{bands} bands for {} lines", cut.lines);
                }
            }
        }
        let cut = Bands { first: 6, unit: 4, block: 32, lines: 30 };
        assert_eq!(
            [cut.band_of(0), cut.band_of(6), cut.band_of(7), cut.band_of(29)],
            [(0, 0, 7), (0, 0, 7), (1, 7, 8), (3, 23, 7)],
            "a first band cut short, and a line that reaches into the next block belongs where it begins"
        );
        assert_eq!(
            (cut.max_lines(), Bands { first: 0, unit: 2, block: 16, lines: 640 }.band_of(639)),
            (8, (79, 632, 8))
        );
    }

    fn corners(quarters: u8, width: usize, height: usize) -> [usize; 4] {
        const GREYS: [u8; 4] = [40, 100, 160, 220];
        let sequence = stream(4, 4, [0, 0, 0, 32]);
        let picture = stored(
            &sequence,
            |column, line| GREYS[usize::from(column >= 32) + 2 * usize::from(line >= 16)],
            |_, _| GREY,
        );
        let projector = Projector::new(&sequence, turned(quarters), width, height, RGB).unwrap();
        let words = drawn(&projector, &picture);
        let grey = |actual: u32| {
            GREYS
                .iter()
                .position(|&luma| word(&projector.colours, luma, GREY.0, GREY.1, RGB) == actual)
                .expect("a grey of the picture")
        };
        [0, width - 1, (height - 1) * width, height * width - 1].map(|at| grey(words[at]))
    }

    #[test]
    fn quarter_turns() {
        for (width, height) in [(32, 16), (24, 12), (64, 32), (100, 50)] {
            assert_eq!(
                corners(0, width, height),
                [0, 1, 2, 3],
                "top left, top right, bottom left, bottom right at {width}x{height}"
            );
            assert_eq!(corners(1, height, width), [2, 0, 3, 1], "a quarter turn clockwise at {height}x{width}");
            assert_eq!(corners(2, width, height), [3, 2, 1, 0]);
            assert_eq!(corners(3, height, width), [1, 3, 0, 2]);
            assert_eq!(corners(5, height, width), corners(1, height, width), "turns go round");
        }
    }

    #[test]
    fn exact_planes() {
        for (crop, (width, height)) in [
            ([0, 6, 0, 20], (37, 22)),
            ([0, 6, 0, 20], (18, 11)),
            ([2, 0, 6, 2], (39, 28)),
            ([4, 0, 12, 0], (19, 13)),
            ([2, 0, 2, 0], (19, 15)),
        ] {
            let sequence = stream(5, 4, crop);
            let (picture, projector) = (varied(&sequence), upright(&sequence, width, height));
            let [luma, chroma] = planes(&projector, &picture, 1000);
            let mut expected = vec![0u32; width * height];
            projector.colours.row(&luma, Chroma::Beside(&chroma), RGB, &mut expected);
            assert!(
                projector.planes[CHROMA].width == width
                    && projector.is_exact
                    && drawn(&projector, &picture) == expected,
                "{width}x{height} cut {crop:?}"
            );
        }
    }

    fn every_turn(sequence: &Sequence, width: usize, height: usize) -> impl Iterator<Item = (Turn, Projector)> {
        let turns = (0..4).flat_map(|quarters| [false, true].map(|is_mirrored| Turn { quarters, is_mirrored }));
        turns.map(move |turn| {
            (
                turn,
                Projector::new(
                    sequence,
                    turn,
                    if turn.is_sideways() { height } else { width },
                    if turn.is_sideways() { width } else { height },
                    RGB,
                )
                .unwrap(),
            )
        })
    }

    #[test]
    fn turned_pictures() {
        for (crop, sizes) in [
            ([0, 6, 0, 20], [(37, 22), (30, 18), (74, 44), (50, 30)]),
            ([2, 0, 10, 2], [(19, 13), (78, 52), (11, 9), (90, 70)]),
        ] {
            let sequence = stream(5, 4, crop);
            let picture = varied(&sequence);
            for (width, height) in sizes {
                let straight = drawn(&upright(&sequence, width, height), &picture);
                for (turn, projector) in every_turn(&sequence, width, height) {
                    let (mut expected, mirrored): (Vec<u32>, Vec<u32>) = (
                        Vec::new(),
                        straight.chunks_exact(width).flat_map(|line| line.iter().rev()).copied().collect(),
                    );
                    turn_words(&straight, width, turned(turn.quarters), &mut expected);
                    if turn.is_mirrored {
                        // A mirrored turn is the turn the other way of the mirror image.
                        turn_words(&mirrored, width, turned(4 - turn.quarters), &mut expected);
                    }
                    assert!(drawn(&projector, &picture) == expected, "{turn:?} at {width}x{height} cut {crop:?}");
                }
            }
        }
    }

    #[test]
    fn parts_match_whole() {
        let sequence = stream(5, 4, [0, 6, 0, 20]);
        let (picture, other) = (varied(&sequence), stored(&sequence, |_, _| 7, |_, _| (7, 7)));
        let mut scratch = Scratch::default();
        for (horizontal, vertical) in [(37, 22), (30, 18), (74, 44), (50, 30)] {
            for (turn, projector) in every_turn(&sequence, horizontal, vertical) {
                let ((width, _), complete) = (projector.size(), drawn(&projector, &picture));
                let mut parts = vec![0u32; complete.len()];
                for (index, part) in parts.chunks_mut(5 * width).enumerate().rev() {
                    projector.draw(&other, 5 * index..5 * index + part.len() / width, part, &mut scratch);
                    projector.draw(&picture, 5 * index..5 * index + part.len() / width, part, &mut scratch);
                }
                assert!(
                    parts == complete,
                    "{turn:?} at {horizontal}x{vertical}, with a scratch that held another picture's bands"
                );
                assert!(complete.iter().any(|&word| word != complete[0]));
            }
        }
    }

    #[test]
    fn fitted_flats_and_slopes() {
        let sequence = stream(4, 4, [0; 4]);
        let flat = stored(&sequence, |_, _| 180, |_, _| (90, 200));
        let slope = stored(&sequence, |column, _| (4 * column) as u8, |_, _| GREY);
        for (width, height) in [(48, 48), (40, 24), (64, 64), (96, 80), (13, 7)] {
            let projector = upright(&sequence, width, height);
            let expected = word(&projector.colours, 180, 90, 200, RGB);
            assert!(drawn(&projector, &flat).iter().all(|&word| word == expected), "one colour at {width}x{height}");
            let words = drawn(&projector, &slope);
            let greens: Vec<u32> = words[..width].iter().map(|word| word >> 8 & 0xFF).collect();
            assert!(
                greens.windows(2).all(|pair| pair[0] <= pair[1]) && greens[0] < 16 && greens[width - 1] > 240,
                "dark to light at {width}x{height}: {greens:?}"
            );
            assert!(words.chunks(width).all(|line| line == &words[..width]), "every line alike");
        }
    }

    #[test]
    fn picks_either_order() {
        let place = |low: u32, high: u32, fraction: u32| {
            [Place { low, high, fraction }, Place { low: high, high: low, fraction }]
        };
        let (places, blends) = ([place(0, 1, 16), place(1, 2, 32), place(2, 2, 0)], [0u16, 4096, 8192]);
        let to_pixels = |order: (bool, bool), line: &[u16]| {
            let mut out = [0u8; 3];
            pick::<1>(line, &to_picks(&places, LUMA, order, (line.len(), 1)), &mut out);
            out
        };
        assert_eq!(
            to_pixels((false, false), &blends),
            [16, 96, 128],
            "a quarter of the way, half way, and on the last sample"
        );
        assert_eq!(to_pixels((true, false), &blends), [128, 96, 16], "the pixels from the last to the first");
        assert_eq!(
            to_pixels((true, true), &[8192, 4096, 0]),
            [128, 96, 16],
            "the same out of a line that holds its samples from the last to the first"
        );
        assert_eq!(to_pixels((false, true), &[8192, 4096, 0]), [16, 96, 128]);
        assert_eq!(
            to_picks(&places, CHROMA, (false, false), (3, CHROMA_KINDS)),
            [
                Place { low: 2, high: 0, fraction: 16 },
                Place { low: 4, high: 2, fraction: 32 },
                Place { low: 4, high: 4, fraction: 0 }
            ],
            "a chroma sample's place counts its pair's samples"
        );
    }

    #[test]
    fn fitted_samples() {
        let sequence = stream(5, 4, [0, 6, 0, 20]);
        let picture = varied(&sequence);
        for (width, height) in [(30, 18), (50, 30), (74, 40), (100, 60)] {
            let projector = upright(&sequence, width, height);
            let ([luma, chroma], [luma_plane, chroma_plane]) = (planes(&projector, &picture, 1000), projector.planes);
            let to_sample = |plane: &[u8], line_bytes: usize, size: usize, kind: usize, column: Place, row: Place| {
                let at = |line: u32, sample: u32| {
                    u32::from(plane[line as usize * line_bytes + sample as usize * size + kind])
                };
                let lerp = |line: u32| {
                    at(line, column.low) * (PLACE_UNIT - column.fraction) + at(line, column.high) * column.fraction
                };
                ((lerp(row.low) * (PLACE_UNIT - row.fraction)
                    + lerp(row.high) * row.fraction
                    + (1 << (2 * PLACE_BITS - 1)))
                    >> (2 * PLACE_BITS)) as u8
            };
            let expected: Vec<u32> = (0..width * height)
                .map(|at| {
                    let (column, row) = (projector.columns[at % width], projector.rows[at / width]);
                    let colour =
                        |kind: usize| to_sample(&chroma, 2 * chroma_plane.width, 2, kind, column[CHROMA], row[CHROMA]);
                    word(
                        &projector.colours,
                        to_sample(&luma, luma_plane.width, 1, 0, column[LUMA], row[LUMA]),
                        colour(0),
                        colour(1),
                        RGB,
                    )
                })
                .collect();
            assert!(drawn(&projector, &picture) == expected, "at {width}x{height}");
        }
    }

    #[test]
    fn refused_sizes() {
        let sequence = stream(80, 45, [0; 4]);
        let is_made = |sequence: &Sequence, width: usize, height: usize| {
            Projector::new(sequence, turned(0), width, height, RGB).is_some()
        };
        assert!(is_made(&sequence, 480, 270) && is_made(&sequence, 1, 1));
        assert!(!is_made(&sequence, 0, 270) && !is_made(&sequence, 480, 0));
        assert!(!is_made(&stream(200, 45, [0; 4]), 480, 270), "wider than the decoder goes");
        assert!(!is_made(&stream(80, 45, [3, 0, 0, 0]), 480, 270), "cut off between two chroma samples");
        let projector = Projector::new(&sequence, turned(1), 360, 640, RGB).unwrap();
        assert_eq!((projector.size(), projector.columns.len(), projector.rows.len()), ((360, 640), 640, 360));
        let mut none: [u32; 0] = [];
        projector.draw(&vec![0; projector.layout.picture_bytes()], 5..5, &mut none, &mut Scratch::default());
    }
}
