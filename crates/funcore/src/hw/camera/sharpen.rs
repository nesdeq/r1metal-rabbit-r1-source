//! A photo's luma sharpened by three unsharp masks of growing reach, each only where its detail stands above the photo's noise (IPOL's HDR+ finishing).

use crate::base::sampling::{filter_down, filter_row, reflect_101};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

// IPOL's three masks: their deviations in pixels, their amounts in 1024ths, and the detail each leaves alone, in 255ths.
const DEVIATIONS: [f64; 3] = [1.0, 2.0, 4.0];
const AMOUNTS: [i32; 3] = [1024, 512, 512];
const THRESHOLDS: [i32; 3] = [5, 10, 15];
const AMOUNT_BITS: u32 = 10;
// OpenCV reaches three deviations with the Gaussian blur of 8-bit samples.
const REACH_DEVIATIONS: f64 = 3.0;
const WEIGHT_BITS: u32 = 8;
// Donoho and Johnstone: the median absolute size of the finest wavelet coefficients over 0.6745 is their noise's deviation.
const MAD_SCALE: f32 = 0.6745;
// Three deviations hold all but 0.3 % of a Gaussian noise.
const NOISE_DEVIATIONS: f32 = 3.0;
const HAAR_STEPS: usize = 4 * u8::MAX as usize + 1;

fn kernel(deviation: f64) -> Vec<u32> {
    let reach = (REACH_DEVIATIONS * deviation + 0.5) as i64;
    let raw: Vec<f64> = (-reach..=reach)
        .map(|at| {
            crate::base::math::power_f64(core::f64::consts::E, -((at * at) as f64) / (2.0 * deviation * deviation))
        })
        .collect();
    let total: f64 = raw.iter().sum();
    let unit = f64::from(1u32 << WEIGHT_BITS);
    let mut weights: Vec<u32> = raw.iter().map(|weight| (weight / total * unit + 0.5) as u32).collect();
    // What rounding took or gave goes to the middle, so that a flat picture stays flat.
    let middle = weights.len() / 2;
    let sum: u32 = weights.iter().sum();
    weights[middle] = (weights[middle] as i64 + i64::from(1u32 << WEIGHT_BITS) - i64::from(sum)) as u32;
    weights
}

/// The three masks and how much detail each leaves alone in one photo.
pub struct Sharpener {
    // Each mask's weights as its blur across takes them, a byte's, and as its blur down of those sums takes them.
    across: [Vec<u8>; 3],
    down: [Vec<u16>; 3],
    thresholds: [i32; 3],
}

impl Sharpener {
    /// A sharpener for the square luma `luma` of `side` pixels, its thresholds raised to three deviations of its noise.
    pub fn new(luma: &[u8], side: usize) -> Sharpener {
        let mut counts = vec![0u32; HAAR_STEPS];
        let mut blocks = 0u32;
        for pair in 0..side / 2 {
            let (upper, lower) = (&luma[2 * pair * side..][..side], &luma[(2 * pair + 1) * side..][..side]);
            for column in 0..side / 2 {
                let [a, b] = [upper[2 * column], upper[2 * column + 1]].map(i32::from);
                let [c, d] = [lower[2 * column], lower[2 * column + 1]].map(i32::from);
                counts[(a - b - c + d).unsigned_abs() as usize] += 1;
                blocks += 1;
            }
        }
        let mut total = 0;
        let median = counts
            .iter()
            .position(|count| {
                total += count;
                total * 2 > blocks
            })
            .unwrap_or(0);
        // The finest diagonal coefficient of the orthonormal Haar transform is half the sum.
        let deviation = median as f32 / 2.0 / MAD_SCALE;
        let floor = (NOISE_DEVIATIONS * deviation + 0.5) as i32;
        // Weights in 256ths that sum to 256 over several taps are each under 256.
        let across = DEVIATIONS.map(|deviation| {
            kernel(deviation).iter().map(|&weight| u8::try_from(weight).expect("a weight under one")).collect()
        });
        let down = across.each_ref().map(|weights: &Vec<u8>| weights.iter().map(|&weight| weight.into()).collect());
        Sharpener { across, down, thresholds: THRESHOLDS.map(|threshold| threshold.max(floor)) }
    }

    /// Sharpens lines `lines` of the square `source` of `side` pixels into `out`, which holds just those lines.
    pub fn sharpen(&self, source: &[u8], side: usize, lines: Range<usize>, out: &mut [u8]) {
        assert!(lines.end <= side && out.len() >= lines.len() * side && source.len() >= side * side, "lines {lines:?}");
        let reach = self.across.iter().map(|weights| weights.len() / 2).max().unwrap_or(0);
        let first = lines.start.saturating_sub(reach);
        let last = (lines.end + reach).min(side);
        // Each mask's blur across, of every line it reaches down to or up to, in 256ths.
        let across: [Vec<u16>; 3] = core::array::from_fn(|mask| {
            let mut blurred = vec![0u16; (last - first) * side];
            for (line, out) in (first..last).zip(blurred.chunks_mut(side)) {
                filter_row(&source[line * side..][..side], (&self.across[mask], 1), reflect_101, out);
            }
            blurred
        });
        // Each mask's blur down of the line, in 65536ths.
        let mut blurs: [Vec<u32>; 3] = core::array::from_fn(|_| vec![0; side]);
        for (line, out) in lines.clone().zip(out.chunks_mut(side)) {
            for ((blur, weights), across) in blurs.iter_mut().zip(&self.down).zip(&across) {
                let row = |at: usize| &across[(at - first) * side..][..side];
                filter_down(row, (weights, line, side), reflect_101, blur);
            }
            let row = &source[line * side..][..side];
            for (x, pixel) in out.iter_mut().enumerate() {
                let value = i32::from(row[x]);
                let mut sum = 0;
                for (mask, blur) in blurs.iter().enumerate() {
                    let blurred = ((blur[x] + (1 << (2 * WEIGHT_BITS - 1))) >> (2 * WEIGHT_BITS)) as i32;
                    let detail = value - blurred;
                    sum += if detail.abs() < self.thresholds[mask] {
                        value
                    } else {
                        value + ((AMOUNTS[mask] * detail) >> AMOUNT_BITS)
                    };
                }
                *pixel = ((sum + 1) / 3).clamp(0, i32::from(u8::MAX)) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;

    const SIDE: usize = 48;

    #[test]
    fn kernels_sum_to_one() {
        for deviation in DEVIATIONS {
            let kernel = kernel(deviation);
            assert_eq!(kernel.iter().sum::<u32>(), 1 << WEIGHT_BITS, "{deviation}");
            assert_eq!(kernel.len(), 2 * (3.0 * deviation) as usize + 1, "OpenCV's size for 8-bit samples");
        }
    }

    #[test]
    fn edges_steeper_flat_and_noise_kept() {
        let edge: Vec<u8> = (0..SIDE * SIDE).map(|at| if at % SIDE < SIDE / 2 { 80 } else { 160 }).collect();
        let sharpener = Sharpener::new(&edge, SIDE);
        assert_eq!(sharpener.thresholds, THRESHOLDS, "a picture without noise keeps IPOL's thresholds");
        let mut out = vec![0u8; SIDE * SIDE];
        for (index, part) in out.chunks_mut(16 * SIDE).enumerate() {
            sharpener.sharpen(&edge, SIDE, 16 * index..16 * index + part.len() / SIDE, part);
        }
        let row = &out[20 * SIDE..21 * SIDE];
        assert!(row[SIDE / 2 - 1] < 80 && row[SIDE / 2] > 160, "the edge overshoots on both sides: {row:?}");
        assert!(row[2] == 80 && row[SIDE - 3] == 160, "flat ground stays: {row:?}");
        let mut state = 9u32;
        let noisy: Vec<u8> = (0..SIDE * SIDE)
            .map(|_| {
                state = xorshift32(state);
                120 + (state % 25) as u8
            })
            .collect();
        let calm = Sharpener::new(&noisy, SIDE);
        assert!(
            calm.thresholds.iter().all(|&threshold| threshold > THRESHOLDS[2]),
            "noise raises the thresholds: {:?}",
            calm.thresholds
        );
        let mut calmed = vec![0u8; SIDE * SIDE];
        calm.sharpen(&noisy, SIDE, 0..SIDE, &mut calmed);
        let changed = calmed.iter().zip(&noisy).filter(|(a, b)| a != b).count();
        assert!(changed * 10 < SIDE * SIDE, "and the noise is mostly left alone: {changed} pixels changed");
    }
}
