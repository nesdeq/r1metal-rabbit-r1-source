//! Sound kept to a clock that is not its own: paced to a video's, or to the plan of a call's sound.

use crate::base::math::to_sample;
use crate::codec::aac;

pub(super) const MAX_PACE_CHANGE: f32 = 0.002;
const SEEK_SECONDS: f32 = 0.06;
const CATCH_UP_RATE: f32 = 0.25;
const SMOOTHING_SECONDS: f32 = 1.0;
const RATE: f32 = aac::RATE as f32;
// The samples the sound lies off the clock's place while it follows at its fastest.
pub(super) const FULL_PACE_LAG: f32 = MAX_PACE_CHANGE * RATE / CATCH_UP_RATE;

// The curve through four samples, read between its second and third: Catmull and Rom's.
pub(super) fn interpolate([p0, p1, p2, p3]: [f32; 4], t: f32) -> f32 {
    let bend = 2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0);
    p1 + 0.5 * t * (p2 - p0 + t * bend)
}

/// A stream of samples with a seek.
pub trait Source {
    fn seek(&mut self, sample: u64);
    // Silence behind the last sample.
    fn next(&mut self) -> i16;
}

/// Keeps sound to a clock that is not its own.
#[derive(Default)]
pub struct Follower {
    window: [f32; 4],
    fraction: f32,
    at: Option<f64>,
    silence: u64,
    lag: f32,
}

impl Follower {
    fn pace(&self) -> f32 {
        1.0 + (self.lag * CATCH_UP_RATE / RATE).clamp(-MAX_PACE_CHANGE, MAX_PACE_CHANGE)
    }

    /// Fills `out` with the sound at the clock's `target`; silence while it is none, and the next sound starts where the clock says.
    pub fn follow(&mut self, target: Option<f64>, source: &mut impl Source, out: &mut [i16]) {
        match target {
            Some(target) => self.fill(target, source, out),
            None => {
                self.at = None;
                out.fill(0);
            }
        }
    }

    fn fill(&mut self, target: f64, source: &mut impl Source, out: &mut [i16]) {
        match self.at.map(|at| (target - at) as f32) {
            Some(offset) if offset.abs() <= SEEK_SECONDS * RATE => {
                self.lag += (offset - self.lag) * (out.len() as f32 / RATE / SMOOTHING_SECONDS).min(1.0);
            }
            // Too far off, or nowhere yet: the sound goes to the clock's place at one go.
            _ => {
                source.seek(target.max(0.0) as u64);
                (self.at, self.silence) = (Some(target), (-target).max(0.0) as u64);
                (self.window, self.fraction, self.lag) = ([0.0; 4], 0.0, 0.0);
            }
        }
        let pace = self.pace();
        for sample in out.iter_mut() {
            self.fraction += pace;
            while self.fraction >= 1.0 {
                self.fraction -= 1.0;
                let next = if self.silence > 0 { 0 } else { source.next() };
                self.silence = self.silence.saturating_sub(1);
                self.window = [self.window[1], self.window[2], self.window[3], f32::from(next)];
            }
            *sample = to_sample(interpolate(self.window, self.fraction));
        }
        self.at = self.at.map(|at| at + out.len() as f64 * f64::from(pace));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::sine_cosine;

    const BLOCK: usize = 1024;

    struct Tone {
        at: u64,
        seeks: usize,
    }

    fn tone(sample: f64) -> f64 {
        8000.0 * sine_cosine(440.0 * sample / f64::from(RATE)).0
    }

    impl Source for Tone {
        fn seek(&mut self, sample: u64) {
            (self.at, self.seeks) = (sample, self.seeks + 1);
        }

        fn next(&mut self) -> i16 {
            self.at += 1;
            tone((self.at - 1) as f64) as i16
        }
    }

    fn play(blocks: usize, speed: f64) -> (Follower, Tone, f64) {
        let (mut follower, mut source) = (Follower::default(), Tone { at: 0, seeks: 0 });
        let (mut out, mut worst) = ([0i16; BLOCK], 0.0f64);
        for block in 0..blocks {
            let start = follower.at;
            follower.fill((block * BLOCK) as f64 * speed, &mut source, &mut out);
            if let (Some(start), true) = (start, block == blocks - 1) {
                // What is played lies behind what was read by the curve's own reach: three samples less the step just made.
                let pace = f64::from(follower.pace());
                worst = out
                    .iter()
                    .enumerate()
                    .map(|(index, &sample)| (f64::from(sample) - tone(start + (index + 1) as f64 * pace - 3.0)).abs())
                    .fold(0.0, f64::max);
            }
        }
        (follower, source, worst)
    }

    #[test]
    fn own_clock() {
        let (follower, source, worst) = play(50, 1.0);
        assert_eq!((follower.pace(), source.seeks, source.at), (1.0, 1, 50 * BLOCK as u64));
        assert!(worst < 1.0, "sample for sample, {worst} off at most");
        let (mut follower, mut source, mut out) = (Follower::default(), Tone { at: 0, seeks: 0 }, [7i16; 6]);
        follower.fill(0.0, &mut source, &mut out);
        assert_eq!(out[..2], [0, 0], "two samples of silence first: the curve's reach");
        assert_eq!(out[2..], [0, 1, 2, 3].map(|sample| tone(f64::from(sample)) as i16));
    }

    #[test]
    fn clock_drift() {
        for speed in [1.000_32, 0.999_68, 1.001, 0.999] {
            let (follower, source, worst) = play(47 * 30, speed);
            let pace = f64::from(follower.pace());
            assert!((pace - speed).abs() < 2e-5, "after half a minute the pace is the clock's: {pace} for {speed}");
            let offset = (47 * 30 * BLOCK) as f64 * speed - follower.at.unwrap();
            assert!(
                offset.abs() < 0.005 * f64::from(RATE),
                "and the sound within 5 ms of it: {offset} samples at {speed}"
            );
            assert_eq!(source.seeks, 1, "it went to its place once, at the beginning");
            assert!(worst < 40.0, "the tone comes out a tone, {worst} of 8000 off at most");
        }
    }

    #[test]
    fn far_off_seek() {
        let (mut follower, mut source, mut out) = (Follower::default(), Tone { at: 0, seeks: 0 }, [0i16; BLOCK]);
        follower.fill(0.0, &mut source, &mut out);
        follower.fill(BLOCK as f64 + 2000.0, &mut source, &mut out);
        assert_eq!((source.seeks, follower.pace()), (1, 1.0 + 2000.0 * (BLOCK as f32 / 48_000.0) * 0.25 / 48_000.0));
        follower.fill(2.0 * BLOCK as f64 + 5000.0, &mut source, &mut out);
        assert_eq!(
            (source.seeks, source.at),
            (2, 2 * BLOCK as u64 + 5000 + BLOCK as u64),
            "more than 60 ms off: a leap"
        );
        assert_eq!(follower.pace(), 1.0, "from which it goes on at its own pace");
        follower.follow(None, &mut source, &mut out);
        assert_eq!((out, source.seeks), ([0; BLOCK], 2), "while the clock stands there is silence");
        follower.follow(Some(3.0 * BLOCK as f64 + 5000.0), &mut source, &mut out);
        assert_eq!(source.seeks, 3, "told that the clock stood still, it finds its place anew");
    }

    #[test]
    fn clock_before_sound() {
        let (mut follower, mut source, mut out) = (Follower::default(), Tone { at: 0, seeks: 0 }, [7i16; BLOCK]);
        follower.fill(-500.0, &mut source, &mut out);
        assert!(out[..502].iter().all(|&sample| sample == 0), "500 samples before it, and the curve's two");
        assert_eq!(out[502..506], [0, 1, 2, 3].map(|sample| tone(f64::from(sample)) as i16));
        assert_eq!((source.seeks, source.at), (1, BLOCK as u64 - 500));
    }
}
