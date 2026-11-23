use super::{MAX_LM, OVERLAP, SHORT};
use crate::base::fft::{Complex, Plan, rotate};
use crate::base::math::sine_cosine;
use alloc::vec;
use alloc::vec::Vec;

// The longest transform's samples: twice the bins of a 20 ms frame.
const LONGEST: usize = 2 * (SHORT << MAX_LM);
// The turn by an eighth of a bin that centres the bins, small enough to stand for its own sine with a cosine of one (mdct.c).
const EIGHTH_BIN: f64 = 0.125;

// The turn by an eighth of a bin of a block of `size` bins.
fn eighth_turn(size: usize) -> Complex {
    [1.0, (core::f64::consts::TAU * EIGHTH_BIN / (2 * size) as f64) as f32]
}

// The MDCT and its inverse (4.3.7): an N/4 point complex transform between rotations, and the low-overlap window (mdct.c).
pub(super) struct Mdct {
    // The transform of each length, from the longest down.
    plans: Vec<Plan>,
    // cos(2 pi i / LONGEST) for i up to a quarter of it.
    trig: Vec<f32>,
    window: Vec<f32>,
    rotated: Vec<Complex>,
    points: Vec<Complex>,
    folded: Vec<f32>,
    block: Vec<f32>,
}

// The window's rising half over the overlap, sin(pi/2 sin^2(pi/2 (n + 1/2) / overlap)) (4.3.7).
fn window() -> Vec<f32> {
    (0..OVERLAP)
        .map(|at| {
            let rise = sine_cosine((at as f64 + 0.5) / (4.0 * OVERLAP as f64)).0;
            sine_cosine(rise * rise / 4.0).0 as f32
        })
        .collect()
}

impl Mdct {
    pub(super) fn new() -> Mdct {
        Mdct {
            plans: (0..=MAX_LM).map(|shift| Plan::new((LONGEST / 4) >> shift)).collect(),
            trig: (0..=LONGEST / 4).map(|at| sine_cosine(at as f64 / LONGEST as f64).1 as f32).collect(),
            window: window(),
            rotated: vec![[0.0; 2]; LONGEST / 4],
            points: vec![[0.0; 2]; LONGEST / 4],
            folded: vec![0.0; LONGEST / 2],
            block: vec![0.0; LONGEST / 2 + OVERLAP],
        }
    }

    pub(super) fn window(&self) -> &[f32] {
        &self.window
    }

    // The samples of a frame's bins, those of `blocks` short transforms interleaved or of one long, into `out`: its first samples added to the `overlap` the frame before left, and the overlap it leaves the next (compute_inv_mdcts).
    pub(super) fn inverse(&mut self, spectrum: &[f32], blocks: usize, out: &mut [f32], overlap: &mut [f32]) {
        let n = out.len();
        let size = n / blocks;
        self.block[..OVERLAP].fill(0.0);
        for block in 0..blocks {
            self.backward(spectrum, block, blocks, size);
        }
        for (at, sample) in out.iter_mut().enumerate() {
            *sample = self.block[at] + if at < OVERLAP { overlap[at] } else { 0.0 };
        }
        overlap.copy_from_slice(&self.block[n..n + OVERLAP]);
    }

    // The bins of a frame of `spectrum.len()` samples, those of `blocks` short transforms interleaved or of one long, from `input`: the overlap the frame before ends in, then the frame (compute_mdcts).
    pub(super) fn forward(&mut self, input: &[f32], blocks: usize, spectrum: &mut [f32]) {
        let size = spectrum.len() / blocks;
        for block in 0..blocks {
            self.forward_block(&input[block * size..][..size + OVERLAP], block, blocks, spectrum);
        }
    }

    // The `size` bins of one block from its samples and the overlap before them, written `stride` apart from `first` (clt_mdct_forward).
    fn forward_block(&mut self, input: &[f32], first: usize, stride: usize, spectrum: &mut [f32]) {
        let size = input.len() - OVERLAP;
        let shift = (LONGEST / (2 * size)).trailing_zeros() as usize;
        let (n2, n4) = (size, size / 2);
        let eighth = eighth_turn(size);
        let turn = |at: usize| [self.trig[at << shift], self.trig[(n4 - at) << shift]];
        let (half, window) = (OVERLAP / 2, &self.window);
        // The windowed edges of the block, each as many pairs as cover half the overlap.
        let edge = OVERLAP.div_ceil(4);
        for at in 0..n4 {
            let (one, other) = (half + 2 * at, n2 - 1 + half - 2 * at);
            let (re, im) = if at < edge {
                let (rising, falling) = (window[half + 2 * at], window[half - 1 - 2 * at]);
                (input[one + n2] * falling + input[other] * rising, input[one] * rising - input[other - n2] * falling)
            } else if at < n4 - edge {
                (input[other], input[one])
            } else {
                let step = 2 * (at - (n4 - edge));
                let (rising, falling) = (window[step], window[OVERLAP - 1 - step]);
                (-input[one - n2] * rising + input[other] * falling, input[one] * falling + input[other + n2] * rising)
            };
            let [re, im] = rotate(rotate([re, im], turn(at), -1.0), eighth, -1.0);
            let scale = 1.0 / n4 as f32;
            self.rotated[at] = [scale * re, scale * im];
        }
        self.plans[shift].forward(&self.rotated[..n4], &mut self.points[..n4]);
        for at in 0..n4 {
            let [re, im] = rotate(rotate(self.points[at], turn(at), -1.0), eighth, -1.0);
            spectrum[first + 2 * stride * at] = -re;
            spectrum[first + stride * (n2 - 1 - 2 * at)] = im;
        }
    }

    // The windowed samples of the `size` bins of one block, read `stride` apart from `first`, into the block buffer from `first * size`, its first overlap added (clt_mdct_backward).
    fn backward(&mut self, spectrum: &[f32], first: usize, stride: usize, size: usize) {
        let shift = (LONGEST / (2 * size)).trailing_zeros() as usize;
        let (n2, n4) = (size, size / 2);
        let eighth = eighth_turn(size);
        let turn = |at: usize| [self.trig[at << shift], self.trig[(n4 - at) << shift]];
        for at in 0..n4 {
            let (one, other) = (spectrum[first + 2 * at * stride], spectrum[first + (n2 - 1 - 2 * at) * stride]);
            self.rotated[at] = rotate(rotate([-other, -one], turn(at), 1.0), eighth, 1.0);
        }
        self.plans[shift].inverse(&self.rotated[..n4], &mut self.points[..n4]);
        for at in 0..n4 {
            self.points[at] = rotate(rotate(self.points[at], turn(at), 1.0), eighth, 1.0);
        }
        for at in 0..n4 {
            self.folded[2 * at] = -self.points[at][0];
            self.folded[2 * at + 1] = self.points[n4 - 1 - at][1];
        }
        let (half, window) = (OVERLAP / 2, &self.window);
        let out = &mut self.block[first * size..first * size + size + OVERLAP];
        for at in 0..n4 - half {
            out[n4 + half - 1 - at] = self.folded[n4 - 1 - at];
            out[n4 + half + at] = self.folded[n4 + at];
        }
        for at in 0..half {
            let value = self.folded[half - 1 - at];
            out[at] -= window[at] * value;
            out[OVERLAP - 1 - at] += window[OVERLAP - 1 - at] * value;
            let value = self.folded[n2 - half + at];
            out[n2 + OVERLAP - 1 - at] = window[at] * value;
            out[n2 + at] = window[OVERLAP - 1 - at] * value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::next_random;
    use core::f64::consts::PI;

    fn noise(count: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..count).map(|_| (next_random(&mut state) >> 16) as f32 / 32_768.0 - 1.0).collect()
    }

    // The inverse MDCT of 2L samples by its sum, unscaled: y[t] = sum of X[k] cos(pi/L (t + 1/2 + L/2)(k + 1/2)).
    fn definition(bins: &[f32]) -> Vec<f64> {
        let size = bins.len();
        (0..2 * size)
            .map(|t| {
                bins.iter()
                    .enumerate()
                    .map(|(k, &bin)| {
                        let phase = PI / size as f64 * (t as f64 + 0.5 + size as f64 / 2.0) * (k as f64 + 0.5);
                        f64::from(bin) * phase.cos()
                    })
                    .sum()
            })
            .collect()
    }

    // The low-overlap window over a block's samples from the first it writes: rising, flat, falling.
    fn low_overlap(window: &[f32], size: usize, at: usize) -> f64 {
        f64::from(if at < OVERLAP {
            window[at]
        } else if at < size {
            1.0
        } else {
            window[size + OVERLAP - 1 - at]
        })
    }

    #[test]
    fn blocks_match_definition() {
        let mut mdct = Mdct::new();
        for size in [120, 240, 480, 960] {
            let bins = noise(size, size as u32);
            let (mut out, mut overlap) = (vec![0.0; size], vec![0.0; OVERLAP]);
            mdct.inverse(&bins, 1, &mut out, &mut overlap);
            let reference = definition(&bins);
            let skip = (size - OVERLAP) / 2;
            for (at, &actual) in out.iter().chain(&overlap).enumerate() {
                let expected = low_overlap(&mdct.window, size, at) * reference[skip + at];
                assert!(
                    (f64::from(actual) - expected).abs() < 2.0e-3,
                    "{size} bins, sample {at}: {actual} for {expected}, the IMDCT from a quarter less half the overlap"
                );
            }
        }
    }

    #[test]
    fn short_blocks_overlap_added() {
        let mut mdct = Mdct::new();
        let bins = noise(480, 9);
        let (mut out, mut overlap) = (vec![0.0; 480], noise(OVERLAP, 4));
        let before = overlap.clone();
        mdct.inverse(&bins, 4, &mut out, &mut overlap);
        let mut expected = vec![0.0f64; 480 + OVERLAP];
        for block in 0..4 {
            let own: Vec<f32> = (0..120).map(|k| bins[block + 4 * k]).collect();
            let reference = definition(&own);
            for at in 0..120 + OVERLAP {
                expected[block * 120 + at] += low_overlap(&mdct.window, 120, at) * reference[at];
            }
        }
        for (at, value) in expected.iter_mut().take(OVERLAP).enumerate() {
            *value += f64::from(before[at]);
        }
        for (at, (&actual, &expected)) in out.iter().chain(&overlap).zip(&expected).enumerate() {
            assert!((f64::from(actual) - expected).abs() < 1.0e-3, "sample {at}: {actual} for {expected}");
        }
    }

    #[test]
    fn window_power_complementary() {
        let window = window();
        for at in 0..OVERLAP {
            let (rising, falling) = (window[at], window[OVERLAP - 1 - at]);
            assert!((rising * rising + falling * falling - 1.0).abs() < 1.0e-6, "at {at}: the overlap keeps power");
        }
        assert!(window.windows(2).all(|pair| pair[0] < pair[1]), "rising");
        assert!((window[0] - 6.728_696_6e-5).abs() < 1.0e-9 && (window[119] - 1.0).abs() < 1.0e-6, "window120's ends");
    }

    #[test]
    fn forward_then_inverse_gives_the_sound_back() {
        let mut mdct = Mdct::new();
        for (size, blocks) in [(960, 1), (960, 8), (480, 1), (480, 4), (240, 1), (240, 2), (120, 1)] {
            let frames = 6;
            let signal: Vec<f32> =
                noise(size * frames + OVERLAP, (size + blocks) as u32).iter().map(|value| 1000.0 * value).collect();
            let (mut overlap, mut spectrum, mut sound) = (vec![0.0; OVERLAP], vec![0.0; size], Vec::new());
            for frame in 0..frames {
                mdct.forward(&signal[frame * size..][..size + OVERLAP], blocks, &mut spectrum);
                let mut samples = vec![0.0; size];
                mdct.inverse(&spectrum, blocks, &mut samples, &mut overlap);
                sound.extend(samples);
            }
            let worst = sound[size..].iter().zip(&signal[size..]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            // Measured at most 0.0217 off samples of up to 1000; the bound lies 0.008 over.
            assert!(
                worst < 0.03,
                "{size} bins in {blocks} blocks: each frame's sound back from its bins and the overlap, {worst} off"
            );
        }
    }

    #[test]
    fn overlap_carried_to_next_frame() {
        let mut mdct = Mdct::new();
        let bins = noise(960, 3);
        let (mut first, mut second, mut overlap) = (vec![0.0; 960], vec![0.0; 960], vec![0.0; OVERLAP]);
        mdct.inverse(&bins, 1, &mut first, &mut overlap);
        let left = overlap.clone();
        mdct.inverse(&vec![0.0; 960], 1, &mut second, &mut overlap);
        assert!(second[..OVERLAP].iter().zip(&left).all(|(a, b)| a == b), "silence after a frame plays what it left");
        assert!(second[OVERLAP..].iter().chain(&overlap).all(|&sample| sample == 0.0), "and nothing more");
    }
}
