//! The gain of a volume, in fixed point: three decibels less for every step under the loudest.

use crate::base::math::Scale;
use alloc::format;
use alloc::string::String;

// The quietest is silence.
pub(crate) const QUIETEST: u8 = 0;
pub(crate) const LOUDEST: u8 = 100;
pub(crate) const STEP: u8 = 10;
pub(crate) const SCALE: Scale = Scale { min: QUIETEST, max: LOUDEST, step: STEP };
const GAIN_BITS: u32 = 15;
pub(super) const UNITY: u32 = 1 << GAIN_BITS;
const SQRT_HALF: u32 = (UNITY * UNITY / 2).isqrt();
const HALVING_STEPS: u32 = 2;
const ROUNDING: i32 = 1 << (GAIN_BITS - 1);

/// The log line of the levels set.
pub fn to_levels_line(volume: u8, sounds: u8) -> String {
    format!("volume: {volume} %, sounds {sounds} %")
}

pub(super) fn gain(setting: u8) -> u32 {
    let setting = SCALE.nearest(u32::from(setting));
    if setting == QUIETEST {
        return 0;
    }
    let steps = u32::from((LOUDEST - setting) / STEP);
    // Two steps halve the gain exactly; the one that is left over is the root of a half of that.
    let base = UNITY >> (steps / HALVING_STEPS);
    if steps % HALVING_STEPS == 0 { base } else { (base * SQRT_HALF + UNITY / 2) >> GAIN_BITS }
}

/// Scales a block by a gain, to the nearest sample with halves away from zero; `UNITY` leaves it alone.
pub fn scale(block: &mut [i16], gain: u32) {
    if gain >= UNITY {
        return;
    }
    for sample in block {
        let product = i32::from(*sample) * gain as i32;
        *sample = (((product.abs() + ROUNDING) >> GAIN_BITS) * product.signum()) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_gains() {
        let steps: [u32; 11] = core::array::from_fn(|step| gain(LOUDEST - step as u8 * STEP));
        assert_eq!(
            steps,
            [UNITY, 23_170, 16_384, 11_585, 8_192, 5_793, 4_096, 2_896, 2_048, 1_448, 0],
            "a whole of 32768, one step down 32768 divided by the root of 2, 23170.47, and two steps halve it"
        );
        assert_eq!((UNITY, SQRT_HALF, gain(QUIETEST)), (32_768, steps[1], 0));
        for pair in steps[..10].windows(2) {
            let power = |gain: u32| u64::from(gain) * u64::from(gain);
            assert!(
                power(pair[0]).abs_diff(2 * power(pair[1])) <= 2 * u64::from(pair[0]),
                "{} to {} is not half the power, within the rounding of the gain to a whole number",
                pair[0],
                pair[1]
            );
        }
        let expected = f64::from(UNITY) * core::f64::consts::FRAC_1_SQRT_2 / 16.0;
        assert!(
            (f64::from(gain(10)) - expected).abs() < 1.0,
            "9 steps of 3 dB, 27 dB down, is the whole divided by 2 to the power of 4.5: {expected}"
        );
    }

    #[test]
    fn block_scaling() {
        let block = [i16::MAX, i16::MIN, 3000, -3000, 7, -7, 1, -1, 0];
        let scaled = |volume: u8| {
            let mut scaled = block;
            scale(&mut scaled, gain(volume));
            scaled
        };
        assert_eq!(scaled(100), block, "at the loudest the block is as it was");
        assert_eq!(
            scaled(80),
            [16_384, -16_384, 1500, -1500, 4, -4, 1, -1, 0],
            "two steps down halve it, to the nearest sample and halves away from zero"
        );
        assert_eq!(scaled(90)[2..4], [2121, -2121], "one step down is the root of a half of it");
        assert_eq!(scaled(10)[2..4], [133, -133], "nine steps down are 27 dB, which takes 3000 to 133");
        assert_eq!(scaled(0), [0; 9], "at the quietest there is silence");
        let (mut loud, mut quiet) = (block, block);
        scale(&mut loud, gain(100));
        scale(&mut quiet, gain(70));
        assert!(
            loud.iter().zip(&quiet).all(|(loud, quiet)| quiet.unsigned_abs() <= loud.unsigned_abs()),
            "a lower volume is never louder in any sample"
        );
    }

    #[test]
    fn nearest_step() {
        assert_eq!([4, 5, 14, 15, 84, 85, 96].map(gain), [0, gain(10), gain(10), gain(20), gain(80), gain(90), UNITY]);
        assert_eq!([101, 150, u8::MAX].map(gain), [UNITY; 3]);
        assert_eq!(to_levels_line(70, 40), "volume: 70 %, sounds 40 %");
    }
}
