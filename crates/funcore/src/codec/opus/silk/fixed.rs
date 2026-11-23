// The fixed-point operations of the reference decoder (RFC 6716 Appendix A, silk/SigProc_FIX.h, macros.h and Inlines.h), on two's complement values that wrap where they leave 32 bits.

// The bits of a 32-bit word and of its halves, which the products are taken by.
const HALF_WORD: u32 = 16;
const WORD: u32 = 32;
// The linear congruential generator of 4.2.7.8.6.
const LCG_MULTIPLIER: i32 = 196_314_165;
const LCG_INCREMENT: i32 = 907_633_515;
// silk_SQRT_APPROX: the square root of 2 in Q15, its slope on the fraction, and the bits of the fraction.
const ROOT_TWO_Q15: i32 = 46_214;
const ONE_Q15: i32 = 1 << 15;
const ROOT_SLOPE: i32 = 213;
const FRACTION_MASK: u32 = 0x7F;
const FRACTION_LEAD: i32 = 24;
// silk_DIV32_varQ and silk_INVERSE32_varQ: the Q of the first inverse and of the result before it is scaled.
const INVERSE_Q: i32 = 29;
const INVERSE_RESULT_Q: i32 = 61;
const RESIDUAL_SHIFT: u32 = 3;
// silk_log2lin (4.2.7.4): the bits of the fraction of its Q7 input, and the parabola's factor.
const LOG_FRACTION_BITS: u32 = 7;
const LOG_PARABOLA: i32 = -174;
const LOG_ONE: i32 = 1 << LOG_FRACTION_BITS;

pub(super) const fn smulwb(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i16 as i64) >> HALF_WORD) as i32
}

pub(super) const fn smlawb(sum: i32, a: i32, b: i32) -> i32 {
    sum.wrapping_add(smulwb(a, b))
}

pub(super) const fn smulbb(a: i32, b: i32) -> i32 {
    a as i16 as i32 * b as i16 as i32
}

pub(super) const fn smlabb(sum: i32, a: i32, b: i32) -> i32 {
    sum.wrapping_add(smulbb(a, b))
}

pub(super) const fn smulww(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> HALF_WORD) as i32
}

const fn smlaww(sum: i32, a: i32, b: i32) -> i32 {
    sum.wrapping_add(smulww(a, b))
}

pub(super) const fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> WORD) as i32
}

pub(super) const fn rshift_round(value: i32, shift: u32) -> i32 {
    if shift == 1 { (value >> 1) + (value & 1) } else { ((value >> (shift - 1)) + 1) >> 1 }
}

pub(super) const fn rshift_round64(value: i64, shift: u32) -> i64 {
    if shift == 1 { (value >> 1) + (value & 1) } else { ((value >> (shift - 1)) + 1) >> 1 }
}

pub(super) fn sat16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

// A left shift that saturates; a shift of the whole word saturates as one of 31 bits does.
fn lshift_sat32(value: i32, shift: u32) -> i32 {
    let shift = shift.min(WORD - 1);
    value.clamp(i32::MIN >> shift, i32::MAX >> shift) << shift
}

pub(super) const fn lcg(seed: i32) -> i32 {
    seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT)
}

// The bits a value can be shifted left by and keep its sign, as silk_CLZ32(silk_abs()) less one gives them.
fn headroom(value: i32) -> u32 {
    value.wrapping_abs().leading_zeros().saturating_sub(1)
}

// Scales a result of `shift` too many fractional bits to its Q, left where `shift` is negative.
fn scale(result: i32, shift: i32) -> i32 {
    match shift {
        ..=0 => lshift_sat32(result, shift.unsigned_abs()),
        1..32 => result >> shift,
        _ => 0,
    }
}

// (a << q) / b, approximated as silk_DIV32_varQ does; b is not none.
pub(super) fn div32_varq(a: i32, b: i32, q: i32) -> i32 {
    let (a_headroom, b_headroom) = (headroom(a), headroom(b));
    let a_norm = a << a_headroom;
    let b_norm = b << b_headroom;
    let inverse = (i32::MAX >> 2) / (b_norm >> HALF_WORD);
    let result = smulwb(a_norm, inverse);
    let left = a_norm.wrapping_sub(smmul(b_norm, result).wrapping_shl(RESIDUAL_SHIFT));
    let result = smlawb(result, left, inverse);
    scale(result, INVERSE_Q + a_headroom as i32 - b_headroom as i32 - q)
}

// (1 << q) / b, approximated as silk_INVERSE32_varQ does; b is not none.
pub(super) fn inverse32_varq(b: i32, q: i32) -> i32 {
    let b_headroom = headroom(b);
    let b_norm = b << b_headroom;
    let inverse = (i32::MAX >> 2) / (b_norm >> HALF_WORD);
    let result = inverse << HALF_WORD;
    let error = (1i32 << INVERSE_Q).wrapping_sub(smulwb(b_norm, inverse)).wrapping_shl(RESIDUAL_SHIFT);
    let result = smlaww(result, error, inverse);
    scale(result, INVERSE_RESULT_Q - b_headroom as i32 - q)
}

// The square root of a value, as 4.2.7.5.3 approximates it.
pub(super) fn sqrt_approx(value: i32) -> i32 {
    if value <= 0 {
        return 0;
    }
    let zeros = value.leading_zeros() as i32;
    let fraction =
        ((value as u32).rotate_right((FRACTION_LEAD - zeros).rem_euclid(WORD as i32) as u32) & FRACTION_MASK) as i32;
    let root = if zeros & 1 != 0 { ONE_Q15 } else { ROOT_TWO_Q15 } >> (zeros >> 1);
    smlawb(root, root, smulbb(ROOT_SLOPE, fraction))
}

// 2 to the power of a Q7 value of 16 or more (4.2.7.4).
pub(super) fn log2lin(log: i32) -> i32 {
    let whole = 1i32.wrapping_shl((log >> LOG_FRACTION_BITS) as u32);
    let fraction = log & (LOG_ONE - 1);
    let bend = smlawb(fraction, fraction * (LOG_ONE - fraction), LOG_PARABOLA);
    whole.wrapping_add((whole >> LOG_FRACTION_BITS).wrapping_mul(bend))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products_are_exact_floors() {
        for (a, b) in [(123_456_789, -98_765), (-7, 3), (i32::MAX, i32::MAX), (-1 << 30, 40_000)] {
            let exact = (i64::from(a) * i64::from(b)).div_euclid(1 << 16);
            assert_eq!(i64::from(smulww(a, b)), exact as i32 as i64, "{a} * {b} over 2^16, rounded down");
        }
        assert_eq!(smulwb(1 << 20, 0x1_8000), -(1 << 19), "the low half of b is taken as a signed 16-bit value");
        assert_eq!(smulbb(0x1_FFFF, 3), -3, "both factors are taken as signed 16-bit values");
        assert_eq!(smlawb(i32::MAX, 1 << 16, 1), i32::MIN, "a sum past 32 bits wraps");
        assert_eq!(smmul(1 << 30, 1 << 30), 1 << 28);
    }

    #[test]
    fn rounding_shifts() {
        assert_eq!([5, 6, 7, -5, -6, -7].map(|value| rshift_round(value, 1)), [3, 3, 4, -2, -3, -3]);
        assert_eq!([20, 23, 24, -24, -25].map(|value| rshift_round(value, 3)), [3, 3, 3, -3, -3]);
        assert_eq!(rshift_round64(1 << 40 | 1 << 15, 16), (1 << 24) + 1, "a half rounds up");
        assert_eq!((sat16(40_000), sat16(-40_000), sat16(-5)), (i16::MAX, i16::MIN, -5));
        assert_eq!((lshift_sat32(1 << 20, 12), lshift_sat32(-1 << 20, 12)), (i32::MAX >> 12 << 12, i32::MIN));
        assert_eq!(lshift_sat32(3, 40), i32::MAX >> 31 << 31, "a shift past the word saturates");
    }

    #[test]
    fn lcg_sequence() {
        assert_eq!(lcg(0), LCG_INCREMENT);
        assert_eq!(lcg(1), (196_314_165u32 + 907_633_515) as i32, "4.2.7.8.6's generator, modulo 2^32");
        assert_eq!(lcg(-1), (907_633_515u32.wrapping_sub(196_314_165)) as i32);
    }

    #[test]
    fn divisions_approximate() {
        for (a, b) in [(65_536, 81_920), (1_686_110_208, 81_920), (81_920, 1_686_110_208), (-300_000, 77_777)] {
            let exact = (f64::from(a) * 65_536.0 / f64::from(b)) as i64;
            let found = i64::from(div32_varq(a, b, 16));
            assert!((found - exact).abs() <= exact.abs() / 10_000 + 1, "{a} / {b} in Q16: {found} for {exact}");
        }
        for b in [81_920, 65_536, 1_686_110_208, 3] {
            let exact = (2f64.powi(47) / f64::from(b)).min(f64::from(i32::MAX)) as i64;
            let found = i64::from(inverse32_varq(b, 47));
            assert!((found - exact).abs() <= exact / 10_000 + 1, "1 / {b} in Q47: {found} for {exact}");
        }
        assert_eq!(div32_varq(1, 1, 40), i32::MAX >> 11 << 11, "a quotient past 32 bits saturates");
    }

    #[test]
    fn square_root_within_its_accuracy() {
        assert_eq!((sqrt_approx(0), sqrt_approx(-4), sqrt_approx(16), sqrt_approx(1 << 20)), (0, 0, 4, 1024));
        for value in [20_000, 123_456, 123_456_789, i32::MAX] {
            let exact = f64::from(value).sqrt();
            let found = f64::from(sqrt_approx(value));
            assert!((found / exact - 1.0).abs() < 0.025, "{value}: {found} for {exact}, within 2.5% above 120");
        }
    }

    #[test]
    fn exponential() {
        assert_eq!(log2lin(16 << 7), 1 << 16, "a whole power of two is exact");
        assert_eq!(log2lin(2090), 81_920, "the formula of 4.2.7.4 at i 16 and f 42");
        assert_eq!(log2lin(30 << 7 | 127), (1 << 30) + 126 * (1 << 23), "and at the top of its range");
    }
}
