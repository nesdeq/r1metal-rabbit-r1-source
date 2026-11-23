// Sound brought to another rate by a polyphase filter, a sinc under a Kaiser window, the lowpass that keeps what the lower of the two rates can hold.

use super::math::{bessel, ceil, dot, sine_cosine, sqrt, to_sample};
use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

// The share of the lower rate's Nyquist frequency under which the filter passes sound as it is; the transition to silence takes the rest.
const PASSBAND: f64 = 0.9;
// Decibels the sound it stops is down, 70: the range of 12 bits. With it Kaiser's formulas (1974) give the filter's length, taps = (A less 7.95) / (14.36 * width), and its window's shape, beta = 0.1102 * (A less 8.7) for A over 50.
const STOPBAND: f64 = 70.0;
const LENGTH_SUBTRAHEND: f64 = 7.95;
const LENGTH_DIVISOR: f64 = 14.36;
const BETA_FACTOR: f64 = 0.1102;
const BETA_SUBTRAHEND: f64 = 8.7;

fn gcd(one: u32, other: u32) -> u32 {
    if other == 0 { one } else { gcd(other, one % other) }
}

// Each sample made is the sum of the `taps` samples about its place, which is its number times the ratio of the rates, each times a coefficient of the phase the place has between two samples; where the stream begins and ends it is silence.
pub(crate) struct Resampler {
    // The samples made per `down` samples taken, once both rates are divided by their greatest common divisor.
    up: u64,
    down: u64,
    taps: usize,
    phases: Vec<f32>,
    // The last `taps` samples taken, the oldest where `head` is, held twice over so that they lie in a row wherever it is.
    ring: Vec<f32>,
    head: usize,
    // Samples taken so far, and the number of the next sample to make.
    taken: u64,
    made: u64,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Resampler {
        let divisor = gcd(from, to).max(1);
        let (up, down) = (to / divisor, from / divisor);
        // The lower rate's Nyquist frequency in cycles per sample taken, the transition above the passband, and the cutoff in the middle of it.
        let nyquist = 0.5 * (f64::from(up) / f64::from(down)).min(1.0);
        let width = nyquist * (1.0 - PASSBAND);
        let cutoff = nyquist * (1.0 + PASSBAND) / 2.0;
        let taps =
            (ceil(((STOPBAND - LENGTH_SUBTRAHEND) / (LENGTH_DIVISOR * width)) as f32) as usize).next_multiple_of(2);
        let (half, beta) = (taps / 2, BETA_FACTOR * (STOPBAND - BETA_SUBTRAHEND));
        let edge = bessel(beta);
        let mut phases = Vec::with_capacity(up as usize * taps);
        for phase in 0..up {
            let first = phases.len();
            for tap in 0..taps {
                let away = tap as f64 - (half - 1) as f64 - f64::from(phase) / f64::from(up);
                let across = away / half as f64;
                let window = bessel(beta * f64::from(sqrt((1.0 - across * across).max(0.0) as f32))) / edge;
                let sinc = if away == 0.0 { 2.0 * cutoff } else { sine_cosine(cutoff * away).0 / (PI * away) };
                phases.push((sinc * window) as f32);
            }
            let sum: f32 = phases[first..].iter().sum();
            phases[first..].iter_mut().for_each(|coefficient| *coefficient /= sum);
        }
        Resampler {
            up: u64::from(up),
            down: u64::from(down),
            taps,
            phases,
            ring: vec![0.0; 2 * taps],
            head: 0,
            taken: 0,
            made: 0,
        }
    }

    // The whole part and the phase of the place of the sample made with that number, in samples taken.
    fn place(&self, made: u64) -> (u64, usize) {
        let place = made * self.down;
        (place / self.up, (place % self.up) as usize)
    }

    // Samples of the sum before the one at its place's whole part.
    const fn before(&self) -> u64 {
        self.taps as u64 / 2 - 1
    }

    // The samples of the stream's length so far that are made only once more of it is taken, those whose sums reach past its newest sample: this many where the rates divide, one more at times where they do not.
    pub(crate) const fn held(&self) -> u64 {
        (self.taps as u64 - self.before() - 1) * self.up / self.down
    }

    // Makes every sample whose sum has all its samples taken, up to the one before `limit`.
    fn make(&mut self, limit: u64, out: &mut impl Extend<i16>) {
        let after = self.taps as u64 - self.before();
        while self.made < limit {
            let (whole, phase) = self.place(self.made);
            if whole + after > self.taken {
                break;
            }
            let coefficients = &self.phases[phase * self.taps..][..self.taps];
            let samples = &self.ring[self.head..self.head + self.taps];
            out.extend(core::iter::once(to_sample(dot(samples, coefficients))));
            self.made += 1;
        }
    }

    fn take(&mut self, sample: f32) {
        (self.ring[self.head], self.ring[self.head + self.taps]) = (sample, sample);
        self.head = (self.head + 1) % self.taps;
        self.taken += 1;
    }

    // Takes the next sample of the stream and gives the samples that are made of it and the ones before.
    pub(crate) fn push(&mut self, sample: i16, out: &mut impl Extend<i16>) {
        self.take(f32::from(sample));
        self.make(u64::MAX, out);
    }

    // Ends the stream: gives the samples made of its last, silence behind them, up to the one its length comes to.
    pub(crate) fn finish(&mut self, out: &mut impl Extend<i16>) {
        let limit = (self.taken * self.up).div_ceil(self.down);
        while self.made < limit {
            self.take(0.0);
            self.make(limit, out);
        }
    }

    // Goes to the sample made that number and gives the sample of the stream to take from there on.
    pub(crate) fn seek(&mut self, made: u64) -> u64 {
        let (whole, _) = self.place(made);
        let first = whole.saturating_sub(self.before());
        self.ring.fill(0.0);
        (self.head, self.taken, self.made) = (0, first, made);
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_held_back() {
        let filter = Resampler::new(24_000, 48_000);
        assert_eq!(
            (filter.taps, filter.before(), filter.held()),
            (88, 43, 88),
            "to twice the rate: its taps, the samples a sum reaches back by, and the samples made that wait for more"
        );
        for (rate, held) in [(8_000, 264), (12_000, 176), (16_000, 132), (44_100, 47)] {
            let (mut filter, mut out) = (Resampler::new(rate, 48_000), Vec::new());
            (0..rate / 10).for_each(|_| filter.push(1000, &mut out));
            assert_eq!(4800 - out.len() as u64, held, "{rate} Hz: of a tenth of a second, the samples not made yet");
            assert_eq!(filter.held(), held);
        }
    }

    const RATES: [u32; 13] =
        [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
    // How far a sample of a tone of 10000 may lie from the tone it should be, measured at 1, the rounding of the sample and of the reference; the bound is twice that.
    const NEAREST: i32 = 2;
    // How far the mean distance between two rises through nothing may lie from the 48 samples of a cycle of 1 kHz at 48 kHz: a rise lies up to a sample off its true place, so the mean of 990 distances lies up to 2 / 990 off, which is 0.002.
    const PERIOD: f64 = 0.002;
    // How far the first sample made of a level of 1000 may lie from the level times a half and the filter's cutoff, measured at 0 at 44.1 kHz and 16 kHz and 0.5 at 96 kHz, the rounding of the sample; the bound is 1.
    const FIRST: f64 = 1.0;
    // How much the gain of a tone may differ from 1 in the passband, as a share: measured at most 0.0003 over every rate, the rounding of the tone's samples; the bound is 0.001, a hundredth of a decibel.
    const FLAT: f64 = 0.001;
    // The decibels what the new rate cannot hold is stopped by at least: the design's 70, measured at 79.76 to 88.66.
    const STOPPED: f64 = 70.0;

    fn tone(rate: u32, hertz: f64, size: f64, count: usize) -> Vec<i16> {
        (0..count).map(|at| (size * (2.0 * PI * hertz * at as f64 / f64::from(rate)).sin()).round() as i16).collect()
    }

    fn convert(from: u32, to: u32, samples: &[i16]) -> Vec<i16> {
        let (mut resampler, mut out) = (Resampler::new(from, to), Vec::new());
        samples.iter().for_each(|&sample| resampler.push(sample, &mut out));
        resampler.finish(&mut out);
        out
    }

    // The size of the tone of that frequency in samples at `rate`, from their sums with a sine and a cosine of the frequency over the middle half of them, clear of both ends.
    fn size(samples: &[i16], rate: u32, hertz: f64) -> f64 {
        let ends = samples.len() / 4;
        let (mut sine, mut cosine) = (0.0, 0.0);
        for (at, &sample) in samples.iter().enumerate().take(samples.len() - ends).skip(ends) {
            let angle = 2.0 * PI * hertz * at as f64 / f64::from(rate);
            sine += f64::from(sample) * angle.sin();
            cosine += f64::from(sample) * angle.cos();
        }
        2.0 * (sine * sine + cosine * cosine).sqrt() / (samples.len() - 2 * ends) as f64
    }

    // The most that samples lie from the tone they should be, leaving out those within `ends` of either end.
    fn off_tone(samples: &[i16], rate: u32, hertz: f64, size: f64, ends: usize) -> Option<i32> {
        let reference = tone(rate, hertz, size, samples.len());
        (ends..samples.len() - ends).map(|at| (i32::from(samples[at]) - i32::from(reference[at])).abs()).max()
    }

    #[test]
    fn sine_frequency_kept() {
        let out = convert(44_100, 48_000, &tone(44_100, 1000.0, 10_000.0, 44_100));
        assert_eq!(out.len(), 48_000, "a second is a second");
        let rises: Vec<usize> = (1..out.len()).filter(|&at| out[at - 1] <= 0 && out[at] > 0).collect();
        let period = (rises[rises.len() - 1] - rises[0]) as f64 / (rises.len() - 1) as f64;
        assert!((period - 48.0).abs() < PERIOD, "a cycle of {period} samples");
        let off = off_tone(&out, 48_000, 1000.0, 10_000.0, 200);
        assert!(off <= Some(NEAREST), "the same sine, at the same places in time: {off:?} off");
    }

    #[test]
    fn rates_tune_and_length() {
        for rate in RATES {
            let out = convert(rate, 48_000, &tone(rate, 1000.0, 10_000.0, rate as usize));
            assert_eq!(out.len(), 48_000, "{rate} Hz: a second is a second");
            let off = off_tone(&out, 48_000, 1000.0, 10_000.0, 1000);
            assert!(off <= Some(NEAREST), "{rate} Hz: {off:?} off the sine");
            for count in [0usize, 1, 7, 1000, 1007] {
                let silence = convert(rate, 48_000, &vec![0; count]);
                assert_eq!(
                    silence.len(),
                    (count as u64 * 48_000).div_ceil(u64::from(rate)) as usize,
                    "{rate} Hz, {count} samples"
                );
            }
        }
    }

    #[test]
    fn passband_and_stopband() {
        // Tones at the edge of the passband, 0.9 of the lower rate's Nyquist frequency, and half that.
        for (from, to, hertz) in [
            (44_100, 48_000, 19_800.0),
            (96_000, 48_000, 21_600.0),
            (16_000, 48_000, 7200.0),
            (88_200, 48_000, 21_600.0),
            (7_350, 48_000, 3300.0),
        ] {
            for hertz in [hertz / 2.0, hertz] {
                let input = tone(from, hertz, 10_000.0, from as usize);
                let out = convert(from, to, &input);
                let gain = size(&out, to, hertz) / size(&input, from, hertz);
                assert!((gain - 1.0).abs() < FLAT, "{from} to {to} at {hertz} Hz: a gain of {gain}");
            }
        }
        // Tones the new rate cannot hold fold down to a frequency it can: their size there is what the filter let through.
        for (from, hertz) in
            [(96_000, 25_000.0), (96_000, 26_000.0), (96_000, 30_000.0), (88_200, 26_000.0), (88_200, 30_000.0)]
        {
            let input = tone(from, hertz, 10_000.0, from as usize);
            let out = convert(from, 48_000, &input);
            let left = size(&out, 48_000, 48_000.0 - hertz) / size(&input, from, hertz);
            assert!(20.0 * left.log10() < -STOPPED, "{from} Hz tone at {hertz} Hz: {} dB", 20.0 * left.log10());
        }
    }

    #[test]
    fn level_between_silences() {
        for rate in [44_100, 16_000, 96_000] {
            let out = convert(rate, 48_000, &vec![1000; rate as usize]);
            let reach = Resampler::new(rate, 48_000).taps * 48_000 / rate as usize;
            assert!(out[reach..out.len() - reach].iter().all(|&sample| sample == 1000), "{rate} Hz: level");
            // At the first sample the window lies over silence up to its middle: what is left of the level is half of the filter's response and the part of it that its cutoff in cycles per sample leaves above the middle.
            let cutoff = 0.5 * (48_000.0 / f64::from(rate)).min(1.0) * (1.0 + PASSBAND) / 2.0;
            let (first, expected) = (f64::from(out[0]), 1000.0 * (0.5 + cutoff));
            assert!((first - expected).abs() < FIRST, "{rate} Hz: {first} for {expected}");
        }
    }

    #[test]
    fn seek_from_any_sample() {
        for rate in [44_100, 16_000, 96_000, 11_025] {
            let input = tone(rate, 1234.0, 9000.0, rate as usize / 2);
            let whole = convert(rate, 48_000, &input);
            let mut resampler = Resampler::new(rate, 48_000);
            for made in [0usize, 1, 5, 100, 4321, whole.len() / 2] {
                let first = resampler.seek(made as u64) as usize;
                let mut out = Vec::new();
                input[first..].iter().for_each(|&sample| resampler.push(sample, &mut out));
                resampler.finish(&mut out);
                assert!(out == whole[made..], "{rate} Hz from sample {made}, taken from {first}");
            }
        }
    }
}
