use crate::base::math::{dot, sqrt, to_sample};
use crate::codec::aac::RATE;

const AIM: f32 = 3277.0;
const CEILING: f32 = 29_205.0;
const MAX_GAIN: f32 = 16.0;
const MIN_VOICE: f32 = 56.0;
const SILENCE: f32 = 1.0;
const VOICE_RATIO: f32 = 4.0;
const BACKGROUND_RISE: f32 = 0.4;
const LOUDNESS_FALL: f32 = 0.2;
const FIRST_GROWTH: f32 = 15.0;
const GROWTH: f32 = 1.0;

pub(super) struct Leveller {
    gain: f32,
    loudness: Option<f32>,
    background: Option<f32>,
    is_at_goal: bool,
}

impl Default for Leveller {
    fn default() -> Leveller {
        Leveller { gain: 1.0, loudness: None, background: None, is_at_goal: false }
    }
}

impl Leveller {
    // True where the block holds voice.
    pub(super) fn level(&mut self, samples: &mut [i16]) -> bool {
        if samples.is_empty() {
            return false;
        }
        let seconds = samples.len() as f32 / RATE as f32;
        let peak = f32::from(super::peak(samples));
        let level = sqrt(dot(samples, samples) / samples.len() as f32);
        if level < SILENCE {
            return false;
        }
        let background = self.background.map_or(level, |last| (last * (1.0 + BACKGROUND_RISE * seconds)).min(level));
        self.background = Some(background);
        let mut target = self.gain;
        let is_voice = level > MIN_VOICE && level > background * VOICE_RATIO;
        if is_voice {
            let loudness = self.loudness.map_or(level, |last| (last * (1.0 - LOUDNESS_FALL * seconds)).max(level));
            self.loudness = Some(loudness);
            let goal = (AIM / loudness).clamp(1.0, MAX_GAIN);
            let growth = if self.is_at_goal { GROWTH } else { FIRST_GROWTH };
            target = goal.min(self.gain * (1.0 + growth * seconds));
            self.is_at_goal |= target >= goal;
        }
        if peak * target > CEILING {
            target = CEILING / peak;
        }
        // A gain that falls does so at once, so that what made it fall is not clipped; one that rises does so across the block.
        let start = self.gain.min(target);
        let step = (target - start) / samples.len() as f32;
        for (index, sample) in samples.iter_mut().enumerate() {
            let scaled = f32::from(*sample) * (start + step * (index + 1) as f32);
            *sample = to_sample(scaled);
        }
        self.gain = target;
        is_voice
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::next_random;
    use alloc::vec::Vec;

    const BLOCK: usize = 1024;

    fn noise(level: f32, seed: &mut u32) -> Vec<i16> {
        (0..BLOCK).map(|_| (((next_random(seed) >> 16) as f32 / 32768.0 - 1.0) * level * sqrt(3.0)) as i16).collect()
    }

    fn level_of(samples: &[i16]) -> f32 {
        sqrt(samples.iter().map(|&sample| f32::from(sample) * f32::from(sample)).sum::<f32>() / samples.len() as f32)
    }

    fn speak(leveller: &mut Leveller, voice: f32, background: f32, blocks: impl Iterator<Item = bool>) -> f32 {
        let (mut seed, mut last) = (9, 0.0);
        for is_voiced in blocks {
            let mut block = noise(if is_voiced { voice } else { background }, &mut seed);
            leveller.level(&mut block);
            assert!(block.iter().all(|&sample| f32::from(sample).abs() <= CEILING + 1.0), "nothing past the ceiling");
            if is_voiced {
                last = level_of(&block);
            }
        }
        last
    }

    #[test]
    fn quiet_voice() {
        let mut leveller = Leveller::default();
        let words = (0..47 * 4).map(|block| block % 20 >= 8);
        let last = speak(&mut leveller, 400.0, 6.0, words);
        assert!((last / AIM - 1.0).abs() < 0.25, "a voice at {last} for {AIM}");
        assert!((leveller.gain - AIM / 400.0).abs() < 2.0);
        let mut early = Leveller::default();
        let first_words = (0..24).map(|block| block >= 8);
        assert!(speak(&mut early, 400.0, 6.0, first_words) > AIM / 2.0, "after a third of a second of voice");
    }

    #[test]
    fn faint_voice() {
        let mut leveller = Leveller::default();
        let last = speak(&mut leveller, 60.0, 5.0, (0..47 * 5).map(|block| block % 20 >= 8));
        assert!(
            (leveller.gain - MAX_GAIN).abs() < 0.01 && last < 1100.0,
            "gain {} and a voice at {last}",
            leveller.gain
        );
    }

    #[test]
    fn loud_voice() {
        let mut leveller = Leveller::default();
        let last = speak(&mut leveller, 9000.0, 20.0, (0..100).map(|block| block % 10 >= 3));
        assert!(leveller.gain <= 1.0 && (last / 9000.0 - 1.0).abs() < 0.1, "gain {} and {last}", leveller.gain);
        let mut square = [32_767i16; BLOCK];
        Leveller::default().level(&mut square);
        assert!(square.iter().all(|&sample| sample == 29_205));
    }

    #[test]
    fn steady_background() {
        for background in [5.0, 40.0, 900.0] {
            let mut leveller = Leveller::default();
            speak(&mut leveller, 0.0, background, (0..47 * 10).map(|_| false));
            assert_eq!(leveller.gain, 1.0, "a background of {background}");
        }
    }

    #[test]
    fn voice_verdict() {
        let (mut leveller, mut seed) = (Leveller::default(), 5);
        let heard = (0..47).filter(|_| leveller.level(&mut noise(6.0, &mut seed))).count();
        assert_eq!(heard, 0, "a steady background holds no voice");
        assert!(leveller.level(&mut noise(400.0, &mut seed)), "a voice over it does");
        assert!(!leveller.level(&mut [0; BLOCK]), "and silence none");
    }

    #[test]
    fn gain_hold_and_fall() {
        let mut leveller = Leveller::default();
        speak(&mut leveller, 400.0, 6.0, (0..47 * 3).map(|block| block % 20 >= 8));
        let voice_gain = leveller.gain;
        speak(&mut leveller, 0.0, 6.0, (0..47 * 5).map(|_| false));
        assert_eq!(leveller.gain, voice_gain, "five seconds without a word");
        let mut seed = 3;
        let mut shout = noise(6000.0, &mut seed);
        leveller.level(&mut shout);
        assert!(leveller.gain <= 1.0 && shout.iter().all(|&sample| f32::from(sample).abs() <= CEILING + 1.0));
        let shout_gain = leveller.gain;
        leveller.level(&mut [0; BLOCK]);
        speak(&mut leveller, 0.0, 40.0, (0..47).map(|_| false));
        assert_eq!(leveller.gain, shout_gain, "samples that are nothing tell nothing of the background");
    }
}
