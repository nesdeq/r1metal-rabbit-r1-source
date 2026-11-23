// The pitch period an encoder's pre-filter takes out: the sound halved in rate and whitened, the best periods by correlation at a quarter and at half the rate, and the period's gain, its multiples weighed against its fractions (pitch.c, celt_lpc.c).

use super::postfilter::{MAX_PERIOD, MIN_PERIOD};
use crate::base::math::{dot, sqrt};

// The whitening predictor's order, its noise floor of -40 dB and lag window, its bandwidth expansion and the zero added after it (pitch_downsample).
const ORDER: usize = 4;
const NOISE_FLOOR: f32 = 1.0001;
const LAG_WINDOW: f32 = 0.008;
const EXPANSION: f32 = 0.9;
const ZERO: f32 = 0.8;
// The predictor stops once it gains 30 dB, and starts from no energy below this (_celt_lpc).
const LPC_GAIN: f32 = 0.001;
const LEAST_ENERGY: f32 = 1.0e-10;
// A correlation's square is scaled down so that it neither underflows nor overflows (find_best_pitch).
const CORRELATION_SCALE: f32 = 1.0e-12;
// How close to the coarse search's best periods the finer search looks, and the share of a neighbour's correlation that moves a period by half a step (pitch_search).
const NEAR: i32 = 2;
const INTERPOLATION: f32 = 0.7;
// remove_doubling: the fractions of the period it tries, the second period each checks, and the thresholds of their gain against the period's.
const MOST_FRACTION: usize = 15;
const SECOND_CHECK: [usize; MOST_FRACTION + 1] = [0, 0, 3, 2, 3, 2, 5, 2, 3, 2, 3, 2, 5, 2, 3, 2];
const THRESHOLD_SHARE: f32 = 0.7;
const THRESHOLD_LEAST: f32 = 0.3;
const SHORT_SHARE: f32 = 0.85;
const SHORT_LEAST: f32 = 0.4;
const CONTINUITY_SPAN: usize = 2;
const CONTINUITY_DEPTH: usize = 5;
// The most samples the search works on: the pre-filter's history and a frame, halved.
const MOST_HALF: usize = (MAX_PERIOD + super::FRAME) / 2;

// The autocorrelation of x at lags 0 to `ORDER` (_celt_autocorr).
fn autocorrelation(x: &[f32]) -> [f32; ORDER + 1] {
    core::array::from_fn(|lag| dot(&x[lag..], x))
}

// The linear predictor of `ORDER` taps of an autocorrelation, by the Levinson-Durbin recursion (_celt_lpc).
fn predictor(ac: &[f32; ORDER + 1]) -> [f32; ORDER] {
    let mut lpc = [0.0f32; ORDER];
    let mut error = ac[0];
    if ac[0] <= LEAST_ENERGY {
        return lpc;
    }
    for i in 0..ORDER {
        let sum: f32 = (0..i).map(|j| lpc[j] * ac[i - j]).sum::<f32>() + ac[i + 1];
        let reflection = -sum / error;
        lpc[i] = reflection;
        for j in 0..(i + 1) >> 1 {
            let (first, second) = (lpc[j], lpc[i - 1 - j]);
            lpc[j] = first + reflection * second;
            lpc[i - 1 - j] = second + reflection * first;
        }
        error -= reflection * reflection * error;
        if error <= LPC_GAIN * ac[0] {
            break;
        }
    }
    lpc
}

// The sound at half the rate, low-passed by [1/4, 1/2, 1/4] and whitened by a predictor of its own with a zero added (pitch_downsample).
pub(super) fn downsample(x: &[f32], out: &mut [f32]) {
    let len = out.len();
    out[0] = 0.25 * x[1] + 0.5 * x[0];
    for at in 1..len {
        out[at] = 0.25 * x[2 * at - 1] + 0.25 * x[2 * at + 1] + 0.5 * x[2 * at];
    }
    let mut ac = autocorrelation(out);
    ac[0] *= NOISE_FLOOR;
    for (lag, value) in ac.iter_mut().enumerate().skip(1) {
        let window = LAG_WINDOW * lag as f32;
        *value -= *value * window * window;
    }
    let mut lpc = predictor(&ac);
    let mut expansion = 1.0f32;
    for tap in &mut lpc {
        expansion *= EXPANSION;
        *tap *= expansion;
    }
    let taps = [lpc[0] + ZERO, lpc[1] + ZERO * lpc[0], lpc[2] + ZERO * lpc[1], lpc[3] + ZERO * lpc[2], ZERO * lpc[3]];
    let mut memory = [0.0f32; ORDER + 1];
    for value in out.iter_mut() {
        let sample = *value;
        *value = sample + taps.iter().zip(&memory).map(|(tap, past)| tap * past).sum::<f32>();
        memory.copy_within(..ORDER, 1);
        memory[0] = sample;
    }
}

// The two periods below `most` of the highest normalised correlation of x with y, the best first (find_best_pitch).
fn best_periods(correlations: &[f32], y: &[f32], len: usize, most: usize) -> [usize; 2] {
    let mut energy = 1.0f32 + dot(&y[..len], &y[..len]);
    let (mut nums, mut dens, mut best) = ([-1.0f32; 2], [0.0f32; 2], [0, 1]);
    for at in 0..most {
        if correlations[at] > 0.0 {
            let scaled = correlations[at] * CORRELATION_SCALE;
            let num = scaled * scaled;
            if num * dens[1] > nums[1] * energy {
                if num * dens[0] > nums[0] * energy {
                    (nums[1], dens[1], best[1]) = (nums[0], dens[0], best[0]);
                    (nums[0], dens[0], best[0]) = (num, energy, at);
                } else {
                    (nums[1], dens[1], best[1]) = (num, energy, at);
                }
            }
        }
        energy += y[at + len] * y[at + len] - y[at] * y[at];
        energy = energy.max(1.0);
    }
    best
}

// The lag of x within y, of `len` samples at half the rate, up to `most`, at twice the half rate's resolution: by a coarse search at a quarter of the rate, a finer one near its best two, and a half step toward the larger neighbour (pitch_search).
pub(super) fn search(x: &[f32], y: &[f32], len: usize, most: usize) -> usize {
    let lag = len + most;
    let mut quarter_x = [0.0f32; MOST_HALF / 2];
    let mut quarter_y = [0.0f32; MOST_HALF];
    for (out, &value) in quarter_x[..len >> 2].iter_mut().zip(x.iter().step_by(2)) {
        *out = value;
    }
    for (out, &value) in quarter_y[..lag >> 2].iter_mut().zip(y.iter().step_by(2)) {
        *out = value;
    }
    let mut correlations = [0.0f32; MOST_HALF];
    for (at, value) in correlations[..most >> 2].iter_mut().enumerate() {
        *value = dot(&quarter_x[..len >> 2], &quarter_y[at..]);
    }
    let coarse = best_periods(&correlations, &quarter_y, len >> 2, most >> 2);
    for (at, value) in correlations[..most >> 1].iter_mut().enumerate() {
        let is_near = coarse.iter().any(|&best| (at as i32 - 2 * best as i32).abs() <= NEAR);
        *value = if is_near { dot(&x[..len >> 1], &y[at..]).max(-1.0) } else { 0.0 };
    }
    let best = best_periods(&correlations, y, len >> 1, most >> 1)[0];
    let offset = if best > 0 && best < (most >> 1) - 1 {
        let (a, b, c) = (correlations[best - 1], correlations[best], correlations[best + 1]);
        if c - a > INTERPOLATION * (b - a) {
            1
        } else if a - c > INTERPOLATION * (b - c) {
            -1
        } else {
            0
        }
    } else {
        0
    };
    (2 * best as i32 - offset) as usize
}

fn gain_of(xy: f32, xx: f32, yy: f32) -> f32 {
    xy / sqrt(1.0 + xx * yy)
}

// The gain of the period, after it is replaced by a fraction of it that correlates nearly as well, so that a multiple of the true period is not taken for it; `x` is the whitened sound at half the rate, the longest period of history then `n` of the frame, at the full rate as the periods are (remove_doubling).
pub(super) fn remove_doubling(x: &[f32], n: usize, period: &mut usize, last_period: usize, last_gain: f32) -> f32 {
    let (max_period, min_period, n) = (MAX_PERIOD / 2, MIN_PERIOD / 2, n / 2);
    let last_period = last_period / 2;
    let x0 = max_period;
    let frame = &x[x0..x0 + n];
    let behind = |lag: usize| &x[x0 - lag..x0 - lag + n];
    let t0 = (*period / 2).min(max_period - 1);
    let xx = dot(frame, frame);
    let xy = dot(frame, behind(t0));
    let mut energies = [0.0f32; MAX_PERIOD / 2 + 1];
    energies[0] = xx;
    let mut yy = xx;
    for lag in 1..=max_period {
        yy = yy + x[x0 - lag] * x[x0 - lag] - x[x0 + n - lag] * x[x0 + n - lag];
        energies[lag] = yy.max(0.0);
    }
    let yy = energies[t0];
    let (mut best_xy, mut best_yy) = (xy, yy);
    let first_gain = gain_of(xy, xx, yy);
    let (mut best_period, mut gain) = (t0, first_gain);
    for (k, &check) in SECOND_CHECK.iter().enumerate().skip(2) {
        let t1 = (2 * t0 + k) / (2 * k);
        if t1 < min_period {
            break;
        }
        let t1b = if k == 2 { if t1 + t0 > max_period { t0 } else { t0 + t1 } } else { (2 * check * t0 + k) / (2 * k) };
        let xy = 0.5 * (dot(frame, behind(t1)) + dot(frame, behind(t1b)));
        let yy = 0.5 * (energies[t1] + energies[t1b]);
        let candidate = gain_of(xy, xx, yy);
        let distance = t1.abs_diff(last_period);
        let continuity = if distance <= 1 {
            last_gain
        } else if distance <= CONTINUITY_SPAN && CONTINUITY_DEPTH * k * k < t0 {
            0.5 * last_gain
        } else {
            0.0
        };
        // A short period is held to more, against correlation over short spans.
        let threshold = if t1 < 3 * min_period {
            (SHORT_SHARE * first_gain - continuity).max(SHORT_LEAST)
        } else {
            (THRESHOLD_SHARE * first_gain - continuity).max(THRESHOLD_LEAST)
        };
        if candidate > threshold {
            (best_xy, best_yy, best_period, gain) = (xy, yy, t1, candidate);
        }
    }
    let best_xy = best_xy.max(0.0);
    let mut period_gain = if best_yy <= best_xy { 1.0 } else { best_xy / (best_yy + 1.0) };
    let around: [f32; 3] = core::array::from_fn(|k| dot(frame, behind(best_period + k - 1)));
    let offset = if around[2] - around[0] > INTERPOLATION * (around[1] - around[0]) {
        1
    } else if around[0] - around[2] > INTERPOLATION * (around[1] - around[2]) {
        -1
    } else {
        0
    };
    period_gain = period_gain.min(gain);
    *period = ((2 * best_period) as i32 + offset).max(MIN_PERIOD as i32) as usize;
    period_gain
}
