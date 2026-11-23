// Kernels over blocks of bytes and words, each a plain body and on aarch64 a NEON body the tests hold to it bit for bit: sums of absolute differences, transformed differences, filter taps, a block of bytes turned to lanes of its columns and back, the pixel words of a row's light and colours, a picture of words, bytes or pairs turned, and the camera's packed codes of 10 bits unpacked.

// The crate's one `unsafe`: a NEON body is a function of the `neon` target feature, which plain code calls only in `unsafe`.
#![allow(unsafe_code)]

use super::bits::BYTE_BITS;
use super::pixel::PixelFormat;
use super::sampling::Turn;
use super::ycbcr::{Chroma, Colours};
use core::ops::Range;

// The lines summed between two looks at a sum's limit: a 4x4 block's height, by which the codecs' searches stop early.
const LOOK_LINES: usize = 4;
// Taps count 128ths and a sum is rounded by half of 128: VP8's six taps and two (RFC 6386, 18.3) and VP9's eight (8.5.2.4).
const TAP_BITS: u32 = 7;
const TAP_ROUNDING: i32 = 1 << (TAP_BITS - 1);
// The side of the blocks a Walsh-Hadamard transform takes.
const SIDE: usize = 4;
// The side of a block of bytes turned to columns: a vector's eight lanes.
pub(crate) const BLOCK: usize = 8;
// A blue and a red sample side by side.
pub(crate) const PAIR: usize = 2;
// Codes of 10 bits packed four to five bytes, lowest bits first, as the camera's frames lay them.
pub(crate) const CODE_BITS: usize = 10;
pub(crate) const CODE_MASK: u16 = (1 << CODE_BITS) - 1;
pub(crate) const PACKED_CODES: usize = 4;
pub(crate) const PACKED_BYTES: usize = PACKED_CODES * CODE_BITS / BYTE_BITS;

// The sum of the absolute differences of two blocks of `size`; once it reaches `limit` after a group of `LOOK_LINES` lines it is summed no further.
#[inline]
pub(crate) fn sad(a: (&[u8], usize), b: (&[u8], usize), size: (usize, usize), limit: u32) -> u32 {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    if neon::takes_sum(size) {
        // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
        return unsafe { neon::sad(a, b, size, limit) };
    }
    plain::sad(a, b, size, limit)
}

// As `sad`, against the mean of blocks `b` and `c`, rounded up.
#[inline]
pub(crate) fn sad_mean(
    a: (&[u8], usize),
    b: (&[u8], usize),
    c: (&[u8], usize),
    size: (usize, usize),
    limit: u32,
) -> u32 {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    if neon::takes_sum(size) {
        // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
        return unsafe { neon::sad_mean(a, b, c, size, limit) };
    }
    plain::sad_mean(a, b, c, size, limit)
}

// The sum of the absolute coefficients of the Walsh-Hadamard transform of each 4x4 block of `a` less `b`, over blocks of `size` in whole 4x4 blocks.
#[inline]
pub(crate) fn satd(a: (&[u8], usize), b: (&[u8], usize), size: (usize, usize)) -> u32 {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    if neon::takes_transform(size) {
        // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
        return unsafe { neon::satd(a, b, size) };
    }
    plain::satd(a, b, size)
}

// Fills a block of `size` of `out` with `samples` filtered by `taps`: each byte the samples from its place on, `step` apart, weighed by the taps in 128ths, rounded and held to a byte.
pub(crate) fn filter<const TAPS: usize>(
    samples: (&[u8], usize),
    step: usize,
    taps: &[i32; TAPS],
    out: (&mut [u8], usize),
    size: (usize, usize),
) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    if let Some(weights) = neon::takes_filter(size).then(|| neon::weights(taps)).flatten() {
        // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
        unsafe { neon::filter(samples, step, (taps, weights), out, size) };
        return;
    }
    plain::filter(samples, step, taps, out, size);
}

// The columns of the block of BLOCK lines of BLOCK bytes from the start of `plane`, lines `stride` apart, each a line of lanes: the loop filters' segments along lines, a segment to a lane.
#[inline]
pub(crate) fn columns(plane: &[u8], stride: usize) -> [[i16; BLOCK]; BLOCK] {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
    let columns = unsafe { neon::columns(plane, stride) };
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little")))]
    let columns = plain::columns(plane, stride);
    columns
}

// Puts lanes back as the columns of such a block, each held to a byte, at `places` of each line alone.
#[inline]
pub(crate) fn put_columns(columns: &[[i16; BLOCK]; BLOCK], block: (&mut [u8], usize), places: Range<usize>) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
    unsafe {
        neon::put_columns(columns, block, places);
    }
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little")))]
    plain::put_columns(columns, block, places);
}

// Each word of `out` by `colours` in `format`: a pixel's of its light and its blue and red where `chroma` lays them.
pub(crate) fn words(colours: &Colours, luma: &[u8], chroma: Chroma<'_>, format: PixelFormat, out: &mut [u32]) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    if let Some(places) = neon::places(format) {
        // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
        unsafe { neon::words(colours, luma, chroma, (format, places), out) };
        return;
    }
    plain::words(colours, luma, chroma, format, out);
}

// Each of `codes` of the packed codes from the start of `packed`, which begins a group of them.
pub(crate) fn unpack(packed: &[u8], codes: &mut [u16]) {
    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
    unsafe {
        neon::unpack(packed, codes);
    }
    #[cfg(not(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little")))]
    plain::unpack(packed, codes);
}

// What a turn moves in blocks in registers: a pixel's word, a sample, or a blue and a red sample side by side.
pub(crate) trait Sample: Copy {
    // Fills `out`, the whole lines of a picture `width` across, with that picture's samples as they stand once turned.
    fn turn(samples: &[Self], width: usize, turn: Turn, out: &mut [Self]);
}

impl Sample for u32 {
    fn turn(words: &[u32], width: usize, turn: Turn, out: &mut [u32]) {
        #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
        if turn.is_sideways() {
            // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
            unsafe { neon::turn_words(words, width, turn, out) };
            return;
        }
        plain::turn(words, width, turn, out);
    }
}

impl Sample for u8 {
    fn turn(bytes: &[u8], width: usize, turn: Turn, out: &mut [u8]) {
        #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
        if turn.is_sideways() {
            // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
            unsafe { neon::turn_bytes(bytes, width, turn, out) };
            return;
        }
        plain::turn(bytes, width, turn, out);
    }
}

impl Sample for [u8; PAIR] {
    fn turn(pairs: &[[u8; PAIR]], width: usize, turn: Turn, out: &mut [[u8; PAIR]]) {
        #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
        if turn.is_sideways() {
            // SAFETY: every aarch64 target this is built for has NEON (`target_feature = "neon"`), so its instructions run wherever this does.
            unsafe { neon::turn_pairs(pairs, width, turn, out) };
            return;
        }
        plain::turn(pairs, width, turn, out);
    }
}

// A turn's walk back over a picture `width` across and `height` down: the samples of a line once turned, the sample the turned picture starts from, and the steps to a line's next sample and to the next line.
fn walk(turn: Turn, (width, height): (usize, usize)) -> (usize, (isize, isize, isize)) {
    let ((x, y), along, down) = turn.walk_back(width, height);
    let stride = width as isize;
    let across = if turn.is_sideways() { height } else { width };
    (across, (x + y * stride, along.0 + along.1 * stride, down.0 + down.1 * stride))
}

// The plain bodies: what other targets build, and what the tests and the bench hold the NEON bodies to.
pub(crate) mod plain {
    use super::{BLOCK, LOOK_LINES, PAIR, SIDE, TAP_BITS, TAP_ROUNDING};
    use super::{CODE_BITS, CODE_MASK, PACKED_BYTES, PACKED_CODES};
    use super::{Chroma, Colours, PixelFormat, Turn};
    use core::ops::Range;

    // The codes of a group of packed bytes.
    fn group_codes(bytes: &[u8; PACKED_BYTES]) -> [u16; PACKED_CODES] {
        let mut word = [0u8; size_of::<u64>()];
        word[..PACKED_BYTES].copy_from_slice(bytes);
        let word = u64::from_le_bytes(word);
        core::array::from_fn(|at| (word >> (at * CODE_BITS)) as u16 & CODE_MASK)
    }

    pub(crate) fn unpack(packed: &[u8], codes: &mut [u16]) {
        let (groups, rest) = codes.as_chunks_mut::<PACKED_CODES>();
        let (whole, part) = packed.as_chunks::<PACKED_BYTES>();
        for (group, bytes) in groups.iter_mut().zip(whole) {
            *group = group_codes(bytes);
        }
        if !rest.is_empty() {
            // The codes of a group a line ends inside of, whose bytes may stop short of a whole group.
            let tail = whole.get(groups.len()).map_or(part, |bytes| &bytes[..]);
            let mut last = [0u8; PACKED_BYTES];
            last[..tail.len()].copy_from_slice(tail);
            rest.copy_from_slice(&group_codes(&last)[..rest.len()]);
        }
    }

    #[inline]
    pub(crate) fn columns(plane: &[u8], stride: usize) -> [[i16; BLOCK]; BLOCK] {
        core::array::from_fn(|column| core::array::from_fn(|line| i16::from(plane[line * stride + column])))
    }

    #[inline]
    pub(crate) fn put_columns(
        columns: &[[i16; BLOCK]; BLOCK],
        (plane, stride): (&mut [u8], usize),
        places: Range<usize>,
    ) {
        for line in 0..BLOCK {
            for place in places.clone() {
                plane[line * stride + place] = columns[place][line].clamp(0, 255) as u8;
            }
        }
    }

    #[inline]
    pub(crate) fn sad(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (width, height): (usize, usize),
        limit: u32,
    ) -> u32 {
        let mut sum = 0;
        for line in 0..height {
            let (first, second) = (&a[line * a_stride..][..width], &b[line * b_stride..][..width]);
            sum += first.iter().zip(second).map(|(&p, &q)| u32::from(p.abs_diff(q))).sum::<u32>();
            if line % LOOK_LINES == LOOK_LINES - 1 && sum >= limit {
                break;
            }
        }
        sum
    }

    #[inline]
    pub(crate) fn sad_mean(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (c, c_stride): (&[u8], usize),
        (width, height): (usize, usize),
        limit: u32,
    ) -> u32 {
        let mut sum = 0;
        for line in 0..height {
            let first = &a[line * a_stride..][..width];
            let (second, third) = (&b[line * b_stride..][..width], &c[line * c_stride..][..width]);
            let means = second.iter().zip(third).map(|(&p, &q)| ((u16::from(p) + u16::from(q) + 1) >> 1) as u8);
            sum += first.iter().zip(means).map(|(&p, q)| u32::from(p.abs_diff(q))).sum::<u32>();
            if line % LOOK_LINES == LOOK_LINES - 1 && sum >= limit {
                break;
            }
        }
        sum
    }

    #[inline]
    pub(crate) fn satd(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (width, height): (usize, usize),
    ) -> u32 {
        let mut sum = 0;
        for down in (0..height).step_by(SIDE) {
            for across in (0..width).step_by(SIDE) {
                let [d0, d1, d2, d3]: [[i32; SIDE]; SIDE] = core::array::from_fn(|line| {
                    let first = &a[(down + line) * a_stride + across..][..SIDE];
                    let second = &b[(down + line) * b_stride + across..][..SIDE];
                    core::array::from_fn(|x| i32::from(first[x]) - i32::from(second[x]))
                });
                // Down each column first, then along each line.
                let columns: [[i32; SIDE]; SIDE] = core::array::from_fn(|x| {
                    let (p, q, r, s) = (d0[x] + d1[x], d2[x] + d3[x], d0[x] - d1[x], d2[x] - d3[x]);
                    [p + q, p - q, r - s, r + s]
                });
                for line in 0..SIDE {
                    let [p, q, r, s] = columns.map(|column| column[line]);
                    let (e, f, g, h) = (p + q, r + s, p - q, r - s);
                    sum += ((e + f).abs() + (e - f).abs() + (g - h).abs() + (g + h).abs()) as u32;
                }
            }
        }
        sum
    }

    pub(crate) fn filter<const TAPS: usize>(
        (samples, stride): (&[u8], usize),
        step: usize,
        taps: &[i32; TAPS],
        (out, out_stride): (&mut [u8], usize),
        (width, height): (usize, usize),
    ) {
        for line in 0..height {
            let lines: [&[u8]; TAPS] = core::array::from_fn(|tap| &samples[line * stride + tap * step..][..width]);
            for (x, pixel) in out[line * out_stride..][..width].iter_mut().enumerate() {
                *pixel = round(lines.iter().zip(taps).map(|(line, &weight)| i32::from(line[x]) * weight).sum());
            }
        }
    }

    // One byte of `filter`: the samples from the first on, `step` apart, weighed by the taps; for a filter whose taps change from byte to byte.
    #[inline]
    pub(crate) fn tap<const TAPS: usize>(samples: &[u8], step: usize, taps: &[i32; TAPS]) -> u8 {
        round(taps.iter().enumerate().map(|(tap, &weight)| i32::from(samples[tap * step]) * weight).sum())
    }

    #[inline]
    fn round(sum: i32) -> u8 {
        ((sum + TAP_ROUNDING) >> TAP_BITS).clamp(0, 255) as u8
    }

    pub(crate) fn words(colours: &Colours, luma: &[u8], chroma: Chroma<'_>, format: PixelFormat, out: &mut [u32]) {
        let word = |light: u8, blue: u8, red: u8| colours.word(light.into(), blue.into(), red.into(), format);
        let pixels = out.len();
        let lights = &luma[..pixels];
        match chroma {
            Chroma::Apart(blue, red) => {
                let samples = lights.iter().zip(&blue[..pixels]).zip(&red[..pixels]);
                for (out, ((&light, &blue), &red)) in out.iter_mut().zip(samples) {
                    *out = word(light, blue, red);
                }
            }
            Chroma::Beside(pairs) => {
                let pairs = pairs[..PAIR * pixels].as_chunks::<PAIR>().0;
                for ((out, &light), &[blue, red]) in out.iter_mut().zip(lights).zip(pairs) {
                    *out = word(light, blue, red);
                }
            }
            Chroma::Shared(pairs) => {
                let pairs = pairs[..pixels.next_multiple_of(PAIR)].as_chunks::<PAIR>().0;
                let (outs, last) = out.as_chunks_mut::<PAIR>();
                for ((outs, lights), &[blue, red]) in outs.iter_mut().zip(lights.as_chunks::<PAIR>().0).zip(pairs) {
                    *outs = lights.map(|light| word(light, blue, red));
                }
                // A row of an odd width ends in a pixel whose pair is its own.
                if let ([out], Some(&[blue, red])) = (last, pairs.last()) {
                    *out = word(lights[pixels - 1], blue, red);
                }
            }
        }
    }

    pub(crate) fn turn<T: Copy>(samples: &[T], width: usize, turn: Turn, out: &mut [T]) {
        let height = out.len().checked_div(width).unwrap_or(0);
        let (across, walk) = super::walk(turn, (width, height));
        place(samples, walk, (out, across), 0..across);
    }

    // Samples `us` of each line of `out`, lines `across` long, as a walk reads them from `samples`: a run of one line forward or back, or a sample of each.
    pub(super) fn place<T: Copy>(
        samples: &[T],
        (start, step, next): (isize, isize, isize),
        (out, across): (&mut [T], usize),
        us: Range<usize>,
    ) {
        if us.is_empty() {
            return;
        }
        for (v, line) in out.chunks_exact_mut(across).enumerate() {
            let first = start + v as isize * next + us.start as isize * step;
            let line = &mut line[us.clone()];
            match step {
                1 => line.copy_from_slice(&samples[first as usize..][..line.len()]),
                // A copy turned end to end: the compiler makes vector loops of both, and none of a zip with a reversed iterator.
                -1 => {
                    line.copy_from_slice(&samples[first as usize + 1 - line.len()..=first as usize]);
                    line.reverse();
                }
                _ => {
                    for (u, sample) in line.iter_mut().enumerate() {
                        *sample = samples[(first + u as isize * step) as usize];
                    }
                }
            }
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
mod neon {
    use super::{BYTE_BITS, CODE_MASK, PACKED_BYTES};
    use super::{Chroma, Colours, PixelFormat, Range, Turn};
    use super::{LOOK_LINES, PAIR, SIDE, TAP_BITS};
    use crate::base::ycbcr::{CHROMA_MIDDLE, COLOUR_BITS};
    use core::arch::aarch64::*;

    // The bytes of a vector of eight lanes, the width these bodies step by.
    const LANES: usize = 8;
    // The words of a vector of four lanes, the side of a block of words a turn moves in registers, and its bytes.
    const WORD_LANES: usize = 4;
    const VECTOR_BYTES: usize = WORD_LANES * size_of::<u32>();
    // A place of a table's byte past its end, where the table gives nought.
    const NONE: u8 = u8::MAX;
    // A line of the A53's data cache (64 bytes, Cortex-A53 TRM): a turn makes as many lines together as it holds samples, so that each run a line of the picture gives is read whole once.
    const CACHE_LINE: usize = 64;
    // A sum's lanes are 16 bits: a group of LOOK_LINES lines no wider than this keeps each under 65536.
    const MOST_WIDTH: usize = u16::MAX as usize / (LOOK_LINES * u8::MAX as usize) * LANES;
    // The weights of either sign, each a byte, summed to no more than keeps a lane of products under 65536.
    const MOST_WEIGHTS: u32 = u16::MAX as u32 / u8::MAX as u32;
    // The bytes of a line the filter sums tap after tap: the widest block a codec predicts, VP9's 64, so that each line of one is one stripe.
    const STRIPE: usize = 64;
    pub(super) const fn takes_sum((width, height): (usize, usize)) -> bool {
        width.is_multiple_of(LANES) && width <= MOST_WIDTH && height.is_multiple_of(LOOK_LINES)
    }

    pub(super) const fn takes_transform((width, height): (usize, usize)) -> bool {
        width.is_multiple_of(SIDE) && height.is_multiple_of(SIDE)
    }

    // A block narrower than a vector is the plain body's whole.
    pub(super) const fn takes_filter((width, _): (usize, usize)) -> bool {
        width >= LANES
    }

    // The taps as the weights of their positive and of their negative parts, where each is a byte and neither part's sum can pass a lane.
    pub(super) fn weights<const TAPS: usize>(taps: &[i32; TAPS]) -> Option<([u8; TAPS], [u8; TAPS])> {
        let (mut plus, mut minus) = ([0u8; TAPS], [0u8; TAPS]);
        for ((&tap, plus), minus) in taps.iter().zip(&mut plus).zip(&mut minus) {
            let weight = u8::try_from(tap.unsigned_abs()).ok()?;
            if tap > 0 {
                *plus = weight;
            } else {
                *minus = weight;
            }
        }
        let sum = |weights: &[u8; TAPS]| weights.iter().map(|&weight| u32::from(weight)).sum::<u32>();
        (sum(&plus) <= MOST_WEIGHTS && sum(&minus) <= MOST_WEIGHTS).then_some((plus, minus))
    }

    // Eight bytes as a vector's lanes, the first in the lowest.
    #[target_feature(enable = "neon")]
    fn load(bytes: &[u8]) -> uint8x8_t {
        let mut word = [0u8; LANES];
        word.copy_from_slice(&bytes[..LANES]);
        vcreate_u8(u64::from_le_bytes(word))
    }

    #[target_feature(enable = "neon")]
    fn store(vector: uint8x8_t, bytes: &mut [u8]) {
        bytes[..LANES].copy_from_slice(&bytes_of(vector));
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub(super) fn sad(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (width, height): (usize, usize),
        limit: u32,
    ) -> u32 {
        let mut sum = 0;
        for group in (0..height).step_by(LOOK_LINES) {
            let mut lanes = vdupq_n_u16(0);
            for line in group..group + LOOK_LINES {
                let (first, second) = (&a[line * a_stride..][..width], &b[line * b_stride..][..width]);
                for (p, q) in first.as_chunks::<LANES>().0.iter().zip(second.as_chunks::<LANES>().0) {
                    lanes = vabal_u8(lanes, load(p), load(q));
                }
            }
            sum += vaddlvq_u16(lanes);
            if sum >= limit {
                break;
            }
        }
        sum
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub(super) fn sad_mean(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (c, c_stride): (&[u8], usize),
        (width, height): (usize, usize),
        limit: u32,
    ) -> u32 {
        let mut sum = 0;
        for group in (0..height).step_by(LOOK_LINES) {
            let mut lanes = vdupq_n_u16(0);
            for line in group..group + LOOK_LINES {
                let first = &a[line * a_stride..][..width];
                let (second, third) = (&b[line * b_stride..][..width], &c[line * c_stride..][..width]);
                let [first, second, third] = [first, second, third].map(|line| line.as_chunks::<LANES>().0);
                for ((p, q), r) in first.iter().zip(second).zip(third) {
                    lanes = vabal_u8(lanes, load(p), vrhadd_u8(load(q), load(r)));
                }
            }
            sum += vaddlvq_u16(lanes);
            if sum >= limit {
                break;
            }
        }
        sum
    }

    // The differences of a line of eight pixels of `a` and `b`, as signed lanes.
    #[target_feature(enable = "neon")]
    fn difference(a: &[u8], b: &[u8]) -> int16x8_t {
        vreinterpretq_s16_u16(vsubl_u8(load(a), load(b)))
    }

    // The differences of two lines of four pixels of `a` and `b`, a line `stride` from the other: the first line's in the lower four lanes.
    #[target_feature(enable = "neon")]
    fn two_differences((a, a_stride): (&[u8], usize), (b, b_stride): (&[u8], usize)) -> int16x8_t {
        let two = |bytes: &[u8], stride: usize| {
            let mut word = [0u8; LANES];
            word[..SIDE].copy_from_slice(&bytes[..SIDE]);
            word[SIDE..].copy_from_slice(&bytes[stride..stride + SIDE]);
            vcreate_u8(u64::from_le_bytes(word))
        };
        vreinterpretq_s16_u16(vsubl_u8(two(a, a_stride), two(b, b_stride)))
    }

    // Adds to `sums` the halves of the transformed differences of a pair of lines after the butterflies down the columns, each lane a column: two blocks' lines side by side, or two lines of a block one after the other.
    #[target_feature(enable = "neon")]
    fn add_lines(sums: uint32x4_t, upper: int16x8_t, lower: int16x8_t) -> uint32x4_t {
        // Along each line |x + y| + |x - y| is twice the larger of |x| and |y|: the last butterfly is a pairwise maximum.
        let (even, odd) = (vuzp1q_s16(upper, lower), vuzp2q_s16(upper, lower));
        let larger = vpmaxq_s16(vabsq_s16(vaddq_s16(even, odd)), vabsq_s16(vsubq_s16(even, odd)));
        vpadalq_u16(sums, vreinterpretq_u16_s16(larger))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub(super) fn satd(
        (a, a_stride): (&[u8], usize),
        (b, b_stride): (&[u8], usize),
        (width, height): (usize, usize),
    ) -> u32 {
        let mut sums = vdupq_n_u32(0);
        let whole = width - width % LANES;
        for down in (0..height).step_by(SIDE) {
            let line = |at: usize, across: usize| {
                (&a[(down + at) * a_stride + across..], &b[(down + at) * b_stride + across..])
            };
            for across in (0..whole).step_by(LANES) {
                let ((a0, b0), (a1, b1), (a2, b2), (a3, b3)) =
                    (line(0, across), line(1, across), line(2, across), line(3, across));
                let (d0, d1, d2, d3) = (difference(a0, b0), difference(a1, b1), difference(a2, b2), difference(a3, b3));
                let (p, q, r, s) = (vaddq_s16(d0, d1), vaddq_s16(d2, d3), vsubq_s16(d0, d1), vsubq_s16(d2, d3));
                sums = add_lines(sums, vaddq_s16(p, q), vsubq_s16(p, q));
                sums = add_lines(sums, vsubq_s16(r, s), vaddq_s16(r, s));
            }
            if whole < width {
                // A block of four across, two of its lines to a vector.
                let ((a0, b0), (a2, b2)) = (line(0, whole), line(2, whole));
                let (upper, lower) =
                    (two_differences((a0, a_stride), (b0, b_stride)), two_differences((a2, a_stride), (b2, b_stride)));
                let (d0, d1, d2, d3) =
                    (vget_low_s16(upper), vget_high_s16(upper), vget_low_s16(lower), vget_high_s16(lower));
                let (p, q, r, s) = (vadd_s16(d0, d1), vadd_s16(d2, d3), vsub_s16(d0, d1), vsub_s16(d2, d3));
                let first = vcombine_s16(vadd_s16(p, q), vsub_s16(p, q));
                sums = add_lines(sums, first, vcombine_s16(vsub_s16(r, s), vadd_s16(r, s)));
            }
        }
        2 * vaddvq_u32(sums)
    }

    #[target_feature(enable = "neon")]
    pub(super) fn filter<const TAPS: usize>(
        (samples, stride): (&[u8], usize),
        step: usize,
        (taps, (plus, minus)): (&[i32; TAPS], ([u8; TAPS], [u8; TAPS])),
        (out, out_stride): (&mut [u8], usize),
        (width, height): (usize, usize),
    ) {
        let whole = width - width % LANES;
        for line in 0..height {
            for x in (0..whole).step_by(STRIPE) {
                let stripe = STRIPE.min(whole - x);
                let (mut sums, mut lesses) = ([vdupq_n_u16(0); STRIPE / LANES], [vdupq_n_u16(0); STRIPE / LANES]);
                for (tap, (&plus, &minus)) in plus.iter().zip(&minus).enumerate() {
                    let chunks = samples[line * stride + x + tap * step..][..stripe].as_chunks::<LANES>().0;
                    if plus > 0 {
                        for (sum, chunk) in sums.iter_mut().zip(chunks) {
                            *sum = vmlal_u8(*sum, load(chunk), vdup_n_u8(plus));
                        }
                    }
                    if minus > 0 {
                        for (less, chunk) in lesses.iter_mut().zip(chunks) {
                            *less = vmlal_u8(*less, load(chunk), vdup_n_u8(minus));
                        }
                    }
                }
                let row = out[line * out_stride + x..][..stripe].as_chunks_mut::<LANES>().0;
                for ((chunk, &sum), &less) in row.iter_mut().zip(&sums).zip(&lesses) {
                    // Below nought the plain body holds a byte at nought too, and past 255 at 255.
                    store(vqrshrn_n_u16::<{ TAP_BITS as i32 }>(vqsubq_u16(sum, less)), chunk);
                }
            }
        }
        if whole < width {
            let rest = (width - whole, height);
            super::plain::filter((&samples[whole..], stride), step, taps, (&mut out[whole..], out_stride), rest);
        }
    }

    // Where each byte of four words lies in a table of eight reds, eight greens and eight blues, by the pixels of the words: where every channel's shift is a whole byte of a word, each its own.
    pub(super) fn places(format: PixelFormat) -> Option<[[u8; VECTOR_BYTES]; 2]> {
        let shifts = [format.r_shift, format.g_shift, format.b_shift].map(usize::from);
        let byte = BYTE_BITS;
        let is_bytes = shifts.iter().all(|&shift| shift.is_multiple_of(byte) && shift < u32::BITS as usize);
        let is_apart = shifts[0] != shifts[1] && shifts[1] != shifts[2] && shifts[2] != shifts[0];
        let place = |half: usize, at: usize| {
            let (pixel, shift) = (half * WORD_LANES + at / size_of::<u32>(), at % size_of::<u32>() * byte);
            shifts.iter().position(|&own| own == shift).map_or(NONE, |channel| (channel * LANES + pixel) as u8)
        };
        (is_bytes && is_apart).then(|| core::array::from_fn(|half| core::array::from_fn(|at| place(half, at))))
    }

    // The words of eight pixels of their lights, blues and reds into `out`, each byte put where `places` has it.
    #[inline]
    #[target_feature(enable = "neon")]
    fn pixel_words(
        colours: &Colours,
        (lights, blues, reds): (uint8x8_t, uint8x8_t, uint8x8_t),
        places: &[uint8x16_t; 2],
        out: &mut [u32; LANES],
    ) {
        let light = vreinterpretq_s16_u16(vsubl_u8(lights, vdup_n_u8(colours.floor as u8)));
        let middle = vdup_n_u8(CHROMA_MIDDLE as u8);
        let (blue, red) =
            (vreinterpretq_s16_u16(vsubl_u8(blues, middle)), vreinterpretq_s16_u16(vsubl_u8(reds, middle)));
        let half = vdupq_n_s32(1 << (COLOUR_BITS - 1));
        // A channel's sums in lanes of 32 bits as the plain body's, their whole parts narrowed to 16 bits, which hold each.
        let codes = |light: int16x4_t, blue: int16x4_t, red: int16x4_t| {
            let light = vmlaq_n_s32(half, vmovl_s16(light), colours.luma);
            let (blue, red) = (vmovl_s16(blue), vmovl_s16(red));
            let green = vmlsq_n_s32(vmlsq_n_s32(light, blue, colours.green_by_blue), red, colours.green_by_red);
            [
                vshrn_n_s32::<{ COLOUR_BITS as i32 }>(vmlaq_n_s32(light, red, colours.red)),
                vshrn_n_s32::<{ COLOUR_BITS as i32 }>(green),
                vshrn_n_s32::<{ COLOUR_BITS as i32 }>(vmlaq_n_s32(light, blue, colours.blue)),
            ]
        };
        let low = codes(vget_low_s16(light), vget_low_s16(blue), vget_low_s16(red));
        let high = codes(vget_high_s16(light), vget_high_s16(blue), vget_high_s16(red));
        // Held to a byte as the plain body holds a code: below nought at nought, past 255 at 255.
        let [r, g, b] = core::array::from_fn(|channel| vqmovun_s16(vcombine_s16(low[channel], high[channel])));
        let table = uint8x16x2_t(vcombine_u8(r, g), vcombine_u8(b, vdup_n_u8(0)));
        let (first, second) = out.split_at_mut(WORD_LANES);
        first.copy_from_slice(&words_of(vreinterpretq_u32_u8(vqtbl2q_u8(table, places[0]))));
        second.copy_from_slice(&words_of(vreinterpretq_u32_u8(vqtbl2q_u8(table, places[1]))));
    }

    #[target_feature(enable = "neon")]
    pub(super) fn words(
        colours: &Colours,
        luma: &[u8],
        chroma: Chroma<'_>,
        (format, places): (PixelFormat, [[u8; VECTOR_BYTES]; 2]),
        out: &mut [u32],
    ) {
        let places = places.map(|bytes| vcombine_u8(load(&bytes), load(&bytes[LANES..])));
        let whole = out.len() - out.len() % LANES;
        let (lights, outs) = (luma[..whole].as_chunks::<LANES>().0, out[..whole].as_chunks_mut::<LANES>().0);
        let rest = match chroma {
            Chroma::Apart(blue, red) => {
                let samples = blue[..whole].as_chunks::<LANES>().0.iter().zip(red[..whole].as_chunks::<LANES>().0);
                for ((out, light), (blue, red)) in outs.iter_mut().zip(lights).zip(samples) {
                    pixel_words(colours, (load(light), load(blue), load(red)), &places, out);
                }
                Chroma::Apart(&blue[whole..], &red[whole..])
            }
            Chroma::Beside(pairs) => {
                for ((out, light), pairs) in
                    outs.iter_mut().zip(lights).zip(pairs[..PAIR * whole].as_chunks::<{ PAIR * LANES }>().0)
                {
                    let (first, second) = (load(pairs), load(&pairs[LANES..]));
                    let samples = (load(light), vuzp1_u8(first, second), vuzp2_u8(first, second));
                    pixel_words(colours, samples, &places, out);
                }
                Chroma::Beside(&pairs[PAIR * whole..])
            }
            Chroma::Shared(pairs) => {
                // Four pairs serve eight pixels, each blue and red two.
                for ((out, light), pairs) in outs.iter_mut().zip(lights).zip(pairs[..whole].as_chunks::<LANES>().0) {
                    let pairs = load(pairs);
                    let (blues, reds) = (vuzp1_u8(pairs, pairs), vuzp2_u8(pairs, pairs));
                    pixel_words(colours, (load(light), vzip1_u8(blues, blues), vzip1_u8(reds, reds)), &places, out);
                }
                Chroma::Shared(&pairs[whole..])
            }
        };
        super::plain::words(colours, &luma[whole..], rest, format, &mut out[whole..]);
    }

    // Eight codes' places in the ten bytes that hold them: the two bytes each code begins in, and the bits it is shifted up there, as negative shifts to take it down.
    const CODE_PAIRS: [u8; 2 * LANES] = [0, 1, 1, 2, 2, 3, 3, 4, 5, 6, 6, 7, 7, 8, 8, 9];
    const CODE_SHIFTS: [i16; LANES] = [0, -2, -4, -6, 0, -2, -4, -6];
    // The bytes eight codes are packed in; sixteen codes are made at a time, two runs of eight beside each other so that the A53, which runs in order, waits on neither, from the bytes read at once for them.
    const EIGHT_BYTES: usize = 2 * PACKED_BYTES;
    const SIXTEEN: usize = 2 * LANES;
    const READ_BYTES: usize = EIGHT_BYTES + 2 * LANES;

    #[target_feature(enable = "neon")]
    pub(super) fn unpack(packed: &[u8], codes: &mut [u16]) {
        let pairs = vcombine_u8(load(&CODE_PAIRS), load(&CODE_PAIRS[LANES..]));
        let shift =
            |half: &[i16]| vcreate_s16(half.iter().rev().fold(0, |word, &bits| word << 16 | u64::from(bits as u16)));
        let shifts = vcombine_s16(shift(&CODE_SHIFTS[..WORD_LANES]), shift(&CODE_SHIFTS[WORD_LANES..]));
        let mask = vdupq_n_u16(CODE_MASK);
        // Eight codes, taken out of their vector a 64-bit half at a time.
        let eight = |bytes: &[u8]| {
            let lanes = vqtbl1q_u8(vcombine_u8(load(bytes), load(&bytes[LANES..])), pairs);
            let codes = vandq_u16(vshlq_u16(vreinterpretq_u16_u8(lanes), shifts), mask);
            let halves = vreinterpretq_u64_u16(codes);
            let halves = [vgetq_lane_u64::<0>(halves), vgetq_lane_u64::<1>(halves)];
            let code = |at: usize| (halves[at / WORD_LANES] >> (u16::BITS as usize * (at % WORD_LANES))) as u16;
            core::array::from_fn::<u16, LANES, _>(code)
        };
        let mut done = 0;
        for (out, bytes) in
            codes.as_chunks_mut::<SIXTEEN>().0.iter_mut().zip(packed.windows(READ_BYTES).step_by(2 * EIGHT_BYTES))
        {
            let (first, second) = (eight(bytes), eight(&bytes[EIGHT_BYTES..]));
            let (low, high) = out.split_at_mut(LANES);
            low.copy_from_slice(&first);
            high.copy_from_slice(&second);
            done += SIXTEEN;
        }
        super::plain::unpack(&packed[done / LANES * EIGHT_BYTES..], &mut codes[done..]);
    }

    // Four words as a vector's lanes, the first in the lowest.
    #[target_feature(enable = "neon")]
    fn load_words(words: &[u32]) -> uint32x4_t {
        let mut lanes = [0u32; WORD_LANES];
        lanes.copy_from_slice(&words[..WORD_LANES]);
        let pair = |low: u32, high: u32| vcreate_u32(u64::from(low) | u64::from(high) << u32::BITS);
        vcombine_u32(pair(lanes[0], lanes[1]), pair(lanes[2], lanes[3]))
    }

    #[target_feature(enable = "neon")]
    fn words_of(lanes: uint32x4_t) -> [u32; WORD_LANES] {
        [vgetq_lane_u32::<0>(lanes), vgetq_lane_u32::<1>(lanes), vgetq_lane_u32::<2>(lanes), vgetq_lane_u32::<3>(lanes)]
    }

    #[target_feature(enable = "neon")]
    fn bytes_of(lanes: uint8x8_t) -> [u8; LANES] {
        vget_lane_u64::<0>(vreinterpret_u64_u8(lanes)).to_le_bytes()
    }

    // Eight pairs as a vector's lanes of 16 bits, the first in the lowest.
    #[target_feature(enable = "neon")]
    fn load_pairs(pairs: &[[u8; PAIR]]) -> uint16x8_t {
        let bytes = pairs[..LANES].as_flattened();
        vreinterpretq_u16_u8(vcombine_u8(load(bytes), load(&bytes[LANES..])))
    }

    #[target_feature(enable = "neon")]
    fn pairs_of(lanes: uint16x8_t) -> [[u8; PAIR]; LANES] {
        let (bytes, mut pairs) = (vreinterpretq_u8_u16(lanes), [[0; PAIR]; LANES]);
        let (low, high) = pairs.as_flattened_mut().split_at_mut(LANES);
        low.copy_from_slice(&bytes_of(vget_low_u8(bytes)));
        high.copy_from_slice(&bytes_of(vget_high_u8(bytes)));
        pairs
    }

    // A block of four lines of four words, each column made a line.
    #[target_feature(enable = "neon")]
    fn transpose_words([a, b, c, d]: [uint32x4_t; WORD_LANES]) -> [uint32x4_t; WORD_LANES] {
        let (upper_even, upper_odd) =
            (vreinterpretq_u64_u32(vtrn1q_u32(a, b)), vreinterpretq_u64_u32(vtrn2q_u32(a, b)));
        let (lower_even, lower_odd) =
            (vreinterpretq_u64_u32(vtrn1q_u32(c, d)), vreinterpretq_u64_u32(vtrn2q_u32(c, d)));
        [
            vreinterpretq_u32_u64(vtrn1q_u64(upper_even, lower_even)),
            vreinterpretq_u32_u64(vtrn1q_u64(upper_odd, lower_odd)),
            vreinterpretq_u32_u64(vtrn2q_u64(upper_even, lower_even)),
            vreinterpretq_u32_u64(vtrn2q_u64(upper_odd, lower_odd)),
        ]
    }

    // A block of eight lines of eight bytes, each column made a line: bytes crossed between lines, then pairs of them, then fours.
    #[target_feature(enable = "neon")]
    fn transpose_bytes([a, b, c, d, e, f, g, h]: [uint8x8_t; LANES]) -> [uint8x8_t; LANES] {
        let ones = |first: uint8x8_t, second: uint8x8_t| {
            (vreinterpret_u16_u8(vtrn1_u8(first, second)), vreinterpret_u16_u8(vtrn2_u8(first, second)))
        };
        let ((ab_even, ab_odd), (cd_even, cd_odd)) = (ones(a, b), ones(c, d));
        let ((ef_even, ef_odd), (gh_even, gh_odd)) = (ones(e, f), ones(g, h));
        let twos = |first: uint16x4_t, second: uint16x4_t| {
            (vreinterpret_u32_u16(vtrn1_u16(first, second)), vreinterpret_u32_u16(vtrn2_u16(first, second)))
        };
        let ((upper_0, upper_2), (upper_1, upper_3)) = (twos(ab_even, cd_even), twos(ab_odd, cd_odd));
        let ((lower_0, lower_2), (lower_1, lower_3)) = (twos(ef_even, gh_even), twos(ef_odd, gh_odd));
        let fours = |upper: uint32x2_t, lower: uint32x2_t| {
            [vreinterpret_u8_u32(vtrn1_u32(upper, lower)), vreinterpret_u8_u32(vtrn2_u32(upper, lower))]
        };
        let ([c0, c4], [c1, c5]) = (fours(upper_0, lower_0), fours(upper_1, lower_1));
        let ([c2, c6], [c3, c7]) = (fours(upper_2, lower_2), fours(upper_3, lower_3));
        [c0, c1, c2, c3, c4, c5, c6, c7]
    }

    // A block of eight lines of eight pairs, each column made a line, crossed as `transpose_bytes` crosses bytes but a pair to a lane.
    #[target_feature(enable = "neon")]
    fn transpose_pairs([a, b, c, d, e, f, g, h]: [uint16x8_t; LANES]) -> [uint16x8_t; LANES] {
        let ones = |first: uint16x8_t, second: uint16x8_t| {
            (vreinterpretq_u32_u16(vtrn1q_u16(first, second)), vreinterpretq_u32_u16(vtrn2q_u16(first, second)))
        };
        let ((ab_even, ab_odd), (cd_even, cd_odd)) = (ones(a, b), ones(c, d));
        let ((ef_even, ef_odd), (gh_even, gh_odd)) = (ones(e, f), ones(g, h));
        let twos = |first: uint32x4_t, second: uint32x4_t| {
            (vreinterpretq_u64_u32(vtrn1q_u32(first, second)), vreinterpretq_u64_u32(vtrn2q_u32(first, second)))
        };
        let ((upper_0, upper_2), (upper_1, upper_3)) = (twos(ab_even, cd_even), twos(ab_odd, cd_odd));
        let ((lower_0, lower_2), (lower_1, lower_3)) = (twos(ef_even, gh_even), twos(ef_odd, gh_odd));
        let fours = |upper: uint64x2_t, lower: uint64x2_t| {
            [vreinterpretq_u16_u64(vtrn1q_u64(upper, lower)), vreinterpretq_u16_u64(vtrn2q_u64(upper, lower))]
        };
        let ([c0, c4], [c1, c5]) = (fours(upper_0, lower_0), fours(upper_1, lower_1));
        let ([c2, c6], [c3, c7]) = (fours(upper_2, lower_2), fours(upper_3, lower_3));
        [c0, c1, c2, c3, c4, c5, c6, c7]
    }

    // A sideways turn, whose lines are the picture's columns: blocks of SIDE samples from SIDE of its lines, each turned in registers by `block`, a band of lines a cache line's samples wide at a time; the edges past whole blocks by the plain body.
    #[inline]
    #[target_feature(enable = "neon")]
    fn tiles<T: Copy, const SIDE: usize>(
        samples: &[T],
        width: usize,
        turn: Turn,
        out: &mut [T],
        block: impl Fn([&[T]; SIDE]) -> [[T; SIDE]; SIDE],
    ) {
        let height = out.len().checked_div(width).unwrap_or(0);
        let (across, (start, step, next)) = super::walk(turn, (width, height));
        let (whole_across, whole_lines) = (across - across % SIDE, width - width % SIDE);
        // SIDE turned lines' samples lie side by side in a line of the picture, the lowest place first: theirs in order where the next line's sample follows, from the last where it goes before.
        let back = if next < 0 { SIDE - 1 } else { 0 };
        let band = CACHE_LINE / size_of::<T>();
        for first in (0..whole_lines).step_by(band) {
            for u in (0..whole_across).step_by(SIDE) {
                // Where the block's first row starts and where its first turned line goes, each moved on by a block's lines.
                let mut source = start + u as isize * step + (first + back) as isize * next;
                let mut target = first * across + u;
                for _ in (first..whole_lines.min(first + band)).step_by(SIDE) {
                    let rows = core::array::from_fn(|row| &samples[(source + row as isize * step) as usize..]);
                    for (line, turned) in block(rows).iter().enumerate() {
                        let line = if next < 0 { SIDE - 1 - line } else { line };
                        out[target + line * across..][..SIDE].copy_from_slice(turned);
                    }
                    (source, target) = (source + (SIDE as isize) * next, target + SIDE * across);
                }
            }
        }
        let lines = whole_lines * across;
        super::plain::place(samples, (start, step, next), (&mut out[..lines], across), whole_across..across);
        let rest = (start + whole_lines as isize * next, step, next);
        super::plain::place(samples, rest, (&mut out[lines..], across), 0..across);
    }

    #[target_feature(enable = "neon")]
    pub(super) fn turn_words(words: &[u32], width: usize, turn: Turn, out: &mut [u32]) {
        tiles::<u32, WORD_LANES>(words, width, turn, out, |rows| {
            transpose_words(rows.map(|row| load_words(row))).map(|lanes| words_of(lanes))
        });
    }

    // Eight lanes of 16 bits as a vector's, the first in the lowest, a half of four at a time.
    #[inline]
    #[target_feature(enable = "neon")]
    fn load_lanes(lanes: &[i16; LANES]) -> int16x8_t {
        let half = |half: &[i16]| {
            vcreate_s16(half.iter().rev().fold(0, |word, &lane| word << u16::BITS | u64::from(lane as u16)))
        };
        vcombine_s16(half(&lanes[..WORD_LANES]), half(&lanes[WORD_LANES..]))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    fn lanes_of(vector: int16x8_t) -> [i16; LANES] {
        let halves = vreinterpretq_u64_s16(vector);
        let halves = [vgetq_lane_u64::<0>(halves), vgetq_lane_u64::<1>(halves)];
        core::array::from_fn(|at| (halves[at / WORD_LANES] >> (u16::BITS as usize * (at % WORD_LANES))) as i16)
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub(super) fn columns(plane: &[u8], stride: usize) -> [[i16; LANES]; LANES] {
        let lines = core::array::from_fn(|line| load(&plane[line * stride..]));
        transpose_bytes(lines).map(|column| lanes_of(vreinterpretq_s16_u16(vmovl_u8(column))))
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub(super) fn put_columns(
        columns: &[[i16; LANES]; LANES],
        (plane, stride): (&mut [u8], usize),
        places: Range<usize>,
    ) {
        // Below nought a value is held at nought, past 255 at 255, as the plain body holds it.
        let bytes = columns.map(|lanes| vqmovun_s16(load_lanes(&lanes)));
        for (line, turned) in transpose_bytes(bytes).into_iter().enumerate() {
            plane[line * stride..][places.clone()].copy_from_slice(&bytes_of(turned)[places.clone()]);
        }
    }

    #[target_feature(enable = "neon")]
    pub(super) fn turn_bytes(bytes: &[u8], width: usize, turn: Turn, out: &mut [u8]) {
        tiles::<u8, LANES>(bytes, width, turn, out, |rows| {
            transpose_bytes(rows.map(|row| load(row))).map(|lanes| bytes_of(lanes))
        });
    }

    #[target_feature(enable = "neon")]
    pub(super) fn turn_pairs(pairs: &[[u8; PAIR]], width: usize, turn: Turn, out: &mut [[u8; PAIR]]) {
        tiles::<[u8; PAIR], LANES>(pairs, width, turn, out, |rows| {
            transpose_pairs(rows.map(|row| load_pairs(row))).map(|lanes| pairs_of(lanes))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;
    use crate::base::ycbcr::{WEIGHTS_601, WEIGHTS_709};
    use alloc::vec;
    use alloc::vec::Vec;

    // The blocks' sizes the tests take: the codecs' 16, 8 and 4, and sizes the NEON bodies leave to the plain ones.
    const SIZES: [(usize, usize); 9] =
        [(16, 16), (8, 8), (4, 4), (16, 8), (8, 16), (64, 64), (24, 12), (12, 8), (16, 5)];
    // Room around a block, so that a block at an odd place of a plane is read.
    const MARGIN: usize = 13;

    // A plane of `count` bytes: random from `seed`, or of one value, or of the ends 0 and 255 in turn.
    fn plane(count: usize, seed: u32, kind: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..count)
            .map(|at| {
                state = xorshift32(state);
                match kind {
                    0 => state as u8,
                    1 => 255,
                    _ => [0, 255][(at + seed as usize) % 2],
                }
            })
            .collect()
    }

    #[test]
    fn sums_match_plain() {
        let mut cases = 0;
        for (index, &(width, height)) in SIZES.iter().enumerate() {
            let stride = width + 2 * MARGIN;
            let count = stride * (height + 2 * MARGIN);
            for kind in 0..3 {
                let (a, b, c) = (plane(count, 7 + index as u32, kind), plane(count, 91, 0), plane(count, 3, 2 - kind));
                for (at, limit) in [(0, u32::MAX), (MARGIN * stride + 3, 0), (1, 2000), (stride + 5, 40_000)] {
                    let blocks = ((&a[at..], stride), (&b[at + 1..], stride), (&c[at + stride..], stride));
                    let size = (width, height);
                    assert_eq!(
                        sad(blocks.0, blocks.1, size, limit),
                        plain::sad(blocks.0, blocks.1, size, limit),
                        "{size:?} of kind {kind} at {at} up to {limit}: the sum of absolute differences"
                    );
                    assert_eq!(
                        sad_mean(blocks.0, blocks.1, blocks.2, size, limit),
                        plain::sad_mean(blocks.0, blocks.1, blocks.2, size, limit),
                        "{size:?} of kind {kind} at {at} up to {limit}: against the rounded mean"
                    );
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, SIZES.len() * 12, "every size, kind and limit");
        let (flat, raised) = ([100u8; 256], [103u8; 256]);
        assert_eq!(sad((&flat, 16), (&raised, 16), (16, 16), u32::MAX), 768, "3 for each of 256 pixels");
        assert_eq!(sad((&flat, 16), (&raised, 16), (16, 16), 100), 192, "stopped after the first four lines");
    }

    #[test]
    fn transforms_match_plain() {
        for (index, &(width, height)) in SIZES.iter().enumerate().filter(|(_, size)| size.0 % 4 == 0 && size.1 % 4 == 0)
        {
            let stride = width + 2 * MARGIN;
            let count = stride * (height + 2 * MARGIN);
            for kind in 0..3 {
                let (a, b) = (plane(count, 5 + index as u32, kind), plane(count, 17, 2 - kind));
                for at in [0, MARGIN * stride + 1, 2 * stride + 7] {
                    let blocks = ((&a[at..], stride), (&b[at + 2..], stride));
                    assert_eq!(
                        satd(blocks.0, blocks.1, (width, height)),
                        plain::satd(blocks.0, blocks.1, (width, height)),
                        "{width}x{height} of kind {kind} at {at}: the transformed differences"
                    );
                }
            }
        }
        let board: [u8; 16] = core::array::from_fn(|at| if (at % 4 + at / 4) % 2 == 0 { 101 } else { 99 });
        assert_eq!(satd((&board, 4), (&[100; 16], 4), (4, 4)), 16, "a checkerboard of one is one coefficient of 16");
    }

    // Taps that sum to 128 as the codecs' do, random within the bounds of `weights` where `seed` is not nought.
    fn taps<const TAPS: usize>(seed: u32) -> [i32; TAPS] {
        let mut state = seed;
        let mut taps: [i32; TAPS] = core::array::from_fn(|_| {
            state = xorshift32(state);
            (state % 41) as i32 - 20
        });
        taps[TAPS / 2] = 0;
        taps[TAPS / 2] = 128 - taps.iter().sum::<i32>();
        taps
    }

    fn check_filter<const TAPS: usize>(taps: &[i32; TAPS], seed: u32) {
        for &(width, height) in &SIZES {
            for kind in 0..3 {
                let stride = width + 2 * MARGIN;
                let samples = plane(stride * (height + 2 * MARGIN), seed, kind);
                for step in [1, stride] {
                    let (mut fast, mut plain) = (alloc::vec![9u8; stride * height], alloc::vec![9u8; stride * height]);
                    filter((&samples[3..], stride), step, taps, (&mut fast[1..], stride), (width, height));
                    plain::filter((&samples[3..], stride), step, taps, (&mut plain[1..], stride), (width, height));
                    assert_eq!(fast, plain, "{width}x{height} of kind {kind}, step {step}, taps {taps:?}");
                }
            }
        }
    }

    #[test]
    fn filters_match_plain() {
        check_filter(&[0, 0, 128, 0, 0, 0], 1);
        check_filter(&[3, -16, 77, 77, -16, 3], 2);
        check_filter(&[2, -11, 108, 36, -8, 1], 3);
        check_filter(&[112, 16], 4);
        check_filter(&[-4, 11, -24, 90, 70, -21, 10, -4], 5);
        check_filter(&[-128, 255, -128, 129], 6);
        check_filter(&[300, -172], 7);
        for seed in 1..40 {
            check_filter(&taps::<6>(seed), seed);
            check_filter(&taps::<8>(seed), seed + 100);
        }
        let ramp: Vec<u8> = (0..32).map(|at| (at * 9) as u8).collect();
        let mut out = [0u8; 16];
        filter((&ramp, 32), 1, &[64, 64], (&mut out, 16), (16, 1));
        assert_eq!(out[..3], [5, 14, 23], "halfway between pixels 9 apart, rounded up");
    }

    #[test]
    fn columns_match_plain() {
        let stride = BLOCK + 2 * MARGIN;
        for seed in 1..200 {
            let samples = plane(stride * (BLOCK + MARGIN), seed, seed as usize % 3);
            let at = seed as usize % MARGIN * (stride + 1);
            let columns = super::columns(&samples[at..], stride);
            assert_eq!(columns, plain::columns(&samples[at..], stride), "seed {seed}: each column a line of lanes");
            // Values past a byte either way, and a run of places of each line.
            let mut state = seed;
            let values: [[i16; BLOCK]; BLOCK] = core::array::from_fn(|_| {
                core::array::from_fn(|_| {
                    state = xorshift32(state);
                    (state % 400) as i16 - 70
                })
            });
            let places = seed as usize % BLOCK..BLOCK - seed as usize / BLOCK % 2;
            let (mut fast, mut plain) = (samples.clone(), samples.clone());
            put_columns(&values, (&mut fast[at..], stride), places.clone());
            plain::put_columns(&values, (&mut plain[at..], stride), places.clone());
            assert_eq!(fast, plain, "seed {seed}: lanes put back at {places:?} of each line");
            put_columns(&columns, (&mut fast[at..], stride), 0..BLOCK);
            assert_eq!(super::columns(&fast[at..], stride), columns, "seed {seed}: put back and read again, as it was");
        }
        let ramp: Vec<u8> = (0..BLOCK * BLOCK).map(|at| at as u8).collect();
        assert_eq!(plain::columns(&ramp, BLOCK)[1][..3], [1, 9, 17], "the second column of a ramp, top down");
    }

    // The colours of a call's pictures, of a JPEG's and of a recording in high definition.
    const COLOURS: [Colours; 3] =
        [Colours::new(WEIGHTS_601, false), Colours::new(WEIGHTS_601, true), Colours::new(WEIGHTS_709, false)];
    // Formats of whole bytes in three orders, and one of none, which the plain body takes whole.
    const FORMATS: [PixelFormat; 4] = [
        PixelFormat::XRGB8888,
        PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 },
        PixelFormat { r_shift: 24, g_shift: 16, b_shift: 8 },
        PixelFormat { r_shift: 2, g_shift: 12, b_shift: 22 },
    ];
    // Rows the NEON body takes whole, in part and not at all.
    const WIDTHS: [usize; 9] = [0, 1, 7, 8, 9, 15, 16, 33, 640];

    #[test]
    fn words_match_plain() {
        for (index, colours) in COLOURS.iter().enumerate() {
            for (&format, &width) in FORMATS.iter().flat_map(|format| WIDTHS.iter().map(move |width| (format, width))) {
                for kind in 0..3 {
                    let seed = (index * width) as u32 + 11;
                    let (luma, blue) = (plane(width, seed, kind), plane(width, seed + 1, 0));
                    let (red, pairs) = (plane(width, seed + 2, 2 - kind), plane(2 * width, seed + 3, kind));
                    for chroma in [Chroma::Apart(&blue, &red), Chroma::Beside(&pairs), Chroma::Shared(&pairs)] {
                        let (mut fast, mut plain) = (vec![0u32; width], vec![1u32; width]);
                        words(colours, &luma, chroma, format, &mut fast);
                        plain::words(colours, &luma, chroma, format, &mut plain);
                        assert_eq!(fast, plain, "{width} pixels of kind {kind} by colours {index} in {format:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn unpack_matches_plain() {
        // Counts of codes the NEON body takes whole, in part and not at all, from bytes that end with a group or inside one.
        for count in [0, 1, 3, 4, 7, 8, 9, 15, 16, 17, 33, 1280, 1282] {
            for kind in 0..3 {
                let bytes = (count * CODE_BITS).div_ceil(BYTE_BITS);
                let packed = plane(bytes, count as u32 + 5, kind);
                let bit = |at: usize| u16::from(packed[at / BYTE_BITS] >> (at % BYTE_BITS) & 1);
                let codes: Vec<u16> = (0..count)
                    .map(|code| (0..CODE_BITS).map(|place| bit(code * CODE_BITS + place) << place).sum())
                    .collect();
                let (mut fast, mut plain) = (vec![7u16; count], vec![9u16; count]);
                unpack(&packed, &mut fast);
                plain::unpack(&packed, &mut plain);
                assert_eq!(
                    (&fast, &plain),
                    (&codes, &codes),
                    "{count} codes of kind {kind}: each its ten bits, lowest first"
                );
            }
        }
    }

    // A picture `width` across turned every way by the kernel and by the plain body, each into what held the picture back to front.
    fn check_turns<T: Sample + PartialEq + core::fmt::Debug>(picture: &[T], width: usize) {
        for (quarters, is_mirrored) in (0..4).flat_map(|quarters| [(quarters, false), (quarters, true)]) {
            let turned = Turn { quarters, is_mirrored };
            let (mut fast, mut plain): (Vec<T>, Vec<T>) = (picture.iter().rev().copied().collect(), picture.to_vec());
            T::turn(picture, width, turned, &mut fast);
            plain::turn(picture, width, turned, &mut plain);
            assert_eq!(fast, plain, "{width} across, {} down, turned {turned:?}", picture.len() / width);
        }
    }

    #[test]
    fn turns_match_plain() {
        for (width, height) in [(1, 1), (3, 2), (4, 4), (5, 7), (8, 12), (33, 18), (17, 64), (64, 40), (8, 360)] {
            let words: Vec<u32> = (0..width * height)
                .scan(width as u32 | 1, |state, _| {
                    *state = xorshift32(*state);
                    Some(*state)
                })
                .collect();
            check_turns(&words, width);
            check_turns(&words.iter().map(|&word| word as u8).collect::<Vec<u8>>(), width);
            check_turns(&words.iter().map(|&word| (word as u16).to_le_bytes()).collect::<Vec<[u8; PAIR]>>(), width);
        }
    }
}
