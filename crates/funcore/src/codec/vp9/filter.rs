use super::header::{LoopFilter, Segmentation};
use super::modes::Info;
use super::tables::{
    BLOCK_16X16, INTRA_FRAME, MAX_SEGMENTS, MI_SIZE, NEARESTMV, NEARMV, NEWMV, NUM_8X8_HIGH, NUM_8X8_WIDE,
    SEG_LVL_ALT_L, SUPERBLOCK_MI, SUPERBLOCK_SIDE, TX_4X4, TX_8X8, TX_16X16,
};
use crate::codec::edge::{self, Lanes, SIDES, apart, each, mask, pick, zip};
use crate::codec::picture::Geometry;

// The largest filter level, the level at which the deltas double, and the deltas of each mode type (VP9 3, 8.8.1).
const MAX_LOOP_FILTER: i32 = 63;
const DELTA_DOUBLING_SHIFT: u32 = 5;
const MAX_MODE_LF_DELTAS: usize = 2;
const MAX_REF_FRAMES: usize = 4;
// The sharpness above which the limit shifts twice, and the limit's bound by it (VP9 8.8.4).
const SHARPNESS_DOUBLE: u8 = 4;
const SHARPNESS_LIMIT: i32 = 9;
const HEV_SHIFT: u32 = 4;
// A flat region varies by at most 1 from the samples at the boundary (VP9 8.8.5.1).
const FLAT: i16 = 1;
// The samples the 15-tap filter reads, p7 to q7, and the base 2 logarithms of the 8 and 16 sample filters (VP9 8.8.5.3).
const WIDE: usize = 16;
const LOG2_8: u32 = 3;
const LOG2_16: u32 = 4;
// Edges every 4 samples, runs of 8 samples sharing one block (VP9 8.8.2).
const EDGE_STEP: usize = 4;
const RUN: usize = 8;
// Edges a multiple of 32 samples apart take at least the 8 sample filter (VP9 8.8.3).
const EDGES_PER_32: usize = 8;

// The filter level of each segment, reference and mode type, and the limits of each level (VP9 8.8.1, 8.8.4).
pub(super) struct Levels {
    lookup: [[[u8; MAX_MODE_LF_DELTAS]; MAX_REF_FRAMES]; MAX_SEGMENTS],
    sharpness: u8,
}

impl Levels {
    pub(super) fn of(filter: &LoopFilter, segmentation: &Segmentation) -> Levels {
        let level = i32::from(filter.level);
        let scale = 1 << (level >> DELTA_DOUBLING_SHIFT);
        let mut lookup = [[[0u8; MAX_MODE_LF_DELTAS]; MAX_REF_FRAMES]; MAX_SEGMENTS];
        for (segment, levels) in lookup.iter_mut().enumerate() {
            let segment_level = match segmentation.feature(segment as u8, SEG_LVL_ALT_L) {
                Some(data) if segmentation.is_absolute => data.clamp(0, MAX_LOOP_FILTER),
                Some(data) => (level + data).clamp(0, MAX_LOOP_FILTER),
                None => level,
            };
            if !filter.is_delta_enabled {
                *levels = [[segment_level as u8; MAX_MODE_LF_DELTAS]; MAX_REF_FRAMES];
                continue;
            }
            let intra = segment_level + i32::from(filter.ref_deltas[usize::from(INTRA_FRAME as u8)]) * scale;
            levels[usize::from(INTRA_FRAME as u8)][0] = intra.clamp(0, MAX_LOOP_FILTER) as u8;
            for (modes, &reference_delta) in levels.iter_mut().zip(&filter.ref_deltas).skip(1) {
                for (value, &mode_delta) in modes.iter_mut().zip(&filter.mode_deltas) {
                    let inter = segment_level + i32::from(reference_delta) * scale + i32::from(mode_delta) * scale;
                    *value = inter.clamp(0, MAX_LOOP_FILTER) as u8;
                }
            }
        }
        Levels { lookup, sharpness: filter.sharpness }
    }

    // lvl, limit, blimit and thresh of a block (VP9 8.8.4).
    fn of_block(&self, info: &Info) -> (i32, i32, i32, i32) {
        let mode_type = usize::from(matches!(info.mode, NEARESTMV | NEARMV | NEWMV));
        let reference = info.refs[0].max(INTRA_FRAME) as usize;
        let level = i32::from(self.lookup[usize::from(info.segment)][reference][mode_type]);
        let shift = if self.sharpness > SHARPNESS_DOUBLE { 2 } else { u32::from(self.sharpness > 0) };
        let limit = if self.sharpness > 0 {
            (level >> shift).clamp(1, SHARPNESS_LIMIT - i32::from(self.sharpness))
        } else {
            (level >> shift).max(1)
        };
        (level, limit, 2 * (level + 2) + limit, level >> HEV_SHIFT)
    }
}

// The loop filter of a frame, superblock by superblock, each plane's vertical edges before its horizontal (VP9 8.8).
pub(super) fn filter_frame(
    planes: &mut [alloc::vec::Vec<u8>; 3],
    geometries: &[Geometry; 3],
    grid: &[Info],
    (mi_rows, mi_cols): (usize, usize),
    levels: &Levels,
) {
    for row in (0..mi_rows).step_by(SUPERBLOCK_MI) {
        for col in (0..mi_cols).step_by(SUPERBLOCK_MI) {
            for (plane, (samples, geometry)) in planes.iter_mut().zip(geometries).enumerate() {
                for is_horizontal in [false, true] {
                    let superblock =
                        Superblock { row, col, mi_rows, mi_cols, sub: usize::from(plane > 0), is_horizontal };
                    superblock.filter(samples, geometry.stride, grid, levels);
                }
            }
        }
    }
}

struct Superblock {
    row: usize,
    col: usize,
    mi_rows: usize,
    mi_cols: usize,
    // The plane's subsampling on both axes: 4:2:0 alone.
    sub: usize,
    is_horizontal: bool,
}

impl Superblock {
    // 8.8.2, run by run: the samples of a run along an edge lie in one block of the plane's.
    fn filter(&self, samples: &mut [u8], stride: usize, grid: &[Info], levels: &Levels) {
        let sub = self.sub;
        let length = SUPERBLOCK_SIDE >> sub;
        for edge in 0..(SUPERBLOCK_SIDE / EDGE_STEP) >> sub {
            for run in (0..length).step_by(RUN) {
                let (x, y) = if self.is_horizontal {
                    (self.col * MI_SIZE + (run << sub), self.row * MI_SIZE + edge * (EDGE_STEP << sub))
                } else {
                    (self.col * MI_SIZE + edge * (EDGE_STEP << sub), self.row * MI_SIZE + (run << sub))
                };
                if x >= MI_SIZE * self.mi_cols || y >= MI_SIZE * self.mi_rows {
                    continue;
                }
                if (!self.is_horizontal && x == 0) || (self.is_horizontal && y == 0) {
                    continue;
                }
                let loop_col = ((x / MI_SIZE) >> sub) << sub;
                let loop_row = ((y / MI_SIZE) >> sub) << sub;
                let Some(info) = grid.get(loop_row * self.mi_cols + loop_col) else {
                    continue;
                };
                let size = usize::from(info.size);
                let tx_size = info.plane_tx_size(sub);
                let block_size = if sub == 0 { size } else { size.max(usize::from(BLOCK_16X16)) };
                let is_block_edge = if self.is_horizontal {
                    y % (MI_SIZE * usize::from(NUM_8X8_HIGH[block_size])) == 0
                } else {
                    x % (MI_SIZE * usize::from(NUM_8X8_WIDE[block_size])) == 0
                };
                let is_chroma_right_edge = self.is_horizontal
                    && sub == 1
                    && self.mi_cols % 2 == 1
                    && edge % 2 == 1
                    && x + MI_SIZE >= self.mi_cols * MI_SIZE;
                let is_tx_edge = !is_chroma_right_edge && edge % (1 << tx_size) == 0;
                let is_intra = info.refs[0] <= INTRA_FRAME;
                if !(is_block_edge || (is_tx_edge && (is_intra || !info.is_skip))) {
                    continue;
                }
                let is_32_edge = edge % EDGES_PER_32 == 0;
                let base_size = if tx_size == TX_4X4 && is_32_edge { TX_8X8 } else { tx_size.min(TX_16X16) };
                let is_last =
                    if self.is_horizontal { y / MI_SIZE == self.mi_rows - 1 } else { x / MI_SIZE == self.mi_cols - 1 };
                let filter_size = if sub == 1 && base_size == TX_16X16 && is_last { TX_8X8 } else { base_size };
                let (level, limit, blimit, thresh) = levels.of_block(info);
                if level == 0 {
                    continue;
                }
                // The run's samples on screen: those whose luma place along the edge is inside the frame.
                let along_limit =
                    if self.is_horizontal { MI_SIZE * self.mi_cols - x } else { MI_SIZE * self.mi_rows - y };
                let count = RUN.min(along_limit.div_ceil(1 << sub));
                let (plane_x, plane_y) = (x >> sub, y >> sub);
                let along = if self.is_horizontal { 1 } else { stride };
                let at = plane_y * stride + plane_x;
                // The limits are at most 2 * (MAX_LOOP_FILTER + 2) + MAX_LOOP_FILTER, which 16 bits hold.
                let limits = [blimit, limit, thresh].map(|limit| limit as i16);
                let plane = (stride, self.is_horizontal);
                if count == RUN {
                    filter_run::<RUN>(samples, at, plane, filter_size, limits);
                } else {
                    for index in 0..count {
                        filter_run::<1>(samples, at + index * along, plane, filter_size, limits);
                    }
                }
            }
        }
    }
}

// 8.8.5 for N samples of a run at once, the first's q0 at `at` of a plane of lines `stride` apart, across a level boundary or an upright one: the masks, then in each lane the narrow filter or a wide one; every step is a step of all lanes, which the compiler makes vector instructions of.
#[inline(always)]
fn filter_run<const N: usize>(
    samples: &mut [u8],
    at: usize,
    (stride, is_level): (usize, bool),
    size: u8,
    limits: [i16; 3],
) {
    let (steps, across) = ((stride, is_level), if is_level { stride } else { 1 });
    let near_start = at - SIDES / 2 * across;
    let sides: [Lanes<N>; SIDES] = edge::gather(samples, near_start, steps);
    let [blimit, limit, thresh] = limits;
    let (_, is_smooth, is_high) = edge::masks(&sides, (blimit, limit, thresh));
    let [p3, p2, p1, p0, q0, q1, q2, q3] = &sides;
    let flat = [apart(p1, p0), apart(p2, p0), apart(p3, p0), apart(q1, q0), apart(q2, q0), apart(q3, q0)];
    let is_flat = if size >= TX_8X8 { zip(&is_smooth, &flatness(&flat), |smooth, flat| smooth & flat) } else { [0; N] };
    let narrow = edge::narrow(&sides, &zip(&is_smooth, &is_flat, |smooth, flat| smooth & !flat), &is_high);
    let mut near = sides;
    near[SIDES / 2 - narrow.len() / 2..SIDES / 2 + narrow.len() / 2].copy_from_slice(&narrow);
    // The samples past p3 and q3 are read for every lane of the run once any lane is flat at TX_16X16 or more, and only the lanes that are wide take the 15-tap filter's (8.8.5).
    if size >= TX_16X16 && is_flat.iter().any(|&lane| lane != 0) {
        let far_start = at - WIDE / 2 * across;
        let whole: [Lanes<N>; WIDE] = edge::gather(samples, far_start, steps);
        // p7 to p4 against p0, and q4 to q7 against q0.
        let [p7, p6, p5, p4] = [&whole[0], &whole[1], &whole[2], &whole[3]];
        let [q4, q5, q6, q7] = [&whole[WIDE - 4], &whole[WIDE - 3], &whole[WIDE - 2], &whole[WIDE - 1]];
        let far = [apart(p4, p0), apart(p5, p0), apart(p6, p0), apart(p7, p0)];
        let farther = [apart(q4, q0), apart(q5, q0), apart(q6, q0), apart(q7, q0)];
        let is_wide = zip(&zip(&is_flat, &flatness(&far), |flat, wide| flat & wide), &flatness(&farther), |a, b| a & b);
        let is_eight = zip(&is_flat, &is_wide, |flat, wide| flat & !wide);
        let (eight, sixteen) = (flattened::<N, WIDE, LOG2_8>(&whole), flattened::<N, WIDE, LOG2_16>(&whole));
        let mut out = whole;
        out[WIDE / 2 - SIDES / 2..WIDE / 2 + SIDES / 2].copy_from_slice(&near);
        for (index, value) in out.iter_mut().enumerate() {
            *value = pick(&pick(value, &eight[index], &is_eight), &sixteen[index], &is_wide);
        }
        edge::scatter(samples, far_start, steps, (&out, 1..WIDE - 1));
    } else {
        let eight = flattened::<N, SIDES, LOG2_8>(&sides);
        for (value, eight) in near.iter_mut().zip(&eight) {
            *value = pick(value, eight, &is_flat);
        }
        edge::scatter(samples, near_start, steps, (&near, 1..SIDES - 1));
    }
}

// All ones in a lane where every difference from the boundary is within FLAT.
#[inline(always)]
fn flatness<const N: usize>(differences: &[Lanes<N>]) -> Lanes<N> {
    let largest = differences.iter().fold([0; N], |largest, next| zip(&largest, next, i16::max));
    each(&largest, |largest| mask(largest <= FLAT))
}

// 8.8.5.3: each of the 2 * taps samples nearest the boundary in the middle of `S` the mean of itself and the 2 * taps + 1 about it, those past the reach repeating the last.
#[inline(always)]
fn flattened<const N: usize, const S: usize, const LOG2: u32>(sides: &[Lanes<N>; S]) -> [Lanes<N>; S] {
    let (middle, taps) = ((S / 2) as i32, (1i32 << (LOG2 - 1)) - 1);
    let value = |offset: i32| &sides[(middle + offset.clamp(-(taps + 1), taps)) as usize];
    let mut out = *sides;
    for offset in -taps..taps {
        let total = (-taps..=taps).fold(*value(offset), |total, tap| zip(&total, value(offset + tap), |a, b| a + b));
        out[(middle + offset) as usize] = each(&total, |total| (total + (1 << (LOG2 - 1))) >> LOG2);
    }
    out
}

// The masks and the filters of one sample across the boundary at `at`, `step` apart.
#[cfg(test)]
fn filter_sample(samples: &mut [u8], at: usize, step: usize, size: u8, (limit, blimit, thresh): (i32, i32, i32)) {
    // One sample is one lane: a step of one is an upright boundary's, a wider one a level boundary's.
    filter_run::<1>(samples, at, (step, step != 1), size, [blimit, limit, thresh].map(|limit| limit as i16));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_and_wide_filters() {
        // A step of 4 across the boundary: rough enough to filter, too rough to be flat.
        let mut line = [10u8, 10, 10, 10, 14, 14, 14, 14];
        filter_sample(&mut line, 4, 1, TX_4X4, (10, 40, 1));
        // filter = 3 * 4 = 12; filter1 = 16 >> 3 = 2, filter2 = 15 >> 3 = 1; then the outer pair by (2 + 1) >> 1.
        assert_eq!(line, [10, 10, 11, 11, 12, 13, 14, 14], "the narrow filter moves two samples on each side");
        let mut flat = [10u8, 10, 10, 10, 11, 11, 11, 11];
        filter_sample(&mut flat, 4, 1, TX_8X8, (10, 40, 1));
        assert_eq!(
            flat,
            [10, 10, 10, 10, 11, 11, 11, 11],
            "a flat step of 1 rounds back to itself under the 7 tap filter"
        );
        let mut ramp = [8u8, 8, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9];
        filter_sample(&mut ramp, 8, 1, TX_16X16, (10, 40, 1));
        assert_eq!(ramp[7], 8, "the 15 tap filter keeps a step of 1 below its rounding");
        let mut rough = [0u8, 50, 0, 50, 100, 50, 100, 50];
        let before = rough;
        filter_sample(&mut rough, 4, 1, TX_16X16, (10, 40, 1));
        assert_eq!(rough, before, "samples that vary past the limit are left alone");
    }

    // A plane's side, and where the boundary of the runs of the tests below crosses it and where their lanes start.
    const SIDE: usize = 24;
    const BOUNDARY: usize = 12;
    const FIRST: usize = 8;

    #[test]
    fn lanes_filter_as_one() {
        let mut state = 0x9E37_79B9u32;
        let mut next = |bound: u32| {
            state = crate::base::math::xorshift32(state);
            state % bound
        };
        let mut moved = [0; 3];
        for round in 0..3000 {
            let (size, is_level) = ([TX_4X4, TX_8X8, TX_16X16][round / 2 % 3], round % 2 == 0);
            let mut plane: alloc::vec::Vec<u8> = (0..SIDE * SIDE).map(|_| next(256) as u8).collect();
            // Each lane's samples across the boundary flat, a step, a ramp or noise, so that every filter is taken.
            for lane in 0..RUN {
                let (kind, base, step) = (next(4), 40 + next(160) as i32, next(12) as i32);
                for index in 0..WIDE {
                    let place = BOUNDARY + index - WIDE / 2;
                    let value = match kind {
                        0 => base + next(2) as i32,
                        1 => base + if index < WIDE / 2 { 0 } else { step } + next(2) as i32,
                        2 => base + index as i32 * step / 4,
                        _ => next(256) as i32,
                    };
                    let (x, y) = if is_level { (FIRST + lane, place) } else { (place, FIRST + lane) };
                    plane[y * SIDE + x] = value.clamp(0, 255) as u8;
                }
            }
            let level = 1 + next(63) as i16;
            let limits = [2 * (level + 2) + level, level, level >> HEV_SHIFT];
            let (at, along) = if is_level { (BOUNDARY * SIDE + FIRST, 1) } else { (FIRST * SIDE + BOUNDARY, SIDE) };
            let (mut together, mut apart) = (plane.clone(), plane.clone());
            filter_run::<RUN>(&mut together, at, (SIDE, is_level), size, limits);
            for lane in 0..RUN {
                filter_run::<1>(&mut apart, at + lane * along, (SIDE, is_level), size, limits);
            }
            assert_eq!(together, apart, "round {round}: size {size}, level {is_level}, limits {limits:?}");
            moved[usize::from(size)] += usize::from(together != plane);
        }
        assert!(moved.iter().all(|&count| count > 100), "every filter size moved samples: {moved:?}");
    }

    #[test]
    fn levels_by_segment_and_delta() {
        let filter = LoopFilter {
            level: 40,
            sharpness: 0,
            is_delta_enabled: true,
            ref_deltas: [1, 0, -1, -1],
            mode_deltas: [0, 0],
        };
        let levels = Levels::of(&filter, &Segmentation::default());
        // Level 40 doubles the deltas: intra 40 + 2 is 42, golden 40 + -2 is 38.
        assert_eq!(levels.lookup[0][0][0], 42, "the intra delta, doubled above level 31");
        assert_eq!(levels.lookup[0][2][1], 38, "golden's delta, doubled");
        let plain = Levels::of(&LoopFilter { is_delta_enabled: false, ..filter }, &Segmentation::default());
        assert_eq!(plain.lookup[3][1][1], 40, "without deltas every block takes the frame's level");
        let info = Info { refs: [INTRA_FRAME, -1], ..Info::default() };
        assert_eq!(levels.of_block(&info), (42, 42, 2 * 44 + 42, 2), "no sharpness: the limit is the level");
    }
}
