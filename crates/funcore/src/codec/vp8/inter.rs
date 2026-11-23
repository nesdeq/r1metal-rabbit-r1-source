use super::frame::MACROBLOCK;
use super::tables::{BILINEAR_FILTERS, SIXTAP_FILTERS};
use crate::base::simd;
use crate::codec::picture::Geometry;

// A tap reaches two pixels before the one it is centred on and three after (RFC 6386, 18.3).
pub(crate) const BEFORE: usize = 2;
pub(crate) const AFTER: usize = 3;
// A vector's fraction of a pixel, in eighths, and half a pixel in them.
const FRACTION_BITS: u32 = 3;
const FRACTION: i32 = (1 << FRACTION_BITS) - 1;
pub(crate) const HALF_EIGHTHS: usize = 1 << (FRACTION_BITS - 1);
// The largest block a vector moves is a luma macroblock; its taps reach past it.
const LARGEST: usize = MACROBLOCK;
const SPAN: usize = LARGEST + BEFORE + AFTER;

// The filters a frame's version names: the six-tap one for version 0, the bilinear one for the others (RFC 6386, 9.1).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Filter {
    #[default]
    Sixtap,
    Bilinear,
}

// Predicts a block from the reference moved by `vector` in eighths, edge pixels repeated without end (RFC 6386, 18).
#[expect(clippy::too_many_arguments, reason = "a block, its place and its vector, and where it goes")]
pub(super) fn predict(
    (pixels, geometry): (&[u8], Geometry),
    (x, y): (i32, i32),
    vector: (i32, i32),
    (width, height): (usize, usize),
    filter: Filter,
    out: &mut [u8],
    at: usize,
    stride: usize,
) {
    let left = i64::from(x) + i64::from(vector.0 >> FRACTION_BITS);
    let top = i64::from(y) + i64::from(vector.1 >> FRACTION_BITS);
    let fraction = ((vector.0 & FRACTION) as usize, (vector.1 & FRACTION) as usize);
    let border = geometry.border as i64;
    let fits = |start: i64, length: usize, extent: usize| {
        start - (BEFORE as i64) >= -border && start + (length + AFTER) as i64 <= extent as i64 + border
    };
    let mut copy = [0u8; SPAN * SPAN];
    let (source, source_stride, origin) = if fits(left, width, geometry.width) && fits(top, height, geometry.height) {
        let origin = (top + border) as usize * geometry.stride + (left + border) as usize;
        (pixels, geometry.stride, origin)
    } else {
        // Far past the edges every pixel repeats one on the edge: the block is read through clamped places.
        let region = (width + BEFORE + AFTER, height + BEFORE + AFTER);
        for (line, row) in copy.as_chunks_mut::<SPAN>().0.iter_mut().take(region.1).enumerate() {
            let source_y = (top - BEFORE as i64 + line as i64).clamp(0, geometry.height as i64 - 1) as usize;
            let start = geometry.at(0, source_y);
            let edge = &pixels[start..start + geometry.width];
            for (place, pixel) in row[..region.0].iter_mut().enumerate() {
                let source_x = (left - BEFORE as i64 + place as i64).clamp(0, geometry.width as i64 - 1) as usize;
                *pixel = edge[source_x];
            }
        }
        (&copy[..], SPAN, BEFORE * SPAN + BEFORE)
    };
    match (filter, fraction) {
        (_, (0, 0)) => {
            for (line, row) in out[at..].chunks_mut(stride).take(height).enumerate() {
                let start = origin + line * source_stride;
                row[..width].copy_from_slice(&source[start..start + width]);
            }
        }
        (Filter::Sixtap, (across, down)) => {
            sixtap(source, source_stride, origin, (width, height), (across, down), out, at, stride)
        }
        (Filter::Bilinear, (across, down)) => {
            bilinear(source, source_stride, origin, (width, height), (across, down), out, at, stride)
        }
    }
}

// Six taps across, then down, each pass held to a byte; a pass of the taps 0, 0, 128 changes nothing and is left out.
#[expect(clippy::too_many_arguments, reason = "a source, a block and its fractions, and where it goes")]
fn sixtap(
    source: &[u8],
    source_stride: usize,
    origin: usize,
    (width, height): (usize, usize),
    (across, down): (usize, usize),
    out: &mut [u8],
    at: usize,
    stride: usize,
) {
    if down == 0 {
        let samples = (&source[origin - BEFORE..], source_stride);
        return simd::filter(samples, 1, &SIXTAP_FILTERS[across], (&mut out[at..], stride), (width, height));
    }
    let mut middle = [0u8; SPAN * LARGEST];
    let (rows, first) = (height + BEFORE + AFTER, origin - BEFORE * source_stride);
    let (passed, passed_stride, passed_origin) = if across == 0 {
        (source, source_stride, first)
    } else {
        let samples = (&source[first - BEFORE..], source_stride);
        simd::filter(samples, 1, &SIXTAP_FILTERS[across], (&mut middle, LARGEST), (width, rows));
        (&middle[..], LARGEST, 0)
    };
    let samples = (&passed[passed_origin..], passed_stride);
    simd::filter(samples, passed_stride, &SIXTAP_FILTERS[down], (&mut out[at..], stride), (width, height));
}

// The bilinear filter across, then down, over the pixel and the one after it (RFC 6386, 18.3).
#[expect(clippy::too_many_arguments, reason = "a source, a block and its fractions, and where it goes")]
fn bilinear(
    source: &[u8],
    source_stride: usize,
    origin: usize,
    (width, height): (usize, usize),
    (across, down): (usize, usize),
    out: &mut [u8],
    at: usize,
    stride: usize,
) {
    let mut middle = [0u8; SPAN * LARGEST];
    let samples = (&source[origin..], source_stride);
    simd::filter(samples, 1, &BILINEAR_FILTERS[across], (&mut middle, LARGEST), (width, height + 1));
    simd::filter((&middle, LARGEST), LARGEST, &BILINEAR_FILTERS[down], (&mut out[at..], stride), (width, height));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const SIDE: usize = 16;
    const BORDER: usize = 8;

    // A plane of `SIDE` pixels each way whose border repeats its edges, as a reference frame's does.
    fn plane(pixel: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let stride = SIDE + 2 * BORDER;
        let mut pixels = vec![0u8; stride * stride];
        for y in 0..stride {
            for x in 0..stride {
                let inside = |place: usize| place.clamp(BORDER, BORDER + SIDE - 1) - BORDER;
                pixels[y * stride + x] = pixel(inside(x), inside(y));
            }
        }
        pixels
    }

    fn block(pixels: &[u8], at: (i32, i32), vector: (i32, i32), size: usize, filter: Filter) -> Vec<u8> {
        let geometry = Geometry { width: SIDE, height: SIDE, border: BORDER, stride: SIDE + 2 * BORDER };
        let mut out = vec![0u8; size * size];
        predict((pixels, geometry), at, vector, (size, size), filter, &mut out, 0, size);
        out
    }

    #[test]
    fn sixtap_impulse() {
        let impulse = plane(|x, y| if (x, y) == (8, 8) { 128 } else { 0 });
        // Half a pixel right: outputs 5 to 10 of row 8 reach the impulse at 8 with their taps, last to first.
        let half = block(&impulse, (4, 8), (4, 0), 8, Filter::Sixtap);
        assert_eq!(
            half[..8],
            [0, 3, 0, 77, 77, 0, 3, 0],
            "the taps 3, -16, 77, 77, -16, 3, the negative ones held at 0"
        );
        let quarter = block(&impulse, (4, 8), (2, 0), 8, Filter::Sixtap);
        assert_eq!(quarter[..8], [0, 1, 0, 36, 108, 0, 2, 0], "the quarter taps 2, -11, 108, 36, -8, 1, reversed");
        let down = block(&impulse, (8, 4), (0, 6), 8, Filter::Sixtap);
        let column: Vec<u8> = down.chunks(8).map(|row| row[0]).collect();
        assert_eq!(column, [0, 2, 0, 108, 36, 0, 1, 0], "three quarters of a pixel down");
        assert!(down.chunks(8).all(|row| row[1..].iter().all(|&pixel| pixel == 0)), "one column alone");
    }

    #[test]
    fn sixtap_two_passes_rounded() {
        let ramp = plane(|x, y| (x * 9 + y * 5) as u8);
        let moved = block(&ramp, (2, 2), (4, 4), 4, Filter::Sixtap);
        // Half a pixel each way along a plane of 9 a pixel across and 5 down: 4.5 rounds to 5, then 2.5 to 3.
        let expected: Vec<u8> = (0..16).map(|place| ((2 + place % 4) * 9 + (2 + place / 4) * 5 + 8) as u8).collect();
        assert_eq!(moved, expected, "each pass rounds half up");
        let whole = block(&ramp, (3, 5), (16, -8), 4, Filter::Sixtap);
        let shifted: Vec<u8> = (0..16).map(|place| ((5 + place % 4) * 9 + (4 + place / 4) * 5) as u8).collect();
        assert_eq!(whole, shifted, "a vector of whole pixels copies");
    }

    #[test]
    fn bilinear_positions() {
        let ramp = plane(|x, _| (x * 16) as u8);
        for eighth in 0..8 {
            let moved = block(&ramp, (4, 4), (eighth, 0), 4, Filter::Bilinear);
            let expected = 64 + 2 * eighth as u8;
            assert_eq!(moved[0], expected, "{eighth} eighths along a ramp of 16 a pixel");
        }
        let steps = plane(|_, y| if y < 8 { 0 } else { 255 });
        let between = block(&steps, (0, 7), (0, 4), 4, Filter::Bilinear);
        assert_eq!(between[..4], [128, 128, 128, 128], "(0 * 64 + 255 * 64 + 64) >> 7 halfway down a step");
    }

    #[test]
    fn far_vectors_repeat_edges() {
        let ramp = plane(|x, y| (x * 9 + y * 5) as u8);
        let far_left = block(&ramp, (0, 4), (-8 * 300, 3), 4, Filter::Sixtap);
        let near_left = block(&ramp, (0, 4), (-8 * 6, 3), 4, Filter::Sixtap);
        assert_eq!(far_left, near_left, "past the border every column left of the frame is its first column");
        let far_down = block(&ramp, (4, 12), (5, 8 * 1000), 4, Filter::Bilinear);
        let corner = block(&ramp, (4, 12), (5, 8 * 7), 4, Filter::Bilinear);
        assert_eq!(far_down, corner, "and every line below it its last line");
        let extreme = block(&ramp, (0, 0), (i32::MIN, i32::MAX), 4, Filter::Sixtap);
        let bottom_left = ramp[(BORDER + SIDE - 1) * (SIDE + 2 * BORDER) + BORDER];
        assert!(
            extreme.iter().all(|&pixel| pixel == bottom_left),
            "the largest vectors read the corner, without overflow"
        );
    }
}
