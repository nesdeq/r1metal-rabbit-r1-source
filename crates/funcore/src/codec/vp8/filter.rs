use super::frame::{CHROMA_BLOCK, MACROBLOCK, SUBBLOCK};
use crate::base::crew::{Handoff, Rows};
use crate::codec::edge::{self, Lanes, each, mask, moved, signed, zip};
use crate::codec::picture::Geometry;

// The largest loop filter level, of six bits (RFC 6386, 9.4).
pub(super) const MAX_LEVEL: i32 = 63;
// A macroblock edge's limit is that of a subblock edge with two more levels (RFC 6386, 15.4).
const MACROBLOCK_EXTRA: i32 = 2;
// The sharpness past which the interior limit is shifted by two places, and the ceiling it is held to (RFC 6386, 15.4).
const SHARPER: u8 = 4;
const SHARPNESS_CEILING: i32 = 9;
// The levels from which an edge counts as varying highly: by one, by two, and by one more on an interframe (15.4).
const HEV_LOW: i32 = 15;
const HEV_HIGH: i32 = 40;
const HEV_INTER: i32 = 20;
// The weights of the macroblock filter's three taps on either side, in 128ths, rounded by 63 (RFC 6386, 15.3).
const MB_WEIGHTS: [i16; 3] = [27, 18, 9];
const MB_ROUNDING: i16 = 63;
const MB_SHIFT: u32 = 7;
// The pixels of a segment: four before the edge and four after it.
const SEGMENT: usize = edge::SIDES;
const BEFORE: usize = SEGMENT / 2;
// The kinds of edge a loop below filters, a constant of the loop (RFC 6386, 15.2 and 15.3).
const MACROBLOCK_EDGE: u8 = 0;
const SUBBLOCK_EDGE: u8 = 1;
const SIMPLE_EDGE: u8 = 2;

// How a macroblock's edges are filtered, from its level after the segment's and the deltas' adjustments.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Strength {
    level: u8,
    interior: u8,
    hev: u8,
    // Whether the edges between subblocks are filtered: the macroblock has coefficients or predicts subblocks apart.
    inner: bool,
}

impl Strength {
    // RFC 6386, 15.4 and the reference's calculate_filter_parameters.
    pub(super) fn new(level: i32, sharpness: u8, is_key: bool, inner: bool) -> Strength {
        let level = level.clamp(0, MAX_LEVEL);
        let mut interior = level;
        if sharpness > 0 {
            interior >>= if sharpness > SHARPER { 2 } else { 1 };
            interior = interior.min(SHARPNESS_CEILING - i32::from(sharpness));
        }
        let interior = interior.max(1);
        let hev = i32::from(level >= HEV_LOW) + i32::from(level >= HEV_HIGH) + i32::from(level >= HEV_INTER && !is_key);
        Strength { level: level as u8, interior: interior as u8, hev: hev as u8, inner }
    }

    const fn macroblock_limits(self) -> Limits {
        let edge = (self.level as i32 + MACROBLOCK_EXTRA) * 2 + self.interior as i32;
        Limits { edge, interior: self.interior as i32, hev: self.hev as i32 }
    }

    const fn subblock_limits(self) -> Limits {
        let edge = self.level as i32 * 2 + self.interior as i32;
        Limits { edge, interior: self.interior as i32, hev: self.hev as i32 }
    }
}

// The limits of an edge: of the four pixels across it, of the differences on either side, and of high variance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Limits {
    edge: i32,
    interior: i32,
    hev: i32,
}

// The filter type of the frame header (RFC 6386, 9.4).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    #[default]
    Normal,
    Simple,
}

// Filters N segments across an edge of the kind `EDGE` at once, each a lane's eight pixels from p3 to q3 (RFC 6386, 15.2 and 15.3).
#[inline(always)]
fn filter_lanes<const EDGE: u8, const N: usize>(lanes: &mut [Lanes<N>; SEGMENT], limits: Limits) {
    let [_, p2, p1, p0, q0, q1, q2, _] = *lanes;
    // A limit is at most (MAX_LEVEL + MACROBLOCK_EXTRA) * 2 + MAX_LEVEL, which 16 bits hold.
    let [edge, interior, hev] = [limits.edge, limits.interior, limits.hev].map(|limit| limit as i16);
    let (is_edge, is_filtered, is_high) = edge::masks(lanes, (edge, interior, hev));
    let all = [mask(true); N];
    match EDGE {
        SIMPLE_EDGE => {
            let [_, p0, q0, _] = edge::narrow(lanes, &is_edge, &all);
            (lanes[3], lanes[4]) = (p0, q0);
        }
        MACROBLOCK_EDGE => {
            let high = zip(&is_filtered, &is_high, |filtered, high| filtered & high);
            let low = zip(&is_filtered, &is_high, |filtered, high| filtered & !high);
            let [_, narrow_p0, narrow_q0, _] = edge::narrow(lanes, &high, &all);
            let w = zip(&zip(&p1, &q1, |p, q| signed(p - q)), &zip(&q0, &p0, |q, p| 3 * (q - p)), |outer, step| {
                signed(outer + step)
            });
            let [near, middle, far] = MB_WEIGHTS.map(|weight| each(&w, |w| (weight * w + MB_ROUNDING) >> MB_SHIFT));
            let [fewer_near, fewer_middle, fewer_far] = [&near, &middle, &far].map(|by| each(by, |by| -by));
            lanes[1] = moved(&p2, &far, &low);
            lanes[2] = moved(&p1, &middle, &low);
            lanes[3] = moved(&narrow_p0, &near, &low);
            lanes[4] = moved(&narrow_q0, &fewer_near, &low);
            lanes[5] = moved(&q1, &fewer_middle, &low);
            lanes[6] = moved(&q2, &fewer_far, &low);
        }
        _ => {
            let [p1, p0, q0, q1] = edge::narrow(lanes, &is_filtered, &is_high);
            (lanes[2], lanes[3], lanes[4], lanes[5]) = (p1, p0, q0, q1);
        }
    }
}

// Filters one segment across an edge of the kind `EDGE`, in place.
#[cfg(test)]
fn filter_segment<const EDGE: u8>(segment: &mut [u8; SEGMENT], limits: Limits) {
    let mut lanes = segment.map(|pixel| [i16::from(pixel)]);
    filter_lanes::<EDGE, 1>(&mut lanes, limits);
    *segment = lanes.map(|[pixel]| pixel.clamp(0, 255) as u8);
}

// The segments a group takes at once: a vector's lanes, as the compiler fills them.
const LANES: usize = 8;

// Filters `length` segments of `region` group by group: segment `lane`'s pixel `index` lies at `index * stride + lane` down columns, at `lane * stride + index` along lines.
#[inline(always)]
fn filter_groups<const EDGE: u8>(region: &mut [u8], length: usize, stride: usize, is_down: bool, limits: Limits) {
    let whole = length - length % LANES;
    for first in (0..whole).step_by(LANES) {
        filter_group::<EDGE, LANES>(region, first, (stride, is_down), limits);
    }
    for first in whole..length {
        filter_group::<EDGE, 1>(region, first, (stride, is_down), limits);
    }
}

#[inline(always)]
fn filter_group<const EDGE: u8, const N: usize>(
    region: &mut [u8],
    first: usize,
    (stride, is_down): (usize, bool),
    limits: Limits,
) {
    let start = if is_down { first } else { first * stride };
    let mut lanes: [Lanes<N>; SEGMENT] = edge::gather(region, start, (stride, is_down));
    filter_lanes::<EDGE, N>(&mut lanes, limits);
    // The outer pixel on either side is never moved.
    edge::scatter(region, start, (stride, is_down), (&lanes, 1..SEGMENT - 1));
}

// Filters each of `length` segments across the edge left of column `x`, lines `y` on: each lies along a line.
fn across<const EDGE: u8>(
    pixels: &mut [u8],
    geometry: Geometry,
    (x, y): (usize, usize),
    length: usize,
    limits: Limits,
) {
    let start = geometry.at(x - BEFORE, y);
    let Some(region) = pixels.get_mut(start..start + (length - 1) * geometry.stride + SEGMENT) else {
        return;
    };
    filter_groups::<EDGE>(region, length, geometry.stride, false, limits);
}

// Filters each of `length` segments across the edge above line `y`, columns `x` on: each lies down a column.
fn down<const EDGE: u8>(pixels: &mut [u8], geometry: Geometry, (x, y): (usize, usize), length: usize, limits: Limits) {
    let start = geometry.at(x, y - BEFORE);
    let Some(region) = pixels.get_mut(start..start + (SEGMENT - 1) * geometry.stride + length) else {
        return;
    };
    filter_groups::<EDGE>(region, length, geometry.stride, true, limits);
}

// Where a macroblock's top edge is filtered: in its plane, or beside the lines above handed over by the row above.
trait Top {
    // Filters the edge of the kind `EDGE` above the block of `size` at `x`, `y` of the plane.
    fn filter<const EDGE: u8>(
        &mut self,
        plane: &mut (&mut [u8], Geometry),
        place: (usize, usize),
        size: usize,
        limits: Limits,
    );
}

// The top edge in the plane, where the row has one above it.
struct InPlace(bool);

impl Top for InPlace {
    fn filter<const EDGE: u8>(
        &mut self,
        (pixels, geometry): &mut (&mut [u8], Geometry),
        place: (usize, usize),
        size: usize,
        limits: Limits,
    ) {
        if self.0 {
            down::<EDGE>(pixels, *geometry, place, size, limits);
        }
    }
}

// The top edge of a luma row filtered at the same time as the row above: the row above hands over its last BEFORE lines a macroblock's columns at a time, from `above` on in `lines`, and takes back those the edge changed; nothing above the first row.
struct Handed<'a> {
    lines: &'a Handoff,
    above: Option<usize>,
    width: usize,
    border: usize,
}

// The lines of a scratch that holds a macroblock's top edge: BEFORE above it and as many of its own.
const SCRATCH: Geometry = Geometry { width: MACROBLOCK, height: SEGMENT, border: 0, stride: MACROBLOCK };

impl Top for Handed<'_> {
    fn filter<const EDGE: u8>(
        &mut self,
        (pixels, geometry): &mut (&mut [u8], Geometry),
        (x, y): (usize, usize),
        size: usize,
        limits: Limits,
    ) {
        let Some(above) = self.above else {
            return;
        };
        let left = x - self.border;
        let mut scratch = [0u8; SEGMENT * MACROBLOCK];
        let (top, own) = scratch.split_at_mut(BEFORE * MACROBLOCK);
        for (line, out) in top.as_chunks_mut::<MACROBLOCK>().0.iter_mut().enumerate() {
            self.lines.take(above + line * self.width + left, &mut out[..size]);
        }
        for (line, out) in own.as_chunks_mut::<MACROBLOCK>().0.iter_mut().enumerate() {
            let at = geometry.at(x, y + line);
            out[..size].copy_from_slice(&pixels[at..at + size]);
        }
        down::<EDGE>(&mut scratch, SCRATCH, (0, BEFORE), size, limits);
        let (top, own) = scratch.split_at(BEFORE * MACROBLOCK);
        for (line, changed) in own.as_chunks::<MACROBLOCK>().0.iter().enumerate() {
            let at = geometry.at(x, y + line);
            pixels[at..at + size].copy_from_slice(&changed[..size]);
        }
        for (line, changed) in top.as_chunks::<MACROBLOCK>().0.iter().enumerate().skip(1) {
            self.lines.put(above + line * self.width + left, &changed[..size]);
        }
    }
}

// The edges of one plane of a macroblock in their order: left, between subblocks down, top by `top`, between subblocks across.
fn filter_block<const OUTER: u8, const INNER: u8>(
    plane: &mut (&mut [u8], Geometry),
    (x, y): (usize, usize),
    size: usize,
    (has_left, has_inner): (bool, bool),
    (outer, inner): (Limits, Limits),
    top: &mut impl Top,
) {
    let geometry = plane.1;
    if has_left {
        across::<OUTER>(plane.0, geometry, (x, y), size, outer);
    }
    if has_inner {
        for offset in (SUBBLOCK..size).step_by(SUBBLOCK) {
            across::<INNER>(plane.0, geometry, (x + offset, y), size, inner);
        }
    }
    top.filter::<OUTER>(plane, (x, y), size, outer);
    if has_inner {
        for offset in (SUBBLOCK..size).step_by(SUBBLOCK) {
            down::<INNER>(plane.0, geometry, (x, y + offset), size, inner);
        }
    }
}

// Filters the edges of a macroblock of `strength` in one plane, the block of `size` at `place`, its top edge by `top`.
fn filter_macroblock(
    plane: &mut (&mut [u8], Geometry),
    place: (usize, usize),
    size: usize,
    (strength, kind, has_left): (Strength, Kind, bool),
    top: &mut impl Top,
) {
    let edges = (has_left, strength.inner);
    let limits = (strength.macroblock_limits(), strength.subblock_limits());
    match kind {
        Kind::Normal => filter_block::<MACROBLOCK_EDGE, SUBBLOCK_EDGE>(plane, place, size, edges, limits, top),
        Kind::Simple => filter_block::<SIMPLE_EDGE, SIMPLE_EDGE>(plane, place, size, edges, limits, top),
    }
}

// Filters a row's edges (RFC 6386, 15.1); it touches no line below the row, so the row under it may be decoded first.
pub(super) fn filter_row(planes: [(&mut [u8], Geometry); 3], row: usize, strengths: &[Strength], kind: Kind) {
    for (index, plane) in planes.into_iter().enumerate() {
        filter_plane_row(plane, index > 0, row, strengths, kind);
    }
}

// Filters one plane's edges of a row; the edges of one plane touch no pixel of another.
pub(super) fn filter_plane_row(
    mut plane: (&mut [u8], Geometry),
    is_chroma: bool,
    row: usize,
    strengths: &[Strength],
    kind: Kind,
) {
    // The simple filter leaves chroma as it is (RFC 6386, 15.2).
    if is_chroma && kind == Kind::Simple {
        return;
    }
    let size = if is_chroma { CHROMA_BLOCK } else { MACROBLOCK };
    for (column, &strength) in strengths.iter().enumerate() {
        if strength.level > 0 {
            let place = (column * size, row * size);
            filter_macroblock(&mut plane, place, size, (strength, kind, column > 0), &mut InPlace(row > 0));
        }
    }
}

// A luma row filters a macroblock once the row above has handed over its lines there: the row above's progress counts the macroblocks it has handed over.
pub(super) const LUMA_LEAD: usize = 1;

// Where a luma row's last BEFORE lines lie among the lines the rows hand over, each of the visible width.
const fn handed_at(row: usize, width: usize) -> usize {
    row * BEFORE * width
}

// The bytes the rows of a luma plane of `rows` rows `width` across hand over as they are filtered at once.
pub(super) const fn handed_bytes(rows: usize, width: usize) -> usize {
    handed_at(rows, width)
}

// Filters luma row `row` of a plane whose lines from the row's first `pixels` holds, at the same time as the rows above and below it: its top edges beside the last lines of the row above, which `lines` hands over as that row notes them to `rows` and takes back changed, and its own last lines handed to the row below a macroblock's columns at a time, those of each macroblock once the one after it is filtered, which reaches back into them.
pub(super) fn filter_luma_row(
    (pixels, geometry): (&mut [u8], Geometry),
    row: usize,
    (strengths, kind): (&[Strength], Kind),
    (lines, rows): (&Handoff, &dyn Rows),
) {
    let (width, border) = (geometry.width, geometry.border);
    let lines_of_row = Geometry { width, height: MACROBLOCK, border: 0, stride: geometry.stride };
    let mut top = Handed { lines, above: row.checked_sub(1).map(|above| handed_at(above, width)), width, border };
    let mut plane = (pixels, lines_of_row);
    let hand = |pixels: &[u8], column: usize| {
        for line in 0..BEFORE {
            let at = lines_of_row.at(border + column * MACROBLOCK, MACROBLOCK - BEFORE + line);
            lines.put(handed_at(row, width) + line * width + column * MACROBLOCK, &pixels[at..at + MACROBLOCK]);
        }
    };
    for (column, &strength) in strengths.iter().enumerate() {
        rows.wait(row, column);
        if strength.level > 0 {
            let place = (border + column * MACROBLOCK, 0);
            filter_macroblock(&mut plane, place, MACROBLOCK, (strength, kind, column > 0), &mut top);
        }
        if let Some(before) = column.checked_sub(1) {
            hand(&*plane.0, before);
            rows.note(row, column);
        }
    }
    if let Some(last) = strengths.len().checked_sub(1) {
        hand(&*plane.0, last);
    }
    rows.note(row, strengths.len());
}

// Puts back in a luma plane of `rows` rows the lines above each row but the first that its top edges changed, as the rows handed them in `lines`.
pub(super) fn put_back(plane: &mut [u8], geometry: Geometry, rows: usize, lines: &Handoff) {
    let width = geometry.width;
    for row in 1..rows {
        // The macroblock edge changes the three lines next to it, the fourth only read (RFC 6386, 15.3).
        for line in 1..BEFORE {
            let at = geometry.at(0, row * MACROBLOCK - BEFORE + line);
            lines.take(handed_at(row - 1, width) + line * width, &mut plane[at..at + width]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn thresholds_by_level_and_sharpness() {
        let plain = Strength::new(32, 0, true, true);
        assert_eq!((plain.interior, plain.hev), (32, 1), "no sharpness: the level itself; 15 to 39 on a key frame: 1");
        assert_eq!(
            (plain.macroblock_limits().edge, plain.subblock_limits().edge),
            (100, 96),
            "(32 + 2) * 2 + 32 and 32 * 2 + 32"
        );
        assert_eq!(Strength::new(32, 3, true, true).interior, 6, "32 >> 1 is held to 9 - 3");
        assert_eq!(Strength::new(32, 5, true, true).interior, 4, "a sharpness past 4 shifts by two: 8, held to 9 - 5");
        assert_eq!(Strength::new(1, 7, true, true).interior, 1, "never below one");
        let hev = |level, is_key| Strength::new(level, 0, is_key, false).hev;
        assert_eq!([hev(14, true), hev(15, true), hev(39, true), hev(40, true)], [0, 1, 1, 2], "a key frame's steps");
        assert_eq!([hev(14, false), hev(19, false), hev(20, false), hev(40, false)], [0, 1, 2, 3], "an interframe's");
        assert_eq!(Strength::new(70, 0, true, false).level, 63, "levels are held to six bits");
        assert_eq!(Strength::new(-4, 0, true, false).level, 0, "and never below nought");
    }

    #[test]
    fn segment_filters() {
        let strength = Strength::new(20, 0, true, true);
        let mut step = [60, 60, 60, 60, 70, 70, 70, 70];
        filter_segment::<MACROBLOCK_EDGE>(&mut step, strength.macroblock_limits());
        // w = (60 - 70) + 3 * 10 = 20 moves them by (27 * 20 + 63) >> 7, (18 * 20 + 63) >> 7, (9 * 20 + 63) >> 7.
        assert_eq!(step, [60, 61, 63, 64, 66, 67, 69, 70], "a low step is smoothed over six pixels: by 4, 3 and 1");
        let mut subblock = [60, 60, 60, 60, 70, 70, 70, 70];
        filter_segment::<SUBBLOCK_EDGE>(&mut subblock, strength.subblock_limits());
        // a = 30: f1 = 34 >> 3 = 4 and f2 = 33 >> 3 = 4; the inner pixels move by (4 + 1) >> 1 = 2.
        assert_eq!(subblock, [60, 60, 62, 64, 66, 68, 70, 70], "a subblock edge moves two pixels on either side");
        let mut cliff = [0, 0, 0, 0, 200, 200, 200, 200];
        filter_segment::<MACROBLOCK_EDGE>(&mut cliff, strength.macroblock_limits());
        assert_eq!(cliff, [0, 0, 0, 0, 200, 200, 200, 200], "an edge past the limit is a picture's own and left alone");
        let mut varied = [60, 60, 40, 60, 70, 90, 70, 70];
        filter_segment::<MACROBLOCK_EDGE>(&mut varied, Strength::new(40, 0, true, true).macroblock_limits());
        // High variance: a = clamp(40 - 90 + 3 * 10) = -20, f1 = -16 >> 3 = -2, f2 = -17 >> 3 = -3.
        assert_eq!(
            varied,
            [60, 60, 40, 57, 72, 90, 70, 70],
            "high variance moves the edge pixels alone, outer taps used"
        );
        let mut rough = [60, 90, 60, 60, 70, 70, 70, 70];
        filter_segment::<MACROBLOCK_EDGE>(&mut rough, strength.macroblock_limits());
        assert_eq!(rough, [60, 90, 60, 60, 70, 70, 70, 70], "an interior difference past the interior limit");
        filter_segment::<SIMPLE_EDGE>(&mut rough, strength.macroblock_limits());
        assert_eq!(rough[3..5], [62, 67], "which the simple filter does not ask: a = 3 * 10 - 10, by 2 and 3");
    }

    #[test]
    fn groups_filter_as_one() {
        let mut state = 0x7F4A_7C15u32;
        let mut next = |bound: u32| {
            state = crate::base::math::xorshift32(state);
            state % bound
        };
        // Segments of a group along lines and down columns of a block of their own size.
        let stride = SEGMENT;
        let mut moved = [0; 3];
        for round in 0..3000 {
            let (kind, is_down) = (round / 2 % 3, round % 2 == 0);
            let mut region = [0u8; SEGMENT * LANES];
            // Each segment flat, a step, a ramp or noise, so that every branch of the filters is taken.
            for lane in 0..LANES {
                let (shape, base, step) = (next(4), 40 + next(160) as i32, next(24) as i32);
                for index in 0..SEGMENT {
                    let value = match shape {
                        0 => base + next(2) as i32,
                        1 => base + if index < BEFORE { 0 } else { step } + next(3) as i32,
                        2 => base + index as i32 * step / 4,
                        _ => next(256) as i32,
                    };
                    let at = if is_down { index * stride + lane } else { lane * stride + index };
                    region[at] = value.clamp(0, 255) as u8;
                }
            }
            let strength = Strength::new(1 + next(63) as i32, next(8) as u8, next(2) == 0, true);
            let (mut together, mut apart) = (region, region);
            let steps = (stride, is_down);
            let limits = [strength.macroblock_limits(), strength.subblock_limits(), strength.macroblock_limits()][kind];
            type Group = fn(&mut [u8], usize, (usize, bool), Limits);
            let (group, one): (Group, Group) = match kind {
                0 => (filter_group::<MACROBLOCK_EDGE, LANES>, filter_group::<MACROBLOCK_EDGE, 1>),
                1 => (filter_group::<SUBBLOCK_EDGE, LANES>, filter_group::<SUBBLOCK_EDGE, 1>),
                _ => (filter_group::<SIMPLE_EDGE, LANES>, filter_group::<SIMPLE_EDGE, 1>),
            };
            group(&mut together, 0, steps, limits);
            for lane in 0..LANES {
                one(&mut apart, lane, steps, limits);
            }
            assert_eq!(together, apart, "round {round}: edge kind {kind}, down {is_down}, {strength:?}");
            moved[kind] += usize::from(together != region);
        }
        assert!(moved.iter().all(|&count| count > 100), "every kind of edge moved pixels: {moved:?}");
    }

    #[test]
    fn simple_row_luma_alone() {
        let (width, stride, lines) = (32, 40, 16);
        let mut luma = vec![0u8; stride * lines];
        let mut chroma = vec![0u8; 2 * 16 * 8];
        for line in 0..lines {
            for x in 0..width {
                luma[line * stride + x] = if x < 16 { 100 } else { 104 };
            }
        }
        chroma.iter_mut().enumerate().for_each(|(at, pixel)| *pixel = if at % 16 < 8 { 10 } else { 20 });
        let before = chroma.clone();
        let strengths = [Strength::new(10, 0, true, false); 2];
        let (u, v) = chroma.split_at_mut(16 * 8);
        let chroma_geometry = Geometry { width: 16, height: 8, border: 0, stride: 16 };
        let planes = [
            (&mut luma[..], Geometry { width, height: lines, border: 0, stride }),
            (u, chroma_geometry),
            (v, chroma_geometry),
        ];
        filter_row(planes, 0, &strengths, Kind::Simple);
        // a = clamp(100 - 104 + 3 * 4) = 8: f1 = 12 >> 3 = 1 and f2 = 11 >> 3 = 1.
        assert_eq!(luma[14..18], [100, 101, 103, 104], "the edge between the two macroblocks");
        assert_eq!(chroma, before, "the simple filter leaves chroma alone");
    }

    #[test]
    fn edges_along_and_down() {
        let geometry = Geometry::new(16, 16, 4);
        let mut plane = vec![0u8; geometry.bytes()];
        for y in 0..16 {
            for x in 0..16 {
                plane[geometry.at(x, y)] = if x < 8 { 60 } else { 70 };
            }
        }
        let limits = Strength::new(20, 0, true, true).subblock_limits();
        across::<SUBBLOCK_EDGE>(&mut plane, geometry, (8, 0), 16, limits);
        let row: alloc::vec::Vec<u8> = (0..16).map(|x| plane[geometry.at(x, 15)]).collect();
        assert_eq!(row[4..12], [60, 60, 62, 64, 66, 68, 70, 70], "the last line of a vertical edge, as a segment does");
        let mut turned = vec![0u8; geometry.bytes()];
        for y in 0..16 {
            for x in 0..16 {
                turned[geometry.at(x, y)] = plane[geometry.at(y, x)];
            }
        }
        let mut flat = vec![0u8; geometry.bytes()];
        for y in 0..16 {
            for x in 0..16 {
                flat[geometry.at(x, y)] = if y < 8 { 60 } else { 70 };
            }
        }
        down::<SUBBLOCK_EDGE>(&mut flat, geometry, (0, 8), 16, limits);
        assert_eq!(flat, turned, "an edge across the lines filters each column as an edge down them filters each line");
    }
}
