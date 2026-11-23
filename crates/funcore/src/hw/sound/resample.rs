// A live voice brought from half the card's rate to it by a curve.

use super::pace::interpolate;
use crate::base::math::to_sample;
use alloc::vec::Vec;

// The live voices' own way from half the card's rate: the filter (`base::resample`), of 88 taps there, would play each sample 43 of the model's later, and other samples than the device has played.
#[derive(Default)]
pub(crate) struct Doubler {
    window: [f32; 3],
}

impl Doubler {
    pub(crate) fn double(&mut self, samples: &[i16], out: &mut Vec<i16>) {
        for &sample in samples {
            let window = [self.window[0], self.window[1], self.window[2], f32::from(sample)];
            out.extend([to_sample(window[1]), to_sample(interpolate(window, 0.5))]);
            self.window = [window[1], window[2], window[3]];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::sine_cosine;

    #[test]
    fn rate_doubling() {
        let slow: Vec<i16> =
            (0..2400).map(|at| (8000.0 * sine_cosine(1000.0 * f64::from(at) / 24_000.0).0) as i16).collect();
        let (mut doubler, mut fast) = (Doubler::default(), Vec::new());
        slow.chunks(100).for_each(|chunk| doubler.double(chunk, &mut fast));
        assert_eq!(fast.len(), 4800);
        let worst = fast
            .iter()
            .enumerate()
            .skip(8)
            .map(|(at, &sample)| {
                (f64::from(sample) - 8000.0 * sine_cosine(1000.0 * (at as f64 - 4.0) / 48_000.0).0).abs()
            })
            .fold(0.0, f64::max);
        assert!(
            worst < 12.0,
            "a tone of 1 kHz comes out the same tone at twice the rate, four samples later: {worst} off"
        );
        let mut level = Vec::new();
        Doubler::default().double(&[1000; 8], &mut level);
        assert_eq!(level[6..], [1000; 10], "a level stays level");
        let mut loud = Vec::new();
        Doubler::default().double(&[i16::MAX, i16::MIN, i16::MAX, i16::MIN, i16::MAX, i16::MIN], &mut loud);
        assert_eq!(loud.len(), 12, "and nothing wraps where the curve goes past what a sample can be");
    }
}
