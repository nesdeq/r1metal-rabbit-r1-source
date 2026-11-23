//! Sound: the card, recording, playback, pacing, a live voice's rate doubled, a call's sound from the network and to it, volume, the echo of the speaker taken out of the microphone, and the arithmetic of samples and spans.

pub mod call;
pub mod card;
mod echo;
mod jitter;
mod level;
pub mod pace;
pub mod playback;
pub mod record;
pub(crate) mod resample;
pub mod volume;

use crate::base::math::log2;
use crate::base::time::MICROS_PER_SECOND;
use crate::base::time::NANOS_PER_SECOND;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use core::time::Duration;
use playback::{MemoryFile, Playback};

pub(crate) const PEAK_FLOOR: u32 = 64;
const PEAK_RATIO: u32 = 4;
pub(crate) const LOUDNESS_STEPS: usize = 4;
const NOTICE: &[u8] = include_bytes!("../../../assets/sound/notice.m4a");
/// The log line of a notice.
pub const NOTICE_LINE: &str = "sound: a notice";
/// The log line of a sound refused the speaker.
pub const SPEAKER_BUSY_LINE: &str = "sound: the speaker is in use, and nothing is played aside";

/// The log line of samples the microphone's thread came too late for.
pub fn to_lost_line(count: u64) -> String {
    format!("sound: {count} samples of the microphone came and went unheard")
}

/// The notice from its first sample; none where it cannot be played, which a test rules out.
pub fn notice() -> Option<Playback<MemoryFile<'static>>> {
    Playback::new(MemoryFile::parse(NOTICE).ok()?)
}

// A sample's full scale.
const FULL_SCALE: f32 = 32_768.0;

// A power in decibels, the least a float tells for none.
fn decibels(power: f32) -> f32 {
    10.0 * log2(power.max(f32::MIN_POSITIVE)) * core::f32::consts::LOG10_2
}

fn samples_in(span: Duration, rate: u32) -> u64 {
    span.as_micros() as u64 * u64::from(rate) / MICROS_PER_SECOND
}

pub(crate) fn to_span(samples: u64, rate: u32) -> Duration {
    let rate = u64::from(rate.max(1));
    Duration::from_secs(samples / rate) + Duration::from_nanos(samples % rate * NANOS_PER_SECOND / rate)
}

fn missed_samples(span: Duration, rate: u32, count: u64) -> u64 {
    samples_in(span, rate).saturating_sub(count)
}

/// The count of samples a microphone handed over.
pub struct Intake {
    rate: u32,
    count: u64,
}

impl Intake {
    pub const fn new(rate: u32) -> Intake {
        Intake { rate, count: 0 }
    }

    pub const fn add(&mut self, samples: u64) {
        self.count += samples;
    }

    /// The samples of `span` since the start that never came; they count as come from now on.
    pub fn take_lost(&mut self, span: Duration) -> u64 {
        let lost = missed_samples(span, self.rate, self.count);
        self.count += lost;
        lost
    }
}

/// The largest magnitude in a block.
pub fn peak(samples: &[i16]) -> u16 {
    samples.iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0)
}

pub(crate) fn loudness(peak: u32) -> usize {
    (0..LOUDNESS_STEPS as u32).take_while(|step| peak >= PEAK_FLOOR * PEAK_RATIO.pow(*step)).count()
}

pub(crate) fn fill(block: &mut [i16], queue: &mut VecDeque<i16>) -> usize {
    let count = block.len().min(queue.len());
    block.iter_mut().zip(queue.drain(..count)).for_each(|(sample, value)| *sample = value);
    block[count..].fill(0);
    block.len() - count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_samples_in_a_span() {
        assert_eq!(samples_in(Duration::from_millis(100), 48_000), 4800);
        assert_eq!(samples_in(Duration::from_micros(20), 48_000), 0, "less than a sample's time is none");
        assert_eq!(samples_in(Duration::ZERO, 48_000), 0);
        assert_eq!(samples_in(Duration::from_secs(3600), 48_000), 172_800_000, "an hour does not run over");
    }

    #[test]
    fn spans_and_missed() {
        assert_eq!(to_span(4800, 48_000), Duration::from_millis(100));
        assert_eq!(to_span(1, 48_000), Duration::from_nanos(20_833), "a sample is the part of a second its rate says");
        assert_eq!(to_span(172_800_000, 48_000), Duration::from_secs(3600), "an hour does not run over");
        assert_eq!((to_span(0, 48_000), to_span(7, 0)), (Duration::ZERO, Duration::from_secs(7)));
        assert_eq!(missed_samples(Duration::from_millis(100), 48_000, 4000), 800);
        assert_eq!(missed_samples(Duration::from_millis(100), 48_000, 5000), 0, "more came than the span holds: none");
    }

    #[test]
    fn lost_samples_counted_once() {
        let mut intake = Intake::new(48_000);
        intake.add(4000);
        assert_eq!(intake.take_lost(Duration::from_millis(100)), 800, "what the span holds and never came");
        assert_eq!(intake.take_lost(Duration::from_millis(100)), 0, "which counts as come from then on");
        intake.add(4800);
        assert_eq!(intake.take_lost(Duration::from_millis(250)), 2400, "the next gap alone");
        assert_eq!(to_lost_line(800), "sound: 800 samples of the microphone came and went unheard");
    }

    #[test]
    fn fill_and_peak() {
        let mut queue: VecDeque<i16> = [1, -9, 3].into_iter().collect();
        let mut block = [7i16; 2];
        assert_eq!((fill(&mut block, &mut queue), block, queue.len()), (0, [1, -9], 1));
        let mut block = [7i16; 4];
        assert_eq!((fill(&mut block, &mut queue), block), (3, [3, 0, 0, 0]), "the one left, and three lacked");
        assert_eq!(fill(&mut block, &mut queue), 4);
        assert_eq!((peak(&[3, -9, 1]), peak(&[i16::MIN]), peak(&[])), (9, 32_768, 0));
    }

    #[test]
    fn loudness_steps() {
        assert_eq!(
            [0, PEAK_FLOOR - 1, PEAK_FLOOR, PEAK_FLOOR * PEAK_RATIO.pow(3), u32::MAX].map(loudness),
            [0, 0, 1, LOUDNESS_STEPS, LOUDNESS_STEPS],
            "a step for each fourfold of the floor, four at the most"
        );
    }

    #[test]
    fn notice_decodes() {
        assert!(notice().is_some(), "a file of ours");
        assert!(playback::support::round_once(NOTICE).len() > 1, "every packet of it decodes, and it lasts");
    }
}
