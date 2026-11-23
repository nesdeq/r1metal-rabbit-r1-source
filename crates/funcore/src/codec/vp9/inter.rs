use super::modes::{Edges, Mv, clamp};
use super::tables::{SUBPEL_FILTERS, SUPERBLOCK_SIDE};
use crate::base::simd;

// Positions in sixteenths of a sample, and the filters' taps (VP9 3, 8.5.2.4).
const SUBPEL_BITS: u32 = 4;
const SUBPEL_MASK: i32 = 15;
const SUBPEL_SHIFTS: i32 = 16;
const TAPS: usize = 8;
const TAPS_BEFORE: i32 = 3;
// How far past a block a clamped vector may reach, in samples (VP9 3, 8.5.2.2).
const INTERP_EXTEND: i32 = 4;
// The scale factors' fixed point, and the scale of a reference of the frame's own size (VP9 3, 8.5.2.3).
const REF_SCALE_SHIFT: u32 = 14;
const NO_SCALE: i64 = 1 << REF_SCALE_SHIFT;
// A reference at least half and at most 16 times the frame's size on each side (VP9 8.5.2.3).
const MOST_SHRINK: usize = 2;
const MOST_GROWTH: usize = 16;
// The reference samples the prediction of the largest block, a superblock, reads along a side at the largest step, 32 (8.5.2.4).
const MOST_STEP: usize = 32;
const MOST_REACH: usize = (((SUPERBLOCK_SIDE - 1) * MOST_STEP + SUBPEL_MASK as usize) >> SUBPEL_BITS) + TAPS;
// A vector in eighths of a luma sample is in sixteenths of a sample twice over for luma, once for chroma (8.5.2.2).
const LUMA_SIXTEENTHS: i32 = 2;

// A reference's size against the frame's, in REF_SCALE_SHIFT bits of fraction (VP9 8.5.2.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Scale {
    x: i64,
    y: i64,
}

impl Scale {
    // None where the reference is too large or too small to predict from.
    pub(super) fn of(
        (reference_width, reference_height): (usize, usize),
        (width, height): (usize, usize),
    ) -> Option<Scale> {
        let is_valid = MOST_SHRINK * width >= reference_width
            && MOST_SHRINK * height >= reference_height
            && width <= MOST_GROWTH * reference_width
            && height <= MOST_GROWTH * reference_height;
        is_valid.then(|| Scale {
            x: ((reference_width as i64) << REF_SCALE_SHIFT) / width as i64,
            y: ((reference_height as i64) << REF_SCALE_SHIFT) / height as i64,
        })
    }

    const fn is_scaled(self) -> bool {
        self.x != NO_SCALE || self.y != NO_SCALE
    }

    const fn scale_x(self, value: i64) -> i64 {
        (value * self.x) >> REF_SCALE_SHIFT
    }

    const fn scale_y(self, value: i64) -> i64 {
        (value * self.y) >> REF_SCALE_SHIFT
    }
}

// A reference plane and the samples it has across and down, past which its edges repeat.
pub(super) struct Source<'a> {
    pub(super) plane: &'a [u8],
    pub(super) stride: usize,
    pub(super) width: usize,
    pub(super) height: usize,
}

// Where a block of a plane is predicted from.
#[derive(Clone, Copy)]
pub(super) struct Target {
    // The block's top left sample in the plane, and that of the 8x8 block holding it in luma samples.
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) luma_x: usize,
    pub(super) luma_y: usize,
    pub(super) width: usize,
    pub(super) height: usize,
    // The whole block's size in the plane's samples and its edges, which clamp a scaled prediction's vector.
    pub(super) block_width: usize,
    pub(super) block_height: usize,
    pub(super) edges: Edges,
    pub(super) is_chroma: bool,
}

// The room a prediction works in, whatever it holds: the lines its pass across leaves, and a compound's second prediction before it is averaged.
pub(super) struct Scratch {
    intermediate: [[u8; SUPERBLOCK_SIDE]; MOST_REACH],
    predicted: [u8; SUPERBLOCK_SIDE * SUPERBLOCK_SIDE],
}

impl Scratch {
    pub(super) const fn new() -> Scratch {
        Scratch::filled(0)
    }

    const fn filled(sample: u8) -> Scratch {
        Scratch {
            intermediate: [[sample; SUPERBLOCK_SIDE]; MOST_REACH],
            predicted: [sample; SUPERBLOCK_SIDE * SUPERBLOCK_SIDE],
        }
    }
}

// The start in sixteenths of a sample and the steps of a prediction (VP9 8.5.2.2, 8.5.2.3, as libvpx computes them).
fn start(mv: Mv, scale: Scale, target: Target) -> (i64, i64, i64, i64) {
    let unit32 = LUMA_SIXTEENTHS >> u32::from(target.is_chroma);
    if !scale.is_scaled() {
        let x = ((target.x as i64) << SUBPEL_BITS) + i64::from(mv.col) * i64::from(unit32);
        let y = ((target.y as i64) << SUBPEL_BITS) + i64::from(mv.row) * i64::from(unit32);
        return (x, y, i64::from(SUBPEL_SHIFTS), i64::from(SUBPEL_SHIFTS));
    }
    let edges = target.edges;
    let spel_left = (INTERP_EXTEND + target.block_width as i32) << SUBPEL_BITS;
    let spel_top = (INTERP_EXTEND + target.block_height as i32) << SUBPEL_BITS;
    let row = (i32::from(mv.row) * unit32) as i16;
    let col = (i32::from(mv.col) * unit32) as i16;
    let row = clamp(i32::from(row), edges.top * unit32 - spel_top, edges.bottom * unit32 + spel_top - SUBPEL_SHIFTS);
    let col = clamp(i32::from(col), edges.left * unit32 - spel_left, edges.right * unit32 + spel_left - SUBPEL_SHIFTS);
    let (row, col) = (i64::from(row as i16), i64::from(col as i16));
    let fraction_x = scale.scale_x((target.luma_x as i64) << SUBPEL_BITS) & i64::from(SUBPEL_MASK);
    let fraction_y = scale.scale_y((target.luma_y as i64) << SUBPEL_BITS) & i64::from(SUBPEL_MASK);
    let x = (scale.scale_x(target.x as i64) << SUBPEL_BITS) + scale.scale_x(col) + fraction_x;
    let y = (scale.scale_y(target.y as i64) << SUBPEL_BITS) + scale.scale_y(row) + fraction_y;
    (x, y, scale.scale_x(i64::from(SUBPEL_SHIFTS)), scale.scale_y(i64::from(SUBPEL_SHIFTS)))
}

// Predicts a block into the plane from a reference, or averages that prediction with what is there (VP9 8.5.2.4).
pub(super) fn predict(
    source: &Source<'_>,
    (plane, stride): (&mut [u8], usize),
    (mv, scale, filter): (Mv, Scale, u8),
    target: Target,
    is_average: bool,
    scratch: &mut Scratch,
) {
    let Scratch { intermediate, predicted } = scratch;
    let (x, y, step_x, step_y) = start(mv, scale, target);
    let kernel = &SUBPEL_FILTERS[usize::from(filter)];
    let (width, height) = (target.width, target.height);
    let (phase_x, phase_y) = ((x & i64::from(SUBPEL_MASK)) as usize, (y & i64::from(SUBPEL_MASK)) as usize);
    let (step_x, step_y) = (step_x as usize, step_y as usize);
    let is_copy_x = phase_x == 0 && step_x == SUBPEL_SHIFTS as usize;
    let is_copy_y = phase_y == 0 && step_y == SUBPEL_SHIFTS as usize;
    let (taps_before, taps) = if is_copy_y { (0, 1) } else { (TAPS_BEFORE as i64, TAPS) };
    let rows = (((height - 1) * step_y + phase_y) >> SUBPEL_BITS) + taps;
    let span = (((width - 1) * step_x + phase_x) >> SUBPEL_BITS) + TAPS;
    let (first_x, first_y) = ((x >> SUBPEL_BITS) - i64::from(TAPS_BEFORE), (y >> SUBPEL_BITS) - taps_before);
    let (last_x, last_y) = (source.width as i64 - 1, source.height as i64 - 1);
    let is_inside = first_x >= 0 && first_x + span as i64 - 1 <= last_x;
    let mut gathered = [0u8; MOST_REACH];
    let is_moved = !is_copy_x && step_x == SUBPEL_SHIFTS as usize;
    // Lines that need no edge repeated, moved by one fraction, are filtered in one go from the reference itself; others line by line.
    let is_whole = is_moved && is_inside && first_y >= 0 && first_y + rows as i64 - 1 <= last_y;
    if is_whole {
        let start = first_y as usize * source.stride + first_x as usize;
        let lines = (intermediate.as_flattened_mut(), SUPERBLOCK_SIDE);
        let (samples, out) = ((&source.plane[start..], source.stride), lines);
        simd::filter(samples, 1, &kernel[phase_x], out, (width, rows));
    }
    for (row, out) in intermediate.iter_mut().enumerate().take(if is_whole { 0 } else { rows }) {
        let line = (first_y + row as i64).clamp(0, last_y) as usize * source.stride;
        let samples: &[u8] = if is_inside {
            &source.plane[line + first_x as usize..line + first_x as usize + span]
        } else {
            for (column, value) in gathered.iter_mut().enumerate().take(span) {
                *value = source.plane[line + (first_x + column as i64).clamp(0, last_x) as usize];
            }
            &gathered[..span]
        };
        if is_copy_x {
            out[..width].copy_from_slice(&samples[TAPS_BEFORE as usize..TAPS_BEFORE as usize + width]);
        } else if is_moved {
            simd::filter((samples, span), 1, &kernel[phase_x], (out, SUPERBLOCK_SIDE), (width, 1));
        } else {
            // A scaled reference steps by another fraction from sample to sample: each takes taps of its own.
            for (column, value) in out.iter_mut().enumerate().take(width) {
                let position = phase_x + column * step_x;
                *value =
                    simd::plain::tap(&samples[position >> SUBPEL_BITS..], 1, &kernel[position & SUBPEL_MASK as usize]);
            }
        }
    }
    let at = target.y * stride + target.x;
    let lines = (intermediate.as_flattened(), SUPERBLOCK_SIDE);
    if is_average {
        down(lines, kernel, (phase_y, step_y, is_copy_y), (&mut predicted[..], SUPERBLOCK_SIDE), (width, height));
        for (row, predicted) in predicted.as_chunks::<SUPERBLOCK_SIDE>().0.iter().take(height).enumerate() {
            let out = &mut plane[at + row * stride..at + row * stride + width];
            for (value, &predicted) in out.iter_mut().zip(predicted) {
                *value = ((u16::from(*value) + u16::from(predicted) + 1) >> 1) as u8;
            }
        }
    } else {
        down(lines, kernel, (phase_y, step_y, is_copy_y), (&mut plane[at..], stride), (width, height));
    }
}

// The pass down the lines the pass across left: each output line the taps over the lines from its place, or a copy of one where no fraction moves it (VP9 8.5.2.4).
fn down(
    (lines, line_stride): (&[u8], usize),
    kernel: &[[i32; TAPS]; SUBPEL_SHIFTS as usize],
    (phase, step, is_copy): (usize, usize, bool),
    (out, stride): (&mut [u8], usize),
    (width, height): (usize, usize),
) {
    if is_copy {
        for row in 0..height {
            out[row * stride..][..width].copy_from_slice(&lines[row * line_stride..][..width]);
        }
    } else if step == SUBPEL_SHIFTS as usize {
        simd::filter((lines, line_stride), line_stride, &kernel[phase], (out, stride), (width, height));
    } else {
        for row in 0..height {
            let position = phase + row * step;
            let (first, taps) = ((position >> SUBPEL_BITS) * line_stride, &kernel[position & SUBPEL_MASK as usize]);
            simd::filter(
                (&lines[first..], line_stride),
                line_stride,
                taps,
                (&mut out[row * stride..], stride),
                (width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn target(x: usize, y: usize) -> Target {
        let edges = Edges { left: 0, right: 0, top: 0, bottom: 0 };
        Target {
            x,
            y,
            luma_x: x,
            luma_y: y,
            width: 4,
            height: 4,
            block_width: 4,
            block_height: 4,
            edges,
            is_chroma: false,
        }
    }

    #[test]
    fn scale_bounds() {
        assert_eq!(Scale::of((64, 64), (64, 64)).map(Scale::is_scaled), Some(false), "the frame's own size");
        assert_eq!(Scale::of((128, 64), (64, 64)).map(|scale| scale.x), Some(2 << 14), "twice as wide");
        assert!(Scale::of((129, 64), (64, 64)).is_none(), "more than twice as wide");
        assert!(Scale::of((4, 4), (65, 64)).is_none(), "the frame more than 16 times wider");
    }

    #[test]
    fn whole_and_half_samples() {
        let source_plane: alloc::vec::Vec<u8> = (0..64).map(|place| (place % 8 * 10) as u8).collect();
        let source = Source { plane: &source_plane, stride: 8, width: 8, height: 8 };
        let scale = Scale::of((8, 8), (8, 8)).unwrap();
        let mut plane = vec![0u8; 64];
        // Room that comes filled: nothing of it reaches the plane.
        let mut scratch = Scratch::filled(u8::MAX);
        predict(&source, (&mut plane, 8), (Mv { row: 0, col: 8 }, scale, 0), target(2, 2), false, &mut scratch);
        assert_eq!(&plane[2 * 8 + 2..2 * 8 + 6], &[30, 40, 50, 60], "a whole sample to the right copies");
        predict(&source, (&mut plane, 8), (Mv { row: 0, col: 4 * 8 }, scale, 0), target(2, 2), false, &mut scratch);
        assert_eq!(&plane[2 * 8 + 2..2 * 8 + 6], &[60, 70, 70, 70], "past the right edge the last column repeats");
        predict(&source, (&mut plane, 8), (Mv { row: 0, col: 4 }, scale, 3), target(2, 2), false, &mut scratch);
        assert_eq!(&plane[2 * 8 + 2..2 * 8 + 6], &[25, 35, 45, 55], "half a sample by the bilinear filter");
        predict(&source, (&mut plane, 8), (Mv { row: 0, col: 0 }, scale, 0), target(2, 2), true, &mut scratch);
        assert_eq!(&plane[2 * 8 + 2..2 * 8 + 6], &[23, 33, 43, 53], "the second prediction of a compound averages");
    }
}
