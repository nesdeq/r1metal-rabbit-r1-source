// The loop filters' steps over several segments across an edge at once, a segment in each lane, which the compiler makes vector instructions of; the masks and the narrow filter are VP8's normal filter (RFC 6386, 15.2) and VP9's (8.8.5.1, 8.8.5.2) alike.

use crate::base::simd::{self, BLOCK};
use core::ops::Range;

// A value of each segment, one in each lane; every value a loop filter reckons fits 16 bits.
pub(super) type Lanes<const N: usize> = [i16; N];
// The pixels on either side of an edge the masks and the narrow filter read: p3 to q3.
pub(super) const SIDES: usize = 8;
// The narrow filter's rounding of its adjustment to eighths.
const ADJUST_SHIFT: u32 = 3;

#[inline(always)]
pub(super) fn zip<const N: usize>(first: &Lanes<N>, second: &Lanes<N>, join: impl Fn(i16, i16) -> i16) -> Lanes<N> {
    core::array::from_fn(|lane| join(first[lane], second[lane]))
}

#[inline(always)]
pub(super) fn each<const N: usize>(values: &Lanes<N>, change: impl Fn(i16) -> i16) -> Lanes<N> {
    core::array::from_fn(|lane| change(values[lane]))
}

// The lanes of `base` moved by those of `by` where `mask` is all ones.
#[inline(always)]
pub(super) fn moved<const N: usize>(base: &Lanes<N>, by: &Lanes<N>, mask: &Lanes<N>) -> Lanes<N> {
    core::array::from_fn(|lane| base[lane] + (by[lane] & mask[lane]))
}

// The lanes of `chosen` where `mask` is all ones, of `base` elsewhere.
#[inline(always)]
pub(super) fn pick<const N: usize>(base: &Lanes<N>, chosen: &Lanes<N>, mask: &Lanes<N>) -> Lanes<N> {
    core::array::from_fn(|lane| (chosen[lane] & mask[lane]) | (base[lane] & !mask[lane]))
}

// All ones where a condition holds, nought where not.
#[inline(always)]
pub(super) fn mask(holds: bool) -> i16 {
    -i16::from(holds)
}

#[inline(always)]
pub(super) fn signed(value: i16) -> i16 {
    value.clamp(-128, 127)
}

#[inline(always)]
pub(super) fn apart<const N: usize>(first: &Lanes<N>, second: &Lanes<N>) -> Lanes<N> {
    zip(first, second, |p, q| (p - q).abs())
}

// Where the step across the edge is within `edge`, where every step beside it is within `interior` too, and where a step next to the edge passes `hev`.
#[inline(always)]
pub(super) fn masks<const N: usize>(
    sides: &[Lanes<N>; SIDES],
    (edge, interior, hev): (i16, i16, i16),
) -> (Lanes<N>, Lanes<N>, Lanes<N>) {
    let [p3, p2, p1, p0, q0, q1, q2, q3] = sides;
    let steps = zip(&apart(p0, q0), &apart(p1, q1), |across, beside| across * 2 + (beside >> 1));
    let is_edge = each(&steps, |step| mask(step <= edge));
    let near = zip(&apart(p1, p0), &apart(q1, q0), i16::max);
    let beside = [apart(p3, p2), apart(p2, p1), apart(q3, q2), apart(q2, q1)];
    let largest = beside.iter().fold(near, |largest, next| zip(&largest, next, i16::max));
    let is_smooth = zip(&is_edge, &largest, |is_edge, largest| is_edge & mask(largest <= interior));
    (is_edge, is_smooth, each(&near, |near| mask(near > hev)))
}

// The narrow filter where `filtered`: p0 and q0 moved by an adjustment with the outer taps where `high`, p1 and q1 too by half of one without them elsewhere; returns p1, p0, q0 and q1.
#[inline(always)]
pub(super) fn narrow<const N: usize>(sides: &[Lanes<N>; SIDES], filtered: &Lanes<N>, high: &Lanes<N>) -> [Lanes<N>; 4] {
    let [_, _, p1, p0, q0, q1, _, _] = sides;
    let outer = zip(&zip(p1, q1, |p, q| signed(p - q)), high, |outer, high| outer & high);
    let a = zip(&outer, &zip(q0, p0, |q, p| 3 * (q - p)), |outer, step| signed(outer + step));
    let (f1, f2) = (each(&a, |a| (a + 4).min(127) >> ADJUST_SHIFT), each(&a, |a| (a + 3).min(127) >> ADJUST_SHIFT));
    let (inner, half) = (zip(filtered, high, |filtered, high| filtered & !high), each(&f1, |f1| (f1 + 1) >> 1));
    let (less, fewer) = (each(&f1, |f1| -f1), each(&half, |half| -half));
    [moved(p1, &half, &inner), moved(p0, &f2, filtered), moved(q0, &less, filtered), moved(q1, &fewer, &inner)]
}

// The pixels of N segments across an edge, `S` of each, from `start` in a plane of lines `stride` apart: segments side by side, down the lines across a level edge, load as one vector a pixel; segments one a line, along an upright edge, as the columns of blocks of BLOCK pixels of BLOCK lines where they fill such blocks, else a pixel at a time.
#[inline(always)]
pub(super) fn gather<const N: usize, const S: usize>(
    plane: &[u8],
    start: usize,
    (stride, is_beside): (usize, bool),
) -> [Lanes<N>; S] {
    // Loops, as the compiler leaves a nested `from_fn` of these a call of its own.
    let mut lanes = [[0; N]; S];
    if is_beside {
        for (index, values) in lanes.iter_mut().enumerate() {
            for (value, &pixel) in values.iter_mut().zip(&plane[start + index * stride..][..N]) {
                *value = i16::from(pixel);
            }
        }
    } else if N == BLOCK && S.is_multiple_of(BLOCK) {
        for (part, sides) in lanes.as_chunks_mut::<BLOCK>().0.iter_mut().enumerate() {
            for (values, column) in sides.iter_mut().zip(simd::columns(&plane[start + part * BLOCK..], stride)) {
                values.copy_from_slice(&column);
            }
        }
    } else {
        for (index, values) in lanes.iter_mut().enumerate() {
            for (lane, value) in values.iter_mut().enumerate() {
                *value = i16::from(plane[start + lane * stride + index]);
            }
        }
    }
    lanes
}

// Puts the pixels `rows` of the segments back where `gather` took them, each held to a byte.
#[inline(always)]
pub(super) fn scatter<const N: usize, const S: usize>(
    plane: &mut [u8],
    start: usize,
    (stride, is_beside): (usize, bool),
    (lanes, rows): (&[Lanes<N>; S], Range<usize>),
) {
    if is_beside {
        for index in rows {
            for (pixel, &value) in plane[start + index * stride..][..N].iter_mut().zip(&lanes[index]) {
                *pixel = value.clamp(0, 255) as u8;
            }
        }
    } else if N == BLOCK && S.is_multiple_of(BLOCK) {
        for (part, sides) in lanes.as_chunks::<BLOCK>().0.iter().enumerate() {
            let first = part * BLOCK;
            let [low, high] = [rows.start, rows.end].map(|end| end.clamp(first, first + BLOCK) - first);
            let columns = core::array::from_fn(|index| core::array::from_fn(|lane| sides[index][lane]));
            simd::put_columns(&columns, (&mut plane[start + first..], stride), low..high);
        }
    } else {
        for index in rows {
            for (lane, &value) in lanes[index].iter().enumerate() {
                plane[start + lane * stride + index] = value.clamp(0, 255) as u8;
            }
        }
    }
}
