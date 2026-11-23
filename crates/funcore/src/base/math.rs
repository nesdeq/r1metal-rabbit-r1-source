use core::f64::consts::{LN_2, SQRT_2, TAU};

const SQRT_GUESS_BIAS: u32 = 0x1FBD_1DF5;
const SQRT_STEPS: usize = 3;
const TRIG_TERMS: u32 = 12;
const TAN_SIXTEENTH: f64 = SQRT_2 - 1.0;
const ATAN_TERMS: u32 = 19;
const LOG2_PARABOLA: [f32; 3] = [-0.335_828_78, 2.0, -1.658_717_6];
const CBRT_STEPS: usize = 5;
const F32_EXPONENT_MAX: u32 = 0xFF;
const F32_EXPONENT_BIAS: i32 = 127;
const F32_FRACTION_BITS: u32 = 23;
const F64_EXPONENT_MAX: u64 = 0x7FF;
const F64_EXPONENT_BIAS: i64 = 1023;
const F64_FRACTION_BITS: u32 = 52;
const EXP_TERMS: u32 = 20;
const ATANH_TERMS: u32 = 17;
// Numerical Recipes' generator: CELT's noise (RFC 6716 4.3), AAC's bands of noise (4.6.13 leaves the generator open) and the tests' noise.
const RANDOM_MULTIPLIER: u32 = 1_664_525;
const RANDOM_INCREMENT: u32 = 1_013_904_223;
// The f32 lanes of a NEON vector, and the values a dot product takes at a time into two pairs of running sums of that many lanes: four chains of additions, which the A53 runs at 1.2 cycles a value against 1.4 for two and 7.6 for one (llvm-mca's Cortex-A53 model).
const LANES: usize = 4;
const HALF: usize = 2 * LANES;

// The sum of the products of `a`'s and `b`'s values as f32, as far as the shorter reaches, in one order of additions on every machine: lane by lane into four running sums over each 16 values, a last 8 into the first two and a last 4 into the first, the sums folded and the last up to 3 products added one by one.
pub(crate) fn dot<T: Copy + Into<f32>>(a: &[T], b: &[T]) -> f32 {
    let count = a.len().min(b.len());
    let (a, b) = (&a[..count], &b[..count]);
    let add = |sums: &mut [f32; LANES], a: &[T], b: &[T]| {
        for (lane, sum) in sums.iter_mut().enumerate() {
            *sum += a[lane].into() * b[lane].into();
        }
    };
    let add_half = |sums: &mut [[f32; LANES]; 2], a: &[T; HALF], b: &[T; HALF]| {
        add(&mut sums[0], &a[..LANES], &b[..LANES]);
        add(&mut sums[1], &a[LANES..], &b[LANES..]);
    };
    let mut sums = [[[0.0f32; LANES]; 2]; 2];
    let ((a_halves, a_rest), (b_halves, b_rest)) = (a.as_chunks::<HALF>(), b.as_chunks::<HALF>());
    let ((a_pairs, a_last), (b_pairs, b_last)) = (a_halves.as_chunks::<2>(), b_halves.as_chunks::<2>());
    for (a, b) in a_pairs.iter().zip(b_pairs) {
        add_half(&mut sums[0], &a[0], &b[0]);
        add_half(&mut sums[1], &a[1], &b[1]);
    }
    for (a, b) in a_last.iter().zip(b_last) {
        add_half(&mut sums[0], a, b);
    }
    let ((a_quads, a_tail), (b_quads, b_tail)) = (a_rest.as_chunks::<LANES>(), b_rest.as_chunks::<LANES>());
    for (a, b) in a_quads.iter().zip(b_quads) {
        add(&mut sums[0][0], a, b);
    }
    let [[first, second], [third, fourth]] = sums;
    let folded: [f32; LANES] = core::array::from_fn(|lane| (first[lane] + third[lane]) + (second[lane] + fourth[lane]));
    let total = (folded[0] + folded[1]) + (folded[2] + folded[3]);
    a_tail.iter().zip(b_tail).fold(total, |sum, (&x, &y)| sum + x.into() * y.into())
}

pub(crate) fn round(value: f32) -> i32 {
    let shifted = value + 0.5;
    let truncated = shifted as i32;
    truncated - i32::from(truncated as f32 > shifted)
}

// A computed sample as 16 bits: the nearest, halves away from nought, and no further than 16 bits reach.
pub(crate) fn to_sample(value: f32) -> i16 {
    (value + if value < 0.0 { -0.5 } else { 0.5 }) as i16
}

pub(crate) fn floor(value: f32) -> f32 {
    let truncated = value as i32 as f32;
    truncated - if truncated > value { 1.0 } else { 0.0 }
}

pub(crate) fn ceil(value: f32) -> f32 {
    -floor(-value)
}

pub(crate) const fn sqrt(value: f32) -> f32 {
    if value < f32::MIN_POSITIVE {
        return 0.0;
    }
    let (mut root, mut step) = (f32::from_bits((value.to_bits() >> 1) + SQRT_GUESS_BIAS), 0);
    while step < SQRT_STEPS {
        root = 0.5 * (root + value / root);
        step += 1;
    }
    root
}

pub(crate) fn log2(value: f32) -> f32 {
    let bits = value.to_bits();
    let exponent = (bits >> F32_FRACTION_BITS & F32_EXPONENT_MAX) as i32 - F32_EXPONENT_BIAS;
    let unit = (F32_EXPONENT_BIAS as u32) << F32_FRACTION_BITS;
    let fraction = f32::from_bits(bits & !(F32_EXPONENT_MAX << F32_FRACTION_BITS) | unit);
    let [square, linear, constant] = LOG2_PARABOLA;
    exponent as f32 + (square * fraction + linear) * fraction + constant
}

pub(crate) fn pow2(power: i32) -> f32 {
    let exponent = (power + F32_EXPONENT_BIAS).clamp(1, F32_EXPONENT_MAX as i32 - 1);
    f32::from_bits((exponent as u32) << F32_FRACTION_BITS)
}

fn exp_small(power: f64) -> f64 {
    let (mut term, mut sum) = (1.0, 0.0);
    for step in 1..=EXP_TERMS {
        sum += term;
        term *= power / f64::from(step);
    }
    sum
}

pub(crate) fn pow2_f64(power: f64) -> f64 {
    let truncated = power as i64;
    let whole = truncated - i64::from(truncated as f64 > power);
    let exponent = (whole + F64_EXPONENT_BIAS).clamp(1, F64_EXPONENT_MAX as i64 - 1) as u64;
    exp_small((power - whole as f64) * LN_2) * f64::from_bits(exponent << F64_FRACTION_BITS)
}

// The base-2 logarithm of a positive normal number: its exponent, and its fraction's by the series of 2 atanh((m - 1) / (m + 1)).
pub(crate) fn log2_f64(value: f64) -> f64 {
    let bits = value.to_bits();
    let exponent = (bits >> F64_FRACTION_BITS & F64_EXPONENT_MAX) as i64 - F64_EXPONENT_BIAS;
    let unit = (F64_EXPONENT_BIAS as u64) << F64_FRACTION_BITS;
    let fraction = f64::from_bits(bits & !(F64_EXPONENT_MAX << F64_FRACTION_BITS) | unit);
    let step = (fraction - 1.0) / (fraction + 1.0);
    let square = step * step;
    let (mut power, mut sum) = (step, 0.0);
    for term in 0..ATANH_TERMS {
        sum += power / f64::from(2 * term + 1);
        power *= square;
    }
    exponent as f64 + 2.0 * sum / LN_2
}

pub(crate) fn power_f64(base: f64, exponent: f64) -> f64 {
    if base <= 0.0 { 0.0 } else { pow2_f64(exponent * log2_f64(base)) }
}

const fn sine_cosine_series(angle: f64) -> (f64, f64) {
    let square = angle * angle;
    let (mut sine, mut cosine) = (0.0, 0.0);
    let (mut odd_term, mut even_term, mut term) = (angle, 1.0, 0);
    while term < TRIG_TERMS {
        sine += odd_term;
        cosine += even_term;
        odd_term *= -square / ((2 * term + 2) * (2 * term + 3)) as f64;
        even_term *= -square / ((2 * term + 1) * (2 * term + 2)) as f64;
        term += 1;
    }
    (sine, cosine)
}

pub(crate) const fn sine_cosine(turns: f64) -> (f64, f64) {
    let fraction = turns - turns as i64 as f64;
    let wrapped = if fraction < 0.0 { fraction + 1.0 } else { fraction };
    // A fraction a hair under none comes to a whole turn once one is added.
    let turn = if wrapped < 1.0 { wrapped } else { 0.0 };
    let quarter = (turn * 4.0) as u32 % 4;
    let (sine, cosine) = sine_cosine_series((turn - quarter as f64 / 4.0) * TAU);
    match quarter {
        0 => (sine, cosine),
        1 => (cosine, -sine),
        2 => (-sine, -cosine),
        _ => (-cosine, sine),
    }
}

// Its own rounding draws the robot's outline, which `sine_cosine` would move by a bit on most angles.
pub(crate) fn short_sine_cosine(angle: f32) -> (f32, f32) {
    let square = angle * angle;
    (angle * (1.0 - square / 6.0 + square * square / 120.0), 1.0 - square / 2.0 + square * square / 24.0)
}

fn atan_turns(tangent: f64) -> f64 {
    let (eighths, reduced) =
        if tangent > TAN_SIXTEENTH { (1.0, (tangent - 1.0) / (tangent + 1.0)) } else { (0.0, tangent) };
    let square = reduced * reduced;
    let (mut sum, mut power) = (0.0, reduced);
    for term in 0..ATAN_TERMS {
        sum += power / f64::from(2 * term + 1);
        power *= -square;
    }
    eighths / 8.0 + sum / TAU
}

pub(crate) fn atan2_turns(x: f64, y: f64) -> f64 {
    let (abs_x, abs_y) = (x.abs(), y.abs());
    let longest = abs_x.max(abs_y);
    if longest == 0.0 {
        return 0.0;
    }
    let is_steep = abs_x > abs_y;
    let arc = atan_turns(abs_x.min(abs_y) / longest);
    let in_quadrant = if is_steep { 0.25 - arc } else { arc };
    let in_half = if y < 0.0 { 0.5 - in_quadrant } else { in_quadrant };
    if x < 0.0 { -in_half } else { in_half }
}

pub(crate) const fn cbrt(value: f64, guess: f64) -> f64 {
    let (mut root, mut step) = (guess, 0);
    while step < CBRT_STEPS {
        root = (2.0 * root + value / (root * root)) / 3.0;
        step += 1;
    }
    root
}

// The modified Bessel function of the first kind of order 0 as its series, summed until a term no longer changes the sum.
pub(crate) const fn bessel(x: f64) -> f64 {
    let quarter = x * x / 4.0;
    let (mut term, mut sum, mut order) = (1.0, 1.0, 1.0);
    while term > sum * f64::EPSILON {
        term *= quarter / (order * order);
        sum += term;
        order += 1.0;
    }
    sum
}

// Over half a turn the midpoint rule sums the integral to the last bits of an f64, for a function of a whole period.
#[cfg(test)]
pub(crate) fn bessel_by_integral(x: f64) -> f64 {
    let steps = 4000;
    let angle = |step: i32| core::f64::consts::PI * (f64::from(step) + 0.5) / f64::from(steps);
    (0..steps).map(|step| (x * angle(step).cos()).exp()).sum::<f64>() / f64::from(steps)
}

// The const functions above written with iterators and trait calls: the tests hold the const forms, and the tables made with them, to these bit for bit.
#[cfg(test)]
pub(crate) mod iterated {
    use super::{CBRT_STEPS, SQRT_GUESS_BIAS, SQRT_STEPS, TAU, TRIG_TERMS};

    pub(crate) fn sqrt(value: f32) -> f32 {
        if value < f32::MIN_POSITIVE {
            return 0.0;
        }
        let guess = f32::from_bits((value.to_bits() >> 1) + SQRT_GUESS_BIAS);
        (0..SQRT_STEPS).fold(guess, |root, _| 0.5 * (root + value / root))
    }

    fn sine_cosine_series(angle: f64) -> (f64, f64) {
        let square = angle * angle;
        let (mut sine, mut cosine) = (0.0, 0.0);
        let (mut odd_term, mut even_term) = (angle, 1.0);
        for term in 0..TRIG_TERMS {
            sine += odd_term;
            cosine += even_term;
            odd_term *= -square / f64::from((2 * term + 2) * (2 * term + 3));
            even_term *= -square / f64::from((2 * term + 1) * (2 * term + 2));
        }
        (sine, cosine)
    }

    pub(crate) fn sine_cosine(turns: f64) -> (f64, f64) {
        let fraction = turns - turns as i64 as f64;
        let wrapped = if fraction < 0.0 { fraction + 1.0 } else { fraction };
        let turn = if wrapped < 1.0 { wrapped } else { 0.0 };
        let quarter = (turn * 4.0) as u32 % 4;
        let (sine, cosine) = sine_cosine_series((turn - f64::from(quarter) / 4.0) * TAU);
        match quarter {
            0 => (sine, cosine),
            1 => (cosine, -sine),
            2 => (-sine, -cosine),
            _ => (-cosine, sine),
        }
    }

    pub(crate) fn cbrt(value: f64, guess: f64) -> f64 {
        (0..CBRT_STEPS).fold(guess, |root, _| (2.0 * root + value / (root * root)) / 3.0)
    }
}

pub(crate) const fn xorshift64(state: u64) -> u64 {
    let first = state ^ state << 13;
    let second = first ^ first >> 7;
    second ^ second << 17
}

pub(crate) const fn xorshift32(state: u32) -> u32 {
    let first = state ^ state << 13;
    let second = first ^ first >> 17;
    second ^ second << 5
}

pub(crate) fn next_random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(RANDOM_MULTIPLIER).wrapping_add(RANDOM_INCREMENT);
    *state
}

#[derive(Clone, Copy)]
pub(crate) struct Scale {
    pub(crate) min: u8,
    pub(crate) max: u8,
    pub(crate) step: u8,
}

impl Scale {
    pub(crate) fn nearest(self, percent: u32) -> u8 {
        let step = u32::from(self.step);
        let snapped = percent.saturating_add(step / 2) / step * step;
        snapped.clamp(u32::from(self.min), u32::from(self.max)) as u8
    }

    pub(crate) fn step_from(self, from: u8, steps: i32) -> u8 {
        let percent = i64::from(from) + i64::from(steps) * i64::from(self.step);
        self.nearest(percent.clamp(0, i64::from(self.max)) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn sqrt_precision() {
        for (value, root) in [(4.0f32, 2.0f32), (2.0, core::f32::consts::SQRT_2), (1.0e12, 1.0e6), (1.0e-12, 1.0e-6)] {
            assert!((sqrt(value) / root - 1.0).abs() < 2.0e-7, "{value}: {}", sqrt(value));
        }
        assert_eq!((sqrt(0.0), sqrt(-1.0), sqrt(1.0e-40)), (0.0, 0.0, 0.0));
    }

    #[test]
    fn log2_precision() {
        for (value, logarithm) in
            [(1.0f32, 0.0f32), (2.0, 1.0), (3.0, 1.584_962_5), (1000.0, 9.965_784), (0.1, -core::f32::consts::LOG2_10)]
        {
            assert!((log2(value) - logarithm).abs() < 0.01, "{value}: {}", log2(value));
        }
    }

    #[test]
    fn pow2_f64_precision() {
        use core::f64::consts::{FRAC_1_SQRT_2, LOG2_10};
        for (power, value) in [(0.0, 1.0), (1.0, 2.0), (-1.0, 0.5), (-0.5, FRAC_1_SQRT_2), (LOG2_10, 10.0)] {
            assert!((pow2_f64(power) / value - 1.0).abs() < 1.0e-15, "{power}: {}", pow2_f64(power));
        }
        for whole in [-1000, -300, 300, 1000] {
            assert_eq!(pow2_f64(f64::from(whole)), 2f64.powi(whole), "{whole}");
        }
    }

    #[test]
    fn log2_f64_precision() {
        for (value, logarithm) in
            [(1.0, 0.0), (8.0, 3.0), (3.0, 1.584_962_500_721_156), (0.1, -core::f64::consts::LOG2_10)]
        {
            assert!((log2_f64(value) - logarithm).abs() < 1.0e-14, "{value}: {}", log2_f64(value));
        }
        assert!(
            (power_f64(0.18, 1.0 / 2.4) - 0.489_437_089_573_878_3).abs() < 1.0e-13,
            "{}",
            power_f64(0.18, 1.0 / 2.4)
        );
        assert_eq!(power_f64(0.0, 2.4), 0.0);
    }

    #[test]
    fn floor_and_ceil() {
        assert_eq!([2.5, -2.5, 3.0, -0.0, 0.25].map(floor), [2.0, -3.0, 3.0, 0.0, 0.0]);
        assert_eq!([2.5, -2.5, 3.0, -0.25].map(ceil), [3.0, -2.0, 3.0, 0.0]);
        assert_eq!(
            [3.0e9, -3.0e9, f32::INFINITY, f32::NEG_INFINITY, f32::NAN].map(floor),
            [2_147_483_648.0, -2_147_483_648.0, 2_147_483_648.0, -2_147_483_648.0, 0.0]
        );
    }

    #[test]
    fn samples_rounded() {
        assert_eq!([0.4, 0.5, 1.5, -0.4, -0.5, -1.5].map(to_sample), [0, 1, 2, 0, -1, -2], "halves away from nought");
        assert_eq!(
            [40_000.0, -40_000.0, 32_767.4, -32_768.4, f32::NAN].map(to_sample),
            [i16::MAX, i16::MIN, i16::MAX, i16::MIN, 0],
            "what lies past 16 bits is the largest there is"
        );
    }

    #[test]
    fn pow2_clamps_to_normal_floats() {
        assert_eq!((pow2(0), pow2(1), pow2(-2), pow2(10)), (1.0, 2.0, 0.25, 1024.0));
        assert_eq!((pow2(-200), pow2(200)), (f32::MIN_POSITIVE, f32::from_bits(0x7F00_0000)));
    }

    #[test]
    fn sine_cosine_by_turns() {
        let half = core::f64::consts::FRAC_1_SQRT_2;
        for (turns, sine, cosine) in [
            (0.0, 0.0, 1.0),
            (0.125, half, half),
            (0.25, 1.0, 0.0),
            (0.375, half, -half),
            (0.5, 0.0, -1.0),
            (0.75, -1.0, 0.0),
            (-0.125, -half, half),
            (3.125, half, half),
            (1.0 / 12.0, 0.5, 0.866_025_403_784_438_6),
            (-1.0e-18, 0.0, 1.0),
        ] {
            let (actual_sine, actual_cosine) = sine_cosine(turns);
            assert!((actual_sine - sine).abs() < 1.0e-14 && (actual_cosine - cosine).abs() < 1.0e-14, "{turns}");
        }
    }

    #[test]
    fn atan2_inverts_sine_cosine() {
        for step in -499..=500 {
            let turn = f64::from(step) / 1000.0;
            let (sine, cosine) = sine_cosine(turn);
            for length in [1.0, 9807.0, 1.0e-3] {
                let angle = atan2_turns(length * sine, length * cosine);
                assert!((angle - turn).abs() < 1.0e-15, "{turn} at {length}: {angle}");
            }
        }
        assert_eq!(
            (
                atan2_turns(0.0, 1.0),
                atan2_turns(1.0, 0.0),
                atan2_turns(0.0, -1.0),
                atan2_turns(-1.0, 0.0),
                atan2_turns(0.0, 0.0)
            ),
            (0.0, 0.25, 0.5, -0.25, 0.0)
        );
        assert!(
            (atan2_turns(1.0, 1.0) - 0.125).abs() < 1.0e-16
                && (atan2_turns(TAN_SIXTEENTH, 1.0) - 0.0625).abs() < 1.0e-16
        );
    }

    #[test]
    fn cbrt_from_previous_root() {
        let mut root = 1.0;
        for whole in 1..=8191u32 {
            root = cbrt(f64::from(whole), root);
            assert!((root * root * root / f64::from(whole) - 1.0).abs() < 1.0e-13, "{whole}");
        }
        assert!((cbrt(27.0, 2.0) - 3.0).abs() < 1.0e-9);
    }

    #[test]
    fn const_forms_bit_exact() {
        let bits = |(sine, cosine): (f64, f64)| (sine.to_bits(), cosine.to_bits());
        let mut state = 1u32;
        for _ in 0..100_000 {
            state = xorshift32(state);
            let value = f32::from_bits(state % f32::INFINITY.to_bits());
            for value in [value, -value] {
                assert_eq!(sqrt(value).to_bits(), iterated::sqrt(value).to_bits(), "the root of {value}");
            }
            let turns = f64::from(state as i32) / 65_536.0;
            assert_eq!(bits(sine_cosine(turns)), bits(iterated::sine_cosine(turns)), "{turns} turns");
            let (value, guess) = (f64::from(state), f64::from(state >> 21) + 1.0);
            assert_eq!(cbrt(value, guess).to_bits(), iterated::cbrt(value, guess).to_bits(), "{value} from {guess}");
        }
    }

    #[test]
    fn bessel_series_matches_integral() {
        for x in [0.0, 0.5, 3.0, 12.566_370_614_359_172, 18.849_555_921_538_76] {
            let (series, integral) = (bessel(x), bessel_by_integral(x));
            assert!((series / integral - 1.0).abs() < 1.0e-12, "{x}: {series} for {integral}");
        }
        assert_eq!(bessel(0.0), 1.0);
    }

    #[test]
    fn dot_counts_each_product_once() {
        for count in [0usize, 1, 3, 4, 7, 8, 12, 15, 16, 20, 24, 27, 28, 31, 32, 88, 100] {
            let values: Vec<f32> = (1..=count).map(|value| value as f32).collect();
            let squares = (count * (count + 1) * (2 * count + 1) / 6) as f32;
            assert_eq!(dot(&values, &values), squares, "{count} values: whole numbers this small sum exactly");
            assert_eq!(dot(&values, &values[..count / 2]), dot(&values[..count / 2], &values), "the shorter's length");
        }
        let mut state = 9u32;
        let samples: Vec<i16> = (0..1000)
            .map(|_| {
                state = xorshift32(state);
                state as i16
            })
            .collect();
        let floats: Vec<f32> = samples.iter().map(|&sample| f32::from(sample)).collect();
        for count in [5, 37, 999] {
            let (whole, rounded) = (dot(&samples[..count], &samples[1..]), dot(&floats[..count], &floats[1..]));
            assert_eq!(whole.to_bits(), rounded.to_bits(), "{count} samples as they are and as f32, bit for bit");
            let exact: f64 = (0..count).map(|at| f64::from(samples[at]) * f64::from(samples[at + 1])).sum();
            let scale: f64 = (0..count).map(|at| (f64::from(samples[at]) * f64::from(samples[at + 1])).abs()).sum();
            assert!((f64::from(whole) - exact).abs() <= scale * 1.0e-6, "{count} samples: {whole} for {exact}");
        }
    }

    #[test]
    fn xorshift32_sequence() {
        assert_eq!((xorshift32(1), xorshift32(270_369), xorshift32(0)), (270_369, 67_634_689, 0));
    }

    #[test]
    fn next_random_sequence() {
        let mut state = 0;
        let values = [next_random(&mut state), next_random(&mut state), next_random(&mut state)];
        assert_eq!(values, [1_013_904_223, 1_196_435_762, 3_519_870_697], "1664525 times the state plus 1013904223");
        assert_eq!(state, 3_519_870_697, "the state is the last value");
    }
}
