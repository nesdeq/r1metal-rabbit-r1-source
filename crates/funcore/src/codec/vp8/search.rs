use super::cost::weigh;
use super::frame::MACROBLOCK;
use super::inter::{AFTER, BEFORE, HALF_EIGHTHS};
use super::modes::{Mv, MvCosts, mv_cost};
use super::tables::SIXTAP_FILTERS;
use crate::base::simd;
use crate::codec::picture::Geometry;
use alloc::vec;
use core::ops::Range;

// A vector counts quarters of a pixel (RFC 6386, 5), the filters eighths.
const QUARTERS: i32 = 4;
const HALF_QUARTERS: i32 = QUARTERS / 2;
const TAPS: usize = BEFORE + 1 + AFTER;
// The largest component a vector may have, so that it less any neighbour's codes in ten bits (RFC 6386, 17.1).
const MOST_COMPONENT: i32 = 511;
// The diamond's steps from the best start before the search stops: 64 coded the test scenes alike, 4 cost the moving test pattern 9 to 20 % more bytes (measured).
const MOST_STEPS: usize = 16;
// One full pixel each way, then the eight neighbours at half and at a quarter of a pixel.
const DIAMOND: [(i32, i32); 4] = [(0, -QUARTERS), (-QUARTERS, 0), (QUARTERS, 0), (0, QUARTERS)];
const RING: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

// Fills the tile rows `tiles` of the half-pel planes of a reference's bordered luma: each sample the decoder's six taps half a pixel across, down, or both, the reference's edges repeated past its border as its prediction of a block repeats them; the sample between pixels x and x + 1 stands at x. Both is filtered down from the samples across, as the decoder's second pass is from its first.
pub(super) fn interpolate(
    (reference, geometry): (&[u8], Geometry),
    tiles: Range<usize>,
    [across, down, both]: [&mut [u8]; 3],
) {
    let (stride, lines) = (geometry.stride, geometry.height + 2 * geometry.border);
    let rows = tiles.start * MACROBLOCK..(tiles.end * MACROBLOCK).min(lines);
    let taps = &SIXTAP_FILTERS[HALF_EIGHTHS];
    // The lines across from BEFORE above the rows to AFTER below them, the plane's first and last lines' past its edges.
    let mut middle = vec![0u8; (rows.len() + BEFORE + AFTER) * stride];
    for (index, out) in middle.chunks_exact_mut(stride).enumerate() {
        let line = (rows.start + index).saturating_sub(BEFORE).min(lines - 1);
        filter_across(&reference[line * stride..][..stride], taps, out);
    }
    across.copy_from_slice(&middle[BEFORE * stride..][..rows.len() * stride]);
    filter_down((reference, stride, lines), rows.clone(), taps, down);
    simd::filter((&middle, stride), stride, taps, (both, stride), (stride, rows.len()));
}

// A line filtered across by `taps` into `out`, its first and last pixels repeated past its ends.
fn filter_across(line: &[u8], taps: &[i32; TAPS], out: &mut [u8]) {
    let width = line.len();
    simd::filter((line, width), 1, taps, (&mut out[BEFORE..], width), (width - BEFORE - AFTER, 1));
    for x in (0..BEFORE).chain(width - AFTER..width) {
        let samples: [u8; TAPS] = core::array::from_fn(|tap| line[(x + tap).saturating_sub(BEFORE).min(width - 1)]);
        out[x] = simd::plain::tap(&samples, 1, taps);
    }
}

// The lines `rows` of a plane of `lines` lines filtered down by `taps` into `out`, its first and last lines repeated past its edges.
fn filter_down((plane, stride, lines): (&[u8], usize, usize), rows: Range<usize>, taps: &[i32; TAPS], out: &mut [u8]) {
    let inner = rows.start.max(BEFORE)..rows.end.min(lines - AFTER);
    if !inner.is_empty() {
        let samples = (&plane[(inner.start - BEFORE) * stride..], stride);
        let target = (&mut out[(inner.start - rows.start) * stride..], stride);
        simd::filter(samples, stride, taps, target, (stride, inner.len()));
    }
    let mut repeated = vec![0u8; TAPS * stride];
    for row in rows.clone().filter(|row| !inner.contains(row)) {
        for (tap, line) in repeated.chunks_exact_mut(stride).enumerate() {
            let source = (row + tap).saturating_sub(BEFORE).min(lines - 1);
            line.copy_from_slice(&plane[source * stride..][..stride]);
        }
        let target = (&mut out[(row - rows.start) * stride..], stride);
        simd::filter((&repeated, stride), stride, taps, target, (stride, 1));
    }
}

// A block of the source to find in the reference: its top left in luma pixels, the vectors it may take, and the vector its own is coded against.
#[derive(Clone, Copy)]
pub(super) struct Block {
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) least: Mv,
    pub(super) most: Mv,
    pub(super) predictor: Mv,
}

impl Block {
    // The vectors the macroblock at `row`, `column` may take: one macroblock past the frame's edges, as the decoder clamps those it predicts (RFC 6386, 16.3), and no component past MOST_COMPONENT.
    pub(super) fn new((row, column): (usize, usize), (rows, columns): (usize, usize), predictor: Mv) -> Block {
        let quarters = |count: usize| count as i32 * MACROBLOCK as i32 * QUARTERS;
        let limit = |low: i32, high: i32| (low.max(-MOST_COMPONENT) as i16, high.min(MOST_COMPONENT) as i16);
        let (left, right) = limit(-quarters(column + 1), quarters(columns - column));
        let (top, bottom) = limit(-quarters(row + 1), quarters(rows - row));
        let (x, y) = (column * MACROBLOCK, row * MACROBLOCK);
        Block { x, y, least: Mv { x: left, y: top }, most: Mv { x: right, y: bottom }, predictor }
    }

    pub(super) fn clamp(&self, mv: Mv) -> Mv {
        Mv { x: mv.x.clamp(self.least.x, self.most.x), y: mv.y.clamp(self.least.y, self.most.y) }
    }

    const fn holds(&self, mv: Mv) -> bool {
        mv.x >= self.least.x && mv.x <= self.most.x && mv.y >= self.least.y && mv.y <= self.most.y
    }
}

// A search of the source's luma in a reference's luma and its half-pel planes, all of one geometry.
pub(super) struct Search<'a> {
    pub(super) source: &'a [u8],
    pub(super) luma: &'a [u8],
    pub(super) halves: [&'a [u8]; 3],
    pub(super) geometry: Geometry,
    // What a bit of a vector is worth in absolute differences, in 256ths.
    pub(super) lambda: u32,
    pub(super) costs: &'a MvCosts,
}

impl Search<'_> {
    // The plane and place of the sample at a position in quarters on the half-pel grid.
    fn sample(&self, (x, y): (i32, i32)) -> (&[u8], usize) {
        let plane = match (x & HALF_QUARTERS != 0, y & HALF_QUARTERS != 0) {
            (false, false) => self.luma,
            (true, false) => self.halves[0],
            (false, true) => self.halves[1],
            (true, true) => self.halves[2],
        };
        let border = self.geometry.border as i32;
        let at = (y.div_euclid(QUARTERS) + border) * self.geometry.stride as i32 + x.div_euclid(QUARTERS) + border;
        (plane, at as usize)
    }

    // The absolute differences of the block of `SIZE` at `block` moved by `mv`, summed no further than `limit`: exact on the half-pel grid, and at a quarter the mean of the two half-pel samples on either side.
    pub(super) fn distortion<const SIZE: usize>(&self, block: &Block, mv: Mv, limit: u32) -> u32 {
        let stride = self.geometry.stride;
        let source = (&self.source[self.geometry.at(block.x, block.y)..], stride);
        let (x, y) = (block.x as i32 * QUARTERS + i32::from(mv.x), block.y as i32 * QUARTERS + i32::from(mv.y));
        let (step_x, step_y) = (x & 1, y & 1);
        if (step_x, step_y) == (0, 0) {
            let (plane, at) = self.sample((x, y));
            return simd::sad(source, (&plane[at..], stride), (SIZE, SIZE), limit);
        }
        let ((before, before_at), (after, after_at)) =
            (self.sample((x - step_x, y - step_y)), self.sample((x + step_x, y + step_y)));
        simd::sad_mean(source, (&before[before_at..], stride), (&after[after_at..], stride), (SIZE, SIZE), limit)
    }

    // What a vector costs, its differences and its bits against the block's predictor, where that is less than `best`.
    fn cost<const SIZE: usize>(&self, block: &Block, mv: Mv, best: u32) -> Option<u32> {
        let bits = weigh(self.lambda, mv_cost(self.costs, mv.minus(block.predictor)));
        let limit = best.checked_sub(bits)?;
        let cost = self.distortion::<SIZE>(block, mv, limit) + bits;
        (cost < best).then_some(cost)
    }

    // The best whole-pixel vector from the cheapest of `starts`, by a diamond of one pixel, and its cost.
    pub(super) fn full<const SIZE: usize>(&self, block: &Block, starts: &[Mv]) -> (Mv, u32) {
        let whole = |mv: Mv| {
            let round = |value: i16| (i32::from(value) + HALF_QUARTERS).div_euclid(QUARTERS) * QUARTERS;
            block.clamp(Mv { x: round(mv.x) as i16, y: round(mv.y) as i16 })
        };
        let (mut best, mut cost) = (whole(Mv::default()), u32::MAX);
        for &start in starts {
            let start = whole(start);
            if let Some(offer) = self.cost::<SIZE>(block, start, cost) {
                (best, cost) = (start, offer);
            }
        }
        for _ in 0..MOST_STEPS {
            let center = best;
            for (dx, dy) in DIAMOND {
                let mv = Mv { x: center.x + dx as i16, y: center.y + dy as i16 };
                if !block.holds(mv) {
                    continue;
                }
                if let Some(offer) = self.cost::<SIZE>(block, mv, cost) {
                    (best, cost) = (mv, offer);
                }
            }
            if best == center {
                break;
            }
        }
        (best, cost)
    }

    // The best of the eight neighbours at half a pixel, then at a quarter, of a vector and its cost.
    pub(super) fn refine<const SIZE: usize>(&self, block: &Block, (mut best, mut cost): (Mv, u32)) -> (Mv, u32) {
        for step in [HALF_QUARTERS, 1] {
            let center = best;
            for (dx, dy) in RING {
                let mv = Mv { x: center.x + (dx * step) as i16, y: center.y + (dy * step) as i16 };
                if !block.holds(mv) {
                    continue;
                }
                if let Some(offer) = self.cost::<SIZE>(block, mv, cost) {
                    (best, cost) = (mv, offer);
                }
            }
        }
        (best, cost)
    }
}

#[cfg(test)]
mod tests {
    use super::super::inter::{self, Filter};
    use super::super::modes::mv_costs;
    use super::super::tables::DEFAULT_MV_CONTEXT;
    use super::*;
    use crate::base::math::xorshift32;
    use crate::codec::picture::extend_plane;
    use alloc::vec::Vec;

    // The half-pel planes' vectors in eighths: across, down, and both.
    const HALF_VECTORS: [(i32, i32); 3] =
        [(HALF_EIGHTHS as i32, 0), (0, HALF_EIGHTHS as i32), (HALF_EIGHTHS as i32, HALF_EIGHTHS as i32)];

    // A textured plane of `columns` by `rows` macroblocks, its border filled as a reference frame's is.
    fn texture((columns, rows): (usize, usize), seed: u32) -> (Vec<u8>, Geometry) {
        let geometry = Geometry::new(columns * MACROBLOCK, rows * MACROBLOCK, 2 * MACROBLOCK);
        let mut plane = vec![0; geometry.bytes()];
        let mut state = seed;
        for y in 0..geometry.height {
            for x in 0..geometry.width {
                state = xorshift32(state);
                plane[geometry.at(x, y)] = ((x * 7 + y * 3) % 160 + state as usize % 64) as u8;
            }
        }
        extend_plane(&mut plane, geometry);
        (plane, geometry)
    }

    // The half-pel planes of a plane, filled a job of one tile row at a time, as the encoder's jobs fill them apart.
    fn halves(plane: &[u8], geometry: Geometry) -> [Vec<u8>; 3] {
        let mut halves = [vec![0; geometry.bytes()], vec![0; geometry.bytes()], vec![0; geometry.bytes()]];
        let job = MACROBLOCK * geometry.stride;
        let [across, down, both] = &mut halves;
        let jobs = across.chunks_mut(job).zip(down.chunks_mut(job)).zip(both.chunks_mut(job));
        for (tile, ((across, down), both)) in jobs.enumerate() {
            interpolate((plane, geometry), tile..tile + 1, [across, down, both]);
        }
        halves
    }

    #[test]
    fn half_planes_are_predictions() {
        let (plane, geometry) = texture((3, 2), 7);
        let halves = halves(&plane, geometry);
        let border = geometry.border as i32;
        let (stride, lines) = (geometry.stride as i32, (geometry.height + 2 * geometry.border) as i32);
        // Every block of 4x4 of each plane, its border's too, against the decoder's prediction of it.
        let places = (-border..lines - border)
            .step_by(4)
            .flat_map(|y| (-border..stride - border).step_by(4).map(move |x| (x, y)));
        for (half, vector) in halves.iter().zip(HALF_VECTORS) {
            for (x, y) in places.clone() {
                let mut predicted = [0u8; 16];
                inter::predict((&plane, geometry), (x, y), vector, (4, 4), Filter::Sixtap, &mut predicted, 0, 4);
                let at = ((y + border) * stride + x + border) as usize;
                let lines: Vec<&[u8]> = (0..4).map(|line| &half[at + line * geometry.stride..][..4]).collect();
                let expected: Vec<&[u8]> = predicted.chunks(4).collect();
                assert_eq!(lines, expected, "the half-pel sample {vector:?} at {x}, {y} is the decoder's prediction");
            }
        }
    }

    #[test]
    fn search_finds_shift() {
        let (reference, geometry) = texture((4, 4), 11);
        let halves = halves(&reference, geometry);
        let costs = mv_costs(&DEFAULT_MV_CONTEXT);
        for (shift, slack) in [(Mv { x: 14, y: -10 }, 0), (Mv { x: -7, y: 5 }, 1)] {
            // The source is the reference moved by the shift, as the decoder predicts it.
            let mut source = vec![0; geometry.bytes()];
            let (x, y) = (MACROBLOCK, MACROBLOCK);
            let eighths = (2 * i32::from(shift.x), 2 * i32::from(shift.y));
            let at = geometry.at(x, y);
            let size = (MACROBLOCK, MACROBLOCK);
            inter::predict(
                (&reference, geometry),
                (16, 16),
                eighths,
                size,
                Filter::Sixtap,
                &mut source,
                at,
                geometry.stride,
            );
            let planes = [&halves[0][..], &halves[1][..], &halves[2][..]];
            let search =
                Search { source: &source, luma: &reference, halves: planes, geometry, lambda: 0, costs: &costs };
            let block = Block::new((1, 1), (4, 4), Mv::default());
            // Starts at nought and near the shift, as a neighbour's vector would give one.
            let starts = [Mv::default(), Mv { x: shift.x & !3, y: shift.y & !3 }];
            let (found, cost) = search.refine::<MACROBLOCK>(&block, search.full::<MACROBLOCK>(&block, &starts));
            let (dx, dy) = (found.x.abs_diff(shift.x), found.y.abs_diff(shift.y));
            assert!(dx <= slack && dy <= slack, "a shift of {shift:?} found as {found:?} at a cost of {cost}");
            if slack == 0 {
                assert_eq!(cost, 0, "a shift on the half-pel grid is found exactly");
            }
        }
    }

    #[test]
    fn vector_bounds() {
        let corner = Block::new((0, 0), (30, 40), Mv::default());
        assert_eq!(
            (corner.least, corner.most),
            (Mv { x: -64, y: -64 }, Mv { x: 511, y: 511 }),
            "one macroblock past the top left, and no further than ten coded bits reach the other way"
        );
        let far = Block::new((29, 39), (30, 40), Mv::default());
        assert_eq!((far.least, far.most), (Mv { x: -511, y: -511 }, Mv { x: 64, y: 64 }), "the bottom right");
        assert_eq!(far.clamp(Mv { x: 100, y: -600 }), Mv { x: 64, y: -511 }, "a vector clamped into them");
    }
}
