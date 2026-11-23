use super::allocation::{Allocation, BIT, bits_to_pulses, cache, pulses, pulses_to_bits};
use super::energy::Energies;
use super::pvq;
use super::tables::{BIT_DEINTERLEAVE, BIT_INTERLEAVE, E_BANDS, EXP2_EIGHTHS, LOG_N, ORDERY, TF_SELECT};
use super::{BANDS, Frame, MAX_LM, WIDEST};
use crate::base::math::{atan2_turns, dot, next_random, pow2_f64, sqrt};
use crate::codec::opus::range::{FRACTION_BITS, RangeCoder, ilog};
use core::f32::consts::{FRAC_1_SQRT_2, SQRT_2};

// The bits of a split's angle are offset by these in eighths, in a stereo band of two bins and in any other (QTHETA_OFFSET).
const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;
// The angle of a split runs from all mid at nought to all side at a quarter turn, in Q14.
const QUARTER_TURN: i32 = 16_384;
const EIGHTH_TURN: i32 = 8_192;
const Q15_ONE: i32 = 32_767;
const Q15: f32 = 32_768.0;
// The polynomials of the bit-exact cosine and log2 of the tangent the split's bits are shared by (bands.c).
const COSINE_POLYNOMIAL: [i32; 3] = [-7_651, 8_277, -626];
const TANGENT_POLYNOMIAL: [i32; 2] = [-2_597, 7_932];
// A part is split in two where its bits are 1.5 more, in eighths, than its largest codebook takes (quant_band).
const SPLIT_MARGIN: i32 = 12;
// The spread value under which a band of one long block, its resolution unchanged, takes noise in place of folding (Table 59).
const SPREAD_AGGRESSIVE: usize = 3;
// The noise added to a folded band, about 48 dB under the folding (quant_band).
const FOLD_NOISE: f32 = 1.0 / 256.0;
// The least energy of either channel a merged stereo band is formed with (stereo_merge).
const MERGE_FLOOR: f32 = 6.0e-4;
// The most bits a band is given, in eighths (quant_all_bands).
const MOST_BAND_BITS: i32 = 16_383;
// The step distribution of a stereo angle weighs the angles up to the middle threefold (quant_band).
const STEP_WEIGHT: u32 = 3;
// The bits above which a band's bits left over go on to its other half, in eighths (quant_band).
const REBALANCE_FLOOR: i32 = 3 * BIT;
// The bits of an angle's distribution checked before a stereo band's inversion is read, in eighths.
const INVERSION_FLOOR: i32 = 2 * BIT;
// The tf_change flag's logp in the first band and the others, in a transient frame and in another (4.3.4.5).
const TF_LOGP: [[u32; 2]; 2] = [[4, 5], [2, 4]];
// The noise of a band filled from nothing takes the generator's top 12 bits.
const NOISE_SHIFT: u32 = 20;
// A collapsed block gets noise of the sign of this bit of the generator (anti_collapse).
const NOISE_SIGN: u32 = 0x8000;
// The anti-collapse threshold falls by 2^(1/8) for each eighth of a bit a bin was given.
const DEPTH_STEP: f32 = 0.125;
// The angle of a split is measured in Q30 of a quarter turn and kept in Q14, and is nought under this energy of its halves (stereo_itheta, celt_atan2p_norm).
const QUARTER_TURNS: f64 = 4.0;
const ANGLE_SCALE: f64 = (1u32 << 30) as f64;
const ANGLE_FRACTION: u32 = 16;
const LEAST_ANGLE_ENERGY: f32 = 1.0e-18;
// What a band's energy is measured above, so that a silent band has an amplitude, and normalised by (compute_band_energies, normalise_bands).
const LEAST_BAND_ENERGY: f32 = 1.0e-27;

// Multiplies two Q15 fractions, as 16-bit values, rounding (FRAC_MUL16).
fn frac_mul16(first: i32, second: i32) -> i32 {
    ((1 << 14) + i32::from(first as i16) * i32::from(second as i16)) >> 15
}

// The cosine of an angle of a quarter turn in Q14, in Q15, bit for bit (bitexact_cos).
fn cosine(angle: i32) -> i32 {
    let [first, second, third] = COSINE_POLYNOMIAL;
    let square = ((1 << 12) + angle * angle) >> 13;
    let value = (Q15_ONE - square) + frac_mul16(square, first + frac_mul16(square, second + frac_mul16(third, square)));
    1 + value
}

// log2 of the tangent of an angle given its sine and cosine, in Q11, bit for bit (bitexact_log2tan).
fn log2_tangent(sine: i32, cosine: i32) -> i32 {
    let [first, second] = TANGENT_POLYNOMIAL;
    let (sine_log, cosine_log) = (ilog(sine as u32) as i32, ilog(cosine as u32) as i32);
    let (sine, cosine) = (sine << (15 - sine_log), cosine << (15 - cosine_log));
    let part = |value: i32| frac_mul16(value, frac_mul16(value, first) + second);
    (sine_log - cosine_log) * (1 << 11) + part(sine) - part(cosine)
}

// The steps of a split's angle the bits allow (compute_qn).
fn theta_steps(n: i32, bits: i32, offset: i32, pulse_cap: i32, is_stereo: bool) -> i32 {
    let n2 = 2 * n - 1 - i32::from(is_stereo && n == 2);
    let resolution = (bits - pulse_cap - (4 << FRACTION_BITS)).min((bits + n2 * offset) / n2).min(8 << FRACTION_BITS);
    if resolution < (1 << FRACTION_BITS >> 1) {
        return 1;
    }
    let steps = EXP2_EIGHTHS[(resolution & 7) as usize] >> (14 - (resolution >> FRACTION_BITS));
    (steps + 1) >> 1 << 1
}

// Bins of the generator's noise, before they are normalised.
pub(super) fn noise(x: &mut [f32], seed: &mut u32) {
    for value in x.iter_mut() {
        *value = (next_random(seed) as i32 >> NOISE_SHIFT) as f32;
    }
}

// One level of the Haar transform over pairs of rows `stride` apart (haar1).
pub(super) fn haar(x: &mut [f32], len: usize, stride: usize) {
    for offset in 0..stride {
        for pair in 0..len / 2 {
            let (first, second) = (stride * 2 * pair + offset, stride * (2 * pair + 1) + offset);
            let (a, b) = (FRAC_1_SQRT_2 * x[first], FRAC_1_SQRT_2 * x[second]);
            (x[first], x[second]) = (a + b, a - b);
        }
    }
}

// The bins of `stride` interleaved blocks of `len` each laid block after block, in the Hadamard's sequency order where `is_hadamard` (deinterleave_hadamard).
fn deinterleave(x: &mut [f32], len: usize, stride: usize, is_hadamard: bool) {
    let mut copy = [0.0; WIDEST];
    let order = &ORDERY[stride.saturating_sub(2)..];
    for block in 0..stride {
        let to = if is_hadamard { order[block] } else { block };
        for at in 0..len {
            copy[to * len + at] = x[at * stride + block];
        }
    }
    x[..len * stride].copy_from_slice(&copy[..len * stride]);
}

// Undoes `deinterleave` (interleave_hadamard).
fn interleave(x: &mut [f32], len: usize, stride: usize, is_hadamard: bool) {
    let mut copy = [0.0; WIDEST];
    let order = &ORDERY[stride.saturating_sub(2)..];
    for block in 0..stride {
        let from = if is_hadamard { order[block] } else { block };
        for at in 0..len {
            copy[at * stride + block] = x[from * len + at];
        }
    }
    x[..len * stride].copy_from_slice(&copy[..len * stride]);
}

// The left and right channels of a band from its mid at `mid` and its side (stereo_merge).
fn merge(x: &mut [f32], y: &mut [f32], mid: f32) {
    let (mut product, mut side) = (0.0f32, 0.0f32);
    for (&a, &b) in x.iter().zip(y.iter()) {
        product += a * b;
        side += b * b;
    }
    product *= mid;
    let left = mid * mid + side - 2.0 * product;
    let right = mid * mid + side + 2.0 * product;
    if right < MERGE_FLOOR || left < MERGE_FLOOR {
        y.copy_from_slice(x);
        return;
    }
    let (left_gain, right_gain) = (1.0 / sqrt(left), 1.0 / sqrt(right));
    for (a, b) in x.iter_mut().zip(y.iter_mut()) {
        let (l, r) = (mid * *a, *b);
        (*a, *b) = (left_gain * (l - r), right_gain * (l + r));
    }
}

// The angle in Q14 of a quarter turn between two halves of a band, by their energies (stereo_itheta).
fn angle(x: &[f32], y: &[f32]) -> i32 {
    let (mid, side) = (sqrt(dot(x, x)), sqrt(dot(y, y)));
    if mid * mid + side * side < LEAST_ANGLE_ENERGY {
        return 0;
    }
    let quarters = QUARTER_TURNS * atan2_turns(f64::from(side), f64::from(mid));
    ((ANGLE_SCALE * quarters + 0.5) as i64 >> ANGLE_FRACTION) as i32
}

// What a part of a band is coded with: its bits in eighths, its blocks, its LM, the gain of its shape, and which of its blocks may be folded into.
#[derive(Clone, Copy)]
struct Part {
    bits: i32,
    blocks: usize,
    lm: i32,
    gain: f32,
    fill: u32,
}

// A split's angle, the bits it took in eighths, the gains of its halves and how their bits differ.
struct Theta {
    itheta: i32,
    qalloc: i32,
    mid: f32,
    side: f32,
    delta: i32,
}

// What runs through a frame's bands as their shapes are coded.
struct Coder<'r, C: RangeCoder> {
    range: &'r mut C,
    remaining: i32,
    seed: u32,
    spread: usize,
    intensity: usize,
    band: usize,
    tf_change: i32,
    // An encoder's: whether a split may not leave one half to noise (a transient frame's first band).
    does_avoid_split_noise: bool,
}

impl<C: RangeCoder> Coder<'_, C> {
    // A band of one bin in each channel: its sign alone.
    fn single(&mut self, x: &mut [f32], y: Option<&mut [f32]>, lowband_out: Option<&mut [f32]>) -> u32 {
        let sign = |coder: &mut Self, value: f32| {
            let is_negative = coder.remaining >= BIT && {
                coder.remaining -= BIT;
                coder.range.bits(u32::from(value < 0.0), 1) == 1
            };
            if is_negative { -1.0 } else { 1.0 }
        };
        x[0] = sign(self, x[0]);
        if let Some(y) = y {
            y[0] = sign(self, y[0]);
        }
        if let Some(out) = lowband_out {
            out[0] = x[0];
        }
        1
    }

    // The angle that splits a part in two halves, `x` and `y`, or a stereo band into mid and side; the halves' blocks were `b0` before the split. An encoder, which codes one channel alone, takes its angle from the halves.
    fn theta(&mut self, x: &[f32], y: &[f32], bits: i32, b0: usize, lm: i32, is_stereo: bool) -> Theta {
        let n = x.len();
        let pulse_cap = LOG_N[self.band] + lm * BIT;
        let offset = (pulse_cap >> 1) - if is_stereo && n == 2 { QTHETA_OFFSET_TWOPHASE } else { QTHETA_OFFSET };
        let mut steps = theta_steps(n as i32, bits, offset, pulse_cap, is_stereo);
        if is_stereo && self.band >= self.intensity {
            steps = 1;
        }
        let wanted = if C::IS_ENCODER { angle(x, y) } else { 0 };
        let tell = self.range.tell_frac() as i32;
        let mut itheta = 0;
        if steps != 1 {
            let qn = steps as u32;
            let mut index = ((wanted * steps + EIGHTH_TURN) / QUARTER_TURN) as u32;
            if C::IS_ENCODER && !is_stereo && self.does_avoid_split_noise && index > 0 && index < qn {
                // An angle that would leave one half bits for noise alone goes all the way to the other.
                let unquantized = index as i32 * QUARTER_TURN / steps;
                let (imid, iside) = (cosine(unquantized), cosine(QUARTER_TURN - unquantized));
                let delta = frac_mul16((n as i32 - 1) << 7, log2_tangent(iside, imid));
                if delta > bits {
                    index = qn;
                } else if delta < -bits {
                    index = 0;
                }
            }
            let found = if is_stereo && n > 2 {
                let middle = qn / 2;
                let total = STEP_WEIGHT * (middle + 1) + middle;
                let below = (middle + 1) * STEP_WEIGHT;
                let x = match self.range.locate(total) {
                    Some(found) if found < below => found / STEP_WEIGHT,
                    Some(found) => middle + 1 + found - below,
                    None => index,
                };
                let (low, high) = if x <= middle {
                    (STEP_WEIGHT * x, STEP_WEIGHT * (x + 1))
                } else {
                    (x - 1 - middle + below, x - middle + below)
                };
                self.range.take(low, high, total);
                x
            } else if b0 > 1 || is_stereo {
                self.range.uint(index, qn + 1)
            } else {
                let half = qn >> 1;
                let total = (half + 1) * (half + 1);
                let rising = |x: u32| (x, x + 1, (x * (x + 1)) >> 1);
                let falling = |x: u32| (x, qn + 1 - x, total - (((qn + 1 - x) * (qn + 2 - x)) >> 1));
                let (x, size, low) = match self.range.locate(total) {
                    Some(found) if found < (half * (half + 1)) >> 1 => rising(((8 * found + 1).isqrt() - 1) >> 1),
                    Some(found) => falling((2 * (qn + 1) - (8 * (total - found - 1) + 1).isqrt()) >> 1),
                    None if index <= half => rising(index),
                    None => falling(index),
                };
                self.range.take(low, low + size, total);
                x
            };
            itheta = found as i32 * QUARTER_TURN / steps;
        } else if is_stereo && bits > INVERSION_FLOOR && self.remaining > INVERSION_FLOOR {
            // The inversion of the side, which the decoder does not make: it mixes the channels into one (RFC 8251 10).
            self.range.bit(false, 2);
        }
        let qalloc = self.range.tell_frac() as i32 - tell;
        let (imid, iside, delta) = match itheta {
            0 => (Q15_ONE, 0, -QUARTER_TURN),
            QUARTER_TURN => (0, Q15_ONE, QUARTER_TURN),
            _ => {
                let (imid, iside) = (cosine(itheta), cosine(QUARTER_TURN - itheta));
                (imid, iside, frac_mul16((n as i32 - 1) << 7, log2_tangent(iside, imid)))
            }
        };
        Theta { itheta, qalloc, mid: imid as f32 / Q15, side: iside as f32 / Q15, delta }
    }

    // Keeps the blocks that a split all to one half leaves to fold into.
    fn fill_of(fill: u32, blocks: usize, itheta: i32) -> u32 {
        let mask = (1 << blocks) - 1;
        match itheta {
            0 => fill & mask,
            QUARTER_TURN => fill & mask << blocks,
            _ => fill,
        }
    }

    // A part of a band of one channel: split in halves while its bits are more than its largest codebook takes, else its pulses, or else its folding (quant_band below level 0).
    fn partition(&mut self, x: &mut [f32], part: Part, lowband: Option<&mut [f32]>) -> u32 {
        let n = x.len();
        let Part { bits, blocks, lm, gain, fill } = part;
        let is_split = lm != -1 && n > 2 && {
            let cache = cache(self.band, lm);
            bits > i32::from(cache[usize::from(cache[0])]) + SPLIT_MARGIN
        };
        if is_split {
            let half = n / 2;
            let lm = lm - 1;
            let fill = if blocks == 1 { (fill & 1) | (fill << 1) } else { fill };
            let halves = (blocks + 1) >> 1;
            let (first, second) = x.split_at_mut(half);
            let theta = self.theta(first, second, bits, blocks, lm, false);
            let bits = bits - theta.qalloc;
            let fill = Self::fill_of(fill, halves, theta.itheta);
            let mut delta = theta.delta;
            if blocks > 1 && theta.itheta != 0 && theta.itheta != QUARTER_TURN {
                if theta.itheta > EIGHTH_TURN {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = (delta + ((half as i32) << FRACTION_BITS >> (5 - lm))).min(0);
                }
            }
            let mut mbits = bits.min((bits - delta) / 2).max(0);
            let mut sbits = bits - mbits;
            self.remaining -= theta.qalloc;
            let (first_low, second_low) = match lowband {
                Some(lowband) => {
                    let (a, b) = lowband.split_at_mut(half);
                    (Some(a), Some(b))
                }
                None => (None, None),
            };
            let mid = Part { bits: mbits, blocks: halves, lm, gain: gain * theta.mid, fill };
            let side = Part { bits: sbits, blocks: halves, lm, gain: gain * theta.side, fill: fill >> halves };
            let before = self.remaining;
            if mbits >= sbits {
                let mut mask = self.partition(first, mid, first_low);
                let rebalance = mbits - (before - self.remaining);
                if rebalance > REBALANCE_FLOOR && theta.itheta != 0 {
                    sbits += rebalance - REBALANCE_FLOOR;
                }
                mask |= self.partition(second, Part { bits: sbits, ..side }, second_low) << (blocks >> 1);
                mask
            } else {
                let mut mask = self.partition(second, side, second_low) << (blocks >> 1);
                let rebalance = sbits - (before - self.remaining);
                if rebalance > REBALANCE_FLOOR && theta.itheta != QUARTER_TURN {
                    mbits += rebalance - REBALANCE_FLOOR;
                }
                mask |= self.partition(first, Part { bits: mbits, ..mid }, first_low);
                mask
            }
        } else {
            let mut pseudo = bits_to_pulses(self.band, lm, bits);
            let mut cost = pulses_to_bits(self.band, lm, pseudo);
            self.remaining -= cost;
            while self.remaining < 0 && pseudo > 0 {
                self.remaining += cost;
                pseudo -= 1;
                cost = pulses_to_bits(self.band, lm, pseudo);
                self.remaining -= cost;
            }
            if pseudo != 0 {
                return pvq::code(self.range, x, pulses(pseudo), self.spread, blocks, gain);
            }
            if C::IS_ENCODER {
                // A part of no pulses is the decoder's to fill.
                return 0;
            }
            let mask = (1 << blocks) - 1;
            let fill = fill & mask;
            if fill == 0 {
                x.fill(0.0);
                return 0;
            }
            let collapse = match lowband {
                None => {
                    noise(x, &mut self.seed);
                    mask
                }
                Some(lowband) => {
                    for (value, &folded) in x.iter_mut().zip(lowband.iter()) {
                        let noise =
                            if next_random(&mut self.seed) & NOISE_SIGN != 0 { FOLD_NOISE } else { -FOLD_NOISE };
                        *value = folded + noise;
                    }
                    fill
                }
            };
            pvq::renormalise(x, gain);
            collapse
        }
    }

    // A band of one channel, its resolution in time and frequency changed by tf_change around its partition, and its shape kept for folding into the bands above (quant_band at level 0).
    fn mono(
        &mut self,
        x: &mut [f32],
        part: Part,
        mut lowband: Option<&mut [f32]>,
        lowband_out: Option<&mut [f32]>,
    ) -> u32 {
        let n0 = x.len();
        if n0 == 1 {
            return self.single(x, None, lowband_out);
        }
        let Part { mut blocks, mut fill, .. } = part;
        let is_long = blocks == 1;
        let mut tf_change = self.tf_change;
        let recombine = tf_change.max(0) as usize;
        let mut n_b = n0 / blocks;
        for k in 0..recombine {
            if C::IS_ENCODER {
                haar(x, n0 >> k, 1 << k);
            }
            if let Some(lowband) = lowband.as_deref_mut() {
                haar(lowband, n0 >> k, 1 << k);
            }
            fill = u32::from(BIT_INTERLEAVE[(fill & 0xF) as usize])
                | u32::from(BIT_INTERLEAVE[(fill >> 4 & 0xF) as usize]) << 2;
        }
        blocks >>= recombine;
        n_b <<= recombine;
        let mut time_divide = 0;
        while n_b & 1 == 0 && tf_change < 0 {
            if C::IS_ENCODER {
                haar(x, n_b, blocks);
            }
            if let Some(lowband) = lowband.as_deref_mut() {
                haar(lowband, n_b, blocks);
            }
            fill |= fill << blocks;
            blocks <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf_change += 1;
        }
        let (b0, n_b0) = (blocks, n_b);
        if b0 > 1 && C::IS_ENCODER {
            deinterleave(x, n_b >> recombine, b0 << recombine, is_long);
        }
        if b0 > 1
            && let Some(lowband) = lowband.as_deref_mut()
        {
            deinterleave(lowband, n_b >> recombine, b0 << recombine, is_long);
        }
        let mut mask = self.partition(x, Part { blocks, fill, ..part }, lowband);
        if C::IS_ENCODER {
            // An encoder does not make the shapes it coded: it folds no band from them.
            return mask;
        }
        if b0 > 1 {
            interleave(x, n_b0 >> recombine, b0 << recombine, is_long);
        }
        let (mut n_b, mut blocks) = (n_b0, b0);
        for _ in 0..time_divide {
            blocks >>= 1;
            n_b <<= 1;
            mask |= mask >> blocks;
            haar(x, n_b, blocks);
        }
        for k in 0..recombine {
            mask = u32::from(BIT_DEINTERLEAVE[(mask & 0xF) as usize]);
            haar(x, n0 >> k, 1 << k);
        }
        blocks <<= recombine;
        if let Some(out) = lowband_out {
            let scale = sqrt(n0 as f32);
            for (out, &value) in out.iter_mut().zip(x.iter()) {
                *out = scale * value;
            }
        }
        mask & ((1 << blocks) - 1)
    }

    // A band of two channels coded as mid and side, at an angle between them (quant_band with two channels).
    fn stereo(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        part: Part,
        lowband: Option<&mut [f32]>,
        lowband_out: Option<&mut [f32]>,
    ) -> u32 {
        let n = x.len();
        if n == 1 {
            return self.single(x, Some(y), lowband_out);
        }
        let Part { bits, blocks, lm, fill, .. } = part;
        let theta = self.theta(x, y, bits, blocks, lm, true);
        let bits = bits - theta.qalloc;
        if n == 2 {
            let sbits = if theta.itheta != 0 && theta.itheta != QUARTER_TURN { BIT } else { 0 };
            let mbits = bits - sbits;
            self.remaining -= theta.qalloc + sbits;
            let mid = Part { bits: mbits, ..part };
            let (first, second) = if theta.itheta > EIGHTH_TURN { (&mut *y, &mut *x) } else { (&mut *x, &mut *y) };
            let is_negative = first[0] * second[1] - first[1] * second[0] < 0.0;
            let sign = if sbits != 0 && self.range.bits(u32::from(is_negative), 1) == 1 { -1.0 } else { 1.0 };
            let mask = self.mono(first, mid, lowband, lowband_out);
            (second[0], second[1]) = (-sign * first[1], sign * first[0]);
            if !C::IS_ENCODER {
                for at in 0..2 {
                    let (a, b) = (theta.mid * x[at], theta.side * y[at]);
                    (x[at], y[at]) = (a - b, a + b);
                }
            }
            return mask;
        }
        let fill = Self::fill_of(fill, blocks, theta.itheta);
        let mut mbits = bits.min((bits - theta.delta) / 2).max(0);
        let mut sbits = bits - mbits;
        self.remaining -= theta.qalloc;
        let mid = Part { bits: mbits, gain: 1.0, fill, ..part };
        let side = Part { bits: sbits, gain: theta.side, fill: fill >> blocks, ..part };
        let before = self.remaining;
        let mask = if mbits >= sbits {
            let mask = self.mono(x, mid, lowband, lowband_out);
            let rebalance = mbits - (before - self.remaining);
            if rebalance > REBALANCE_FLOOR && theta.itheta != 0 {
                sbits += rebalance - REBALANCE_FLOOR;
            }
            mask | self.mono(y, Part { bits: sbits, ..side }, None, None)
        } else {
            let mask = self.mono(y, side, None, None);
            let rebalance = sbits - (before - self.remaining);
            if rebalance > REBALANCE_FLOOR && theta.itheta != QUARTER_TURN {
                mbits += rebalance - REBALANCE_FLOOR;
            }
            mask | self.mono(x, Part { bits: mbits, ..mid }, lowband, lowband_out)
        };
        if !C::IS_ENCODER {
            merge(x, y, theta.mid);
        }
        mask
    }
}

// The change of each band's resolution in time and frequency, an encoder's by its `wanted` flags and select (tf_decode, tf_encode, 4.3.4.5).
pub(super) fn code_tf<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    wanted: &[usize; BANDS],
    wanted_select: bool,
) -> [i32; BANDS] {
    let Frame { start, end, lm, is_transient, .. } = *frame;
    let [first_logp, logp] = TF_LOGP[usize::from(is_transient)];
    let mut budget = coder.len() as u32 * u8::BITS;
    let mut tell = coder.tell();
    let has_select = lm > 0 && tell + first_logp < budget;
    budget -= u32::from(has_select);
    let (mut current, mut changed) = (0, 0);
    let mut tf = [0usize; BANDS];
    for (band, tf) in tf.iter_mut().enumerate().take(end).skip(start) {
        let flag_logp = if band == start { first_logp } else { logp };
        if tell + flag_logp <= budget {
            current ^= usize::from(coder.bit(wanted[band] != current, flag_logp));
            tell = coder.tell();
            changed |= current;
        }
        *tf = current;
    }
    let row = &TF_SELECT[lm];
    let base = 4 * usize::from(is_transient);
    let tf_select = has_select && row[base + changed] != row[base + 2 + changed] && coder.bit(wanted_select, 1);
    core::array::from_fn(|band| i32::from(row[base + 2 * usize::from(tf_select) + tf[band]]))
}

// Copies the shape folding starts from out of the frame's earlier bands, so that the bands written to it stay apart.
fn fold_from<'f>(fold: &'f mut [f32; WIDEST], norm: &[f32], at: Option<usize>, width: usize) -> Option<&'f mut [f32]> {
    let at = at?;
    fold[..width].copy_from_slice(&norm[at..at + width]);
    Some(&mut fold[..width])
}

// The normalised shapes of a frame's bands in `x`, a channel's bins after the other's: a decoder's read, with what the folding keeps of them in `norm` and the generator's state in `seed`; an encoder's, of one channel, written. The collapse masks of their blocks (quant_all_bands, 4.3.4).
pub(super) fn code<C: RangeCoder>(
    range: &mut C,
    frame: &Frame,
    allocation: &Allocation,
    total: i32,
    x: &mut [f32],
    norm: &mut [f32],
    seed: &mut u32,
) -> [u8; 2 * BANDS] {
    let Frame { start, end, lm, channels, is_transient, spread, tf_res } = *frame;
    let m = 1 << lm;
    let blocks = if is_transient { m } else { 1 };
    let n = x.len() / channels;
    let (x, y) = x.split_at_mut(n);
    let (norm, norm2) = norm.split_at_mut(norm.len() / 2);
    let mut coder = Coder {
        range,
        remaining: 0,
        seed: *seed,
        spread,
        intensity: allocation.intensity,
        band: start,
        tf_change: 0,
        does_avoid_split_noise: blocks > 1,
    };
    let mut collapse = [0u8; 2 * BANDS];
    let (mut balance, mut dual_stereo) = (allocation.balance, allocation.dual_stereo);
    let (mut lowband_offset, mut update_lowband) = (0, true);
    let mut fold = [0.0; WIDEST];
    for band in start..end {
        let (low, high) = (m * E_BANDS[band], m * E_BANDS[band + 1]);
        let width = high - low;
        let tell = coder.range.tell_frac() as i32;
        if band != start {
            balance -= tell;
        }
        coder.remaining = total - tell - 1;
        let bits = if band < allocation.coded_bands {
            let share = balance / (allocation.coded_bands - band).min(3) as i32;
            (coder.remaining + 1).min(allocation.pulses[band] + share).clamp(0, MOST_BAND_BITS)
        } else {
            0
        };
        // An encoder makes no shapes to fold from (quant_all_bands without resynthesis).
        if !C::IS_ENCODER
            && (low >= width + m * E_BANDS[start] || band == start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = band;
        }
        if band == start + 1 {
            // A band too narrow to fold the next from is repeated into it (RFC 8251 9).
            let first = m * (E_BANDS[start + 1] - E_BANDS[start]);
            let second = m * (E_BANDS[start + 2] - E_BANDS[start + 1]);
            let offset = m * E_BANDS[start];
            norm.copy_within(offset + 2 * first - second..offset + first, offset + first);
            if channels == 2 {
                norm2.copy_within(offset + 2 * first - second..offset + first, offset + first);
            }
        }
        coder.band = band;
        coder.tf_change = tf_res[band];
        let (mut x_cm, mut y_cm) = ((1 << blocks) - 1, (1 << blocks) - 1);
        let mut effective = None;
        if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || blocks > 1 || tf_res[band] < 0) {
            let at = (m * E_BANDS[start]).max((m * E_BANDS[lowband_offset]).saturating_sub(width));
            let mut fold_start = lowband_offset - 1;
            while m * E_BANDS[fold_start] > at {
                fold_start -= 1;
            }
            let mut fold_end = lowband_offset;
            while fold_end < band && m * E_BANDS[fold_end] < at + width {
                fold_end += 1;
            }
            (x_cm, y_cm) = (0, 0);
            for folded in fold_start..fold_end {
                x_cm |= u32::from(collapse[folded * channels]);
                y_cm |= u32::from(collapse[folded * channels + channels - 1]);
            }
            effective = Some(at);
        }
        if dual_stereo && band == allocation.intensity {
            dual_stereo = false;
            if !C::IS_ENCODER {
                for at in m * E_BANDS[start]..low {
                    norm[at] = 0.5 * (norm[at] + norm2[at]);
                }
            }
        }
        let part = Part { bits, blocks, lm: lm as i32, gain: 1.0, fill: x_cm | y_cm };
        if dual_stereo {
            let lowband = fold_from(&mut fold, norm, effective, width);
            let left = Part { bits: bits / 2, fill: x_cm, ..part };
            x_cm = coder.mono(&mut x[low..high], left, lowband, Some(&mut norm[low..high]));
            let lowband = fold_from(&mut fold, norm2, effective, width);
            let right = Part { bits: bits / 2, fill: y_cm, ..part };
            y_cm = coder.mono(&mut y[low..high], right, lowband, Some(&mut norm2[low..high]));
        } else {
            let lowband = fold_from(&mut fold, norm, effective, width);
            x_cm = if channels == 2 {
                coder.stereo(&mut x[low..high], &mut y[low..high], part, lowband, Some(&mut norm[low..high]))
            } else {
                coder.mono(&mut x[low..high], part, lowband, Some(&mut norm[low..high]))
            };
            y_cm = x_cm;
        }
        collapse[band * channels] = x_cm as u8;
        collapse[band * channels + channels - 1] = y_cm as u8;
        balance += allocation.pulses[band] + tell;
        update_lowband = bits > (width << FRACTION_BITS) as i32;
        coder.does_avoid_split_noise = false;
    }
    *seed = coder.seed;
    collapse
}

// Noise into the blocks of a transient frame's bands that came to none, at the least energy of the two frames before (anti_collapse, 4.3.5).
pub(super) fn anti_collapse(
    frame: &Frame,
    x: &mut [f32],
    collapse: &[u8; 2 * BANDS],
    [energies, last, before]: [&Energies; 3],
    pulses: &[i32; BANDS],
    mut seed: u32,
) {
    let Frame { start, end, lm, channels, .. } = *frame;
    let n = x.len() / channels;
    for band in start..end {
        let n0 = E_BANDS[band + 1] - E_BANDS[band];
        let depth = (1 + pulses[band]) / (n0 << lm) as i32;
        let thresh = 0.5 * pow2_f64(f64::from(-DEPTH_STEP * depth as f32)) as f32;
        let scale = 1.0 / sqrt((n0 << lm) as f32);
        for channel in 0..channels {
            let (mut prev1, mut prev2) = (last[channel][band], before[channel][band]);
            if channels == 1 {
                prev1 = prev1.max(last[1][band]);
                prev2 = prev2.max(before[1][band]);
            }
            let difference = (energies[channel][band] - prev1.min(prev2)).max(0.0);
            let mut size = 2.0 * pow2_f64(f64::from(-difference)) as f32;
            if lm == MAX_LM {
                size *= SQRT_2;
            }
            let size = size.min(thresh) * scale;
            let offset = channel * n + (E_BANDS[band] << lm);
            let bins = &mut x[offset..offset + (n0 << lm)];
            let mut is_filled = false;
            for block in 0..1 << lm {
                if collapse[band * channels + channel] & 1 << block == 0 {
                    for at in 0..n0 {
                        let sign = next_random(&mut seed) & NOISE_SIGN != 0;
                        bins[(at << lm) + block] = if sign { size } else { -size };
                    }
                    is_filled = true;
                }
            }
            if is_filled {
                pvq::renormalise(bins, 1.0);
            }
        }
    }
}

// The amplitude of each band of a channel's bins below `end`, never nought (compute_band_energies).
pub(super) fn measure(spectrum: &[f32], lm: usize, end: usize) -> [f32; BANDS] {
    core::array::from_fn(|band| {
        if band < end {
            let bins = &spectrum[E_BANDS[band] << lm..E_BANDS[band + 1] << lm];
            sqrt(LEAST_BAND_ENERGY + dot(bins, bins))
        } else {
            0.0
        }
    })
}

// A channel's normalised shapes from its bins and its bands' amplitudes, the bands below `end` (normalise_bands).
pub(super) fn normalise(spectrum: &[f32], amplitudes: &[f32; BANDS], lm: usize, end: usize, x: &mut [f32]) {
    for band in 0..end {
        let bins = E_BANDS[band] << lm..E_BANDS[band + 1] << lm;
        let gain = 1.0 / (LEAST_BAND_ENERGY + amplitudes[band]);
        for (shape, &bin) in x[bins.clone()].iter_mut().zip(&spectrum[bins]) {
            *shape = bin * gain;
        }
    }
}

// A channel's bins from its normalised shapes and its bands' amplitudes, nought from the end band up (denormalise_bands, 4.3.6).
pub(super) fn denormalise(x: &[f32], amplitudes: &[f32; BANDS], lm: usize, end: usize, spectrum: &mut [f32]) {
    for band in 0..end {
        let bins = E_BANDS[band] << lm..E_BANDS[band + 1] << lm;
        for (out, &shape) in spectrum[bins.clone()].iter_mut().zip(&x[bins]) {
            *out = shape * amplitudes[band];
        }
    }
    spectrum[E_BANDS[end] << lm..].fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitexact_cosine_and_tangent() {
        for angle in [64, 100, 4_096, 8_192, 12_000, 16_320] {
            let expected = (core::f64::consts::FRAC_PI_2 * f64::from(angle) / 16_384.0).cos() * 32_768.0;
            assert!((f64::from(cosine(angle)) - expected).abs() < 3.0, "the cosine of {angle}: {}", cosine(angle));
        }
        assert_eq!(
            (cosine(8_192), cosine(64)),
            (23_171, 32_767),
            "the reference's arithmetic at an eighth of a turn and at the smallest angle 256 steps give"
        );
        let (imid, iside) = (cosine(4_096), cosine(16_384 - 4_096));
        let tangent = f64::from(log2_tangent(iside, imid)) / 2_048.0;
        assert!((tangent - (f64::from(iside) / f64::from(imid)).log2()).abs() < 0.01, "log2 of the tangent: {tangent}");
        assert_eq!(log2_tangent(23_171, 23_171), 0, "an eighth of a turn has a tangent of one");
    }

    #[test]
    fn theta_steps_by_bits() {
        assert_eq!(theta_steps(8, 0, 4, 16, false), 1, "no bits, no angle");
        assert_eq!(theta_steps(8, 2_000, 30, 16, false), 256, "eight bits of resolution at most: 256 steps");
        assert_eq!(theta_steps(4, 100, 10, 8, false), 8, "170 eighths over 7 degrees of freedom: 3 bits, 8 steps");
        assert_eq!(
            theta_steps(4, 100, 10, 8, false),
            theta_steps(4, 103, 10, 8, false),
            "3 eighths more, the same steps"
        );
        assert_eq!(theta_steps(2, 60, 0, 8, true), 6, "a stereo pair of two bins has 2 degrees of freedom: 2.5 bits");
    }

    #[test]
    fn haar_and_interleave_invert() {
        let original: [f32; 16] = core::array::from_fn(|at| (at * 5 % 7) as f32 - 3.0);
        let mut x = original;
        haar(&mut x, 16, 1);
        haar(&mut x, 16, 1);
        assert!(x.iter().zip(&original).all(|(a, b)| (a - b).abs() < 1.0e-5), "one level of Haar is its own inverse");
        for (stride, is_hadamard) in [(2, false), (4, true), (8, true), (16, true), (4, false)] {
            let mut x = original;
            deinterleave(&mut x, 16 / stride, stride, is_hadamard);
            if stride == 4 && !is_hadamard {
                assert_eq!(x[..4], [original[0], original[4], original[8], original[12]], "block 0 first");
            }
            interleave(&mut x, 16 / stride, stride, is_hadamard);
            assert_eq!(x, original, "stride {stride}");
        }
        let mut x = original;
        deinterleave(&mut x, 8, 2, true);
        assert_eq!(
            x[..8],
            core::array::from_fn::<f32, 8, _>(|at| original[2 * at + 1]),
            "sequency order puts block 1 first"
        );
    }

    #[test]
    fn merge_mid_and_side() {
        let (mut x, mut y) = ([0.6, 0.8], [0.0, 0.0]);
        merge(&mut x, &mut y, 1.0);
        let norm = |v: &[f32]| v.iter().map(|a| a * a).sum::<f32>();
        assert!((norm(&x) - 1.0).abs() < 1.0e-5 && (norm(&y) - 1.0).abs() < 1.0e-5, "both channels of unit norm");
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1.0e-5), "no side: left and right alike");
        let (mut x, mut y) = ([1.0, 0.0], [0.0, 0.0]);
        merge(&mut x, &mut y, 0.0);
        assert_eq!(y, x, "a mid of nought leaves the right as the left");
    }

    #[test]
    fn shapes_written_and_read() {
        use super::super::allocation::{Choice, allocate, caps};
        use crate::codec::opus::range::{RangeDecoder, RangeEncoder};
        use alloc::vec;
        use alloc::vec::Vec;
        let mut state = 0x5EED_u32;
        let mut next = move || (next_random(&mut state) >> 16) as f32 / 32_768.0 - 1.0;
        for (is_transient, len) in [(false, 80), (true, 160), (false, 1275)] {
            let (lm, n) = (MAX_LM, 960);
            let mut shapes: Vec<f32> = (0..n).map(|_| next()).collect();
            for band in 0..BANDS {
                pvq::renormalise(&mut shapes[E_BANDS[band] << lm..E_BANDS[band + 1] << lm], 1.0);
            }
            let tf_res = if is_transient { [-1; BANDS] } else { [0; BANDS] };
            let frame = Frame { start: 0, end: BANDS, lm, channels: 1, is_transient, spread: 2, tf_res };
            let cap = caps(lm, 1);
            let choice = Choice { last_coded: 0, signal_end: BANDS - 1 };
            let total = (len * 8) as i32 * BIT;
            let mut encoder = RangeEncoder::new(len);
            let written = allocate(&mut encoder, &frame, &[0; BANDS], &cap, 5, total - 9, &choice);
            let (mut x, mut norm, mut seed) = (shapes.clone(), vec![0.0; 1600], 0);
            code(&mut encoder, &frame, &written, total, &mut x, &mut norm, &mut seed);
            let range = encoder.range();
            let bytes = encoder.finish().expect("the shapes fit the frame");
            let mut decoder = RangeDecoder::new(bytes);
            let read = allocate(&mut decoder, &frame, &[0; BANDS], &cap, 5, total - 9, &Choice::default());
            let case = alloc::format!("transient {is_transient}, {len} bytes");
            assert_eq!(read.coded_bands, written.coded_bands, "{case}: the allocation as written");
            let (mut y, mut norm, mut seed) = (vec![0.0; n], vec![0.0; 1600], 0);
            code(&mut decoder, &frame, &read, total, &mut y, &mut norm, &mut seed);
            assert_eq!(decoder.range(), range, "{case}: the decoder ends in the encoder's range");
            let coded = (0..read.coded_bands).filter(|&band| read.pulses[band] > 0);
            let similar: Vec<f32> = coded
                .map(|band| {
                    let bins = E_BANDS[band] << lm..E_BANDS[band + 1] << lm;
                    y[bins.clone()].iter().zip(&shapes[bins]).map(|(a, b)| a * b).sum::<f32>()
                })
                .collect();
            let mean = similar.iter().sum::<f32>() / similar.len() as f32;
            // Measured at 0.88 to 1.00 over the cases; the bound lies 0.11 under the least.
            assert!(mean > 0.77, "{case}: the shapes read lie near those written, {mean}");
        }
    }

    #[test]
    fn denormalised_by_band() {
        let x = [1.0f32; 120];
        let mut amplitudes = [0.0; BANDS];
        amplitudes[1] = 2.0;
        amplitudes[12] = 3.0;
        let mut spectrum = [9.0f32; 120];
        denormalise(&x, &amplitudes, 0, 13, &mut spectrum);
        assert_eq!((spectrum[0], spectrum[1], spectrum[16], spectrum[19]), (0.0, 2.0, 3.0, 3.0));
        assert!(spectrum[20..].iter().all(|&bin| bin == 0.0), "nought from the end band up");
    }
}
