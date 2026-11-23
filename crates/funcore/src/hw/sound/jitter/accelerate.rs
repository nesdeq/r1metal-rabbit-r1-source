use super::RATE;
use crate::base::math::{dot, sqrt, to_sample};
use alloc::collections::VecDeque;

// 15 ms: where a period is taken out of the 30 ms the search reads (accelerate.cc, `k15ms`; neteq_impl.cc, `DoAccelerate`).
const HALF: usize = RATE as usize * 15 / 1000;
// The sound a search reads, 30 ms.
pub(super) const WINDOW: usize = 2 * HALF;
// The search runs at 4 kHz: lags of 2.5 ms to 15 ms, and a correlation over 12.5 ms behind the newest 15 ms (time_stretch.h, `kMinLag`, `kMaxLag`, `kCorrelationLen`).
const STEP: usize = RATE as usize / 4000;
const MIN_LAG: usize = 10;
const MAX_LAG: usize = 60;
const CORRELATION: usize = 50;
const DOWNSAMPLED: usize = CORRELATION + MAX_LAG;
// The lowpass before the 4 kHz search, its seven taps from 48 kHz and the samples its first output lies in by, its taps less one and its delay (dsp_helper.cc, `kDownsample48kHzTbl`, `DownsampleTo4kHz`).
const TAPS: [f32; 7] = [0.2487, 0.0952, 0.1042, 0.1074, 0.1042, 0.0952, 0.2487];
const FIRST_TAP: usize = TAPS.len() - 1 + 4;
// The correlation two periods must reach, and in fast mode (time_stretch.h, `kCorrelationThreshold`; accelerate.cc).
const ALIKE: f32 = 0.9;
const FAST_ALIKE: f32 = 0.5;
// Sound of less energy a sample than eight times the background's is no speech; with no background estimated, NetEq's 75000 (time_stretch.cc, `SpeechDetection`).
const SPEECH_ENERGY: f32 = 8.0 * 75_000.0;

// The lag of the strongest correlation of the window's sound with itself, at 48 kHz: found at 4 kHz and placed between its neighbours by a parabola (time_stretch.cc, `AutoCorrelation`; dsp_helper.cc, `PeakDetection`).
fn pitch(window: &[i16]) -> usize {
    let low: [f32; DOWNSAMPLED] = core::array::from_fn(|at| {
        TAPS.iter().enumerate().map(|(tap, weight)| weight * f32::from(window[FIRST_TAP + STEP * at - tap])).sum()
    });
    let correlation: [f32; CORRELATION] = core::array::from_fn(|shift| {
        dot(&low[MAX_LAG..][..CORRELATION], &low[MAX_LAG - MIN_LAG - shift..][..CORRELATION])
    });
    let peak =
        (0..CORRELATION).fold(0, |best, shift| if correlation[shift] > correlation[best] { shift } else { best });
    let after = correlation.get(peak + 1).copied().unwrap_or(0.0);
    let offset = match peak.checked_sub(1).map(|before| correlation[before]) {
        Some(before) => {
            let bend = before - 2.0 * correlation[peak] + after;
            if bend < 0.0 { (0.5 * (before - after) / bend).clamp(-0.5, 0.5) } else { 0.0 }
        }
        None => 0.0,
    };
    ((MIN_LAG + peak) as f32 * STEP as f32 + offset * STEP as f32 + 0.5) as usize
}

// The samples to take out of a window of the sound that plays next at its middle: a pitch period where the periods on either side are alike, or the sound holds no speech; in fast mode as many periods as 15 ms holds, alike by less. None where neither holds (time_stretch.cc, `Process`; accelerate.cc, `CheckCriteriaAndStretch`).
pub(super) fn period(window: &[i16], is_fast: bool) -> Option<usize> {
    let lag = pitch(window);
    let (before, after) = (&window[HALF - lag..HALF], &window[HALF..HALF + lag]);
    let (first, second, cross) = (dot(before, before), dot(after, after), dot(before, after));
    let is_speech = (first + second) / (2 * lag) as f32 > SPEECH_ENERGY;
    let alike = cross.max(0.0) / (sqrt(first) * sqrt(second)).max(f32::MIN_POSITIVE);
    let threshold = if is_fast { FAST_ALIKE } else { ALIKE };
    (!is_speech || alike > threshold).then(|| if is_fast { HALF / lag * lag } else { lag })
}

// Takes `lag` samples out of the front of `line` at 15 ms, the period before it faded into the one after (audio_vector.cc, `CrossFade`).
pub(super) fn cut(line: &mut VecDeque<i16>, lag: usize) {
    let step = 1.0 / (lag + 1) as f32;
    for at in 0..lag {
        let share = 1.0 - step * (at + 1) as f32;
        let (kept, taken) = (f32::from(line[HALF - lag + at]), f32::from(line[HALF + at]));
        line[HALF - lag + at] = to_sample(share * kept + (1.0 - share) * taken);
    }
    line.drain(HALF..HALF + lag);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::{sine_cosine, xorshift64};
    use alloc::vec::Vec;

    // A voice of a pitch period of `period` samples: pulses through a resonance at 700 Hz that rings down by 0.97 a sample.
    fn voiced(period: usize) -> Vec<i16> {
        let (radius, mut ringing) = (0.97, [0.0f64; 2]);
        let swing = 2.0 * radius * sine_cosine(700.0 / f64::from(RATE)).1;
        (0..WINDOW)
            .map(|at| {
                let pulse = if at % period == 0 { 3000.0 } else { 0.0 };
                let value = pulse + swing * ringing[0] - radius * radius * ringing[1];
                ringing = [value, ringing[0]];
                value.clamp(-32_768.0, 32_767.0) as i16
            })
            .collect()
    }

    // Noise even between `-level` and `level`.
    fn noise(level: f64, seed: u64) -> Vec<i16> {
        let mut state = seed;
        (0..WINDOW)
            .map(|_| {
                state = xorshift64(state);
                (((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * level) as i16
            })
            .collect()
    }

    #[test]
    fn pitch_period_taken_out() {
        // A man's voice at 100 Hz and 125 Hz, a woman's at 200 Hz.
        for length in [480, 384, 240] {
            let sound = voiced(length);
            assert_eq!(period(&sound, false), Some(length), "a voice of a period of {length} samples loses one");
            let fast = period(&sound, true);
            assert_eq!(fast, Some(HALF / length * length), "and in fast mode as many as 15 ms holds");
        }
        assert_eq!(period(&noise(20_000.0, 7), false), None, "loud noise has no period alike on either side");
        assert!(period(&noise(300.0, 7), false).is_some(), "quiet noise is no speech, and goes");
        assert!(period(&[0; WINDOW], false).is_some(), "nor is silence");
    }

    #[test]
    fn cut_fades_into_next_period() {
        let sound = voiced(240);
        let mut line: VecDeque<i16> = sound.iter().copied().chain([1, 2, 3]).collect();
        cut(&mut line, 240);
        assert_eq!(line.len(), WINDOW + 3 - 240, "the period goes");
        assert!(line.iter().take(HALF - 240).eq(&sound[..HALF - 240]), "the sound before the fade stays");
        assert!(line.iter().skip(HALF).eq(sound[HALF + 240..].iter().chain(&[1, 2, 3])), "and after it, moved up");
        let worst = (0..240).map(|at| (i32::from(line[HALF - 240 + at]) - i32::from(sound[HALF + at])).abs()).max();
        assert!(worst < Some(3), "a voice that repeats fades into itself: {worst:?} off");
    }
}
