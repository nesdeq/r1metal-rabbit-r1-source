use super::tables::TAPSETS;

// The shortest period the filter takes, which a period of nought, a filter that is off, is raised to (COMBFILTER_MINPERIOD).
pub(super) const MIN_PERIOD: usize = 15;
// The longest period the pre-filter takes, and the pre-emphasised samples it keeps for it (COMBFILTER_MAXPERIOD).
pub(super) const MAX_PERIOD: usize = 1024;

// The post-filter's parameters: its pitch period in samples, its gain and its tapset (4.3.7.1).
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct Pitch {
    pub(super) period: usize,
    pub(super) gain: f32,
    pub(super) tapset: usize,
}

impl Pitch {
    // The taps around the period, each times the gain: at it, one off it and two off it.
    fn taps(self) -> [f32; 3] {
        TAPSETS[self.tapset].map(|tap| self.gain * tap)
    }
}

// Filters `len` samples of `samples` from `at` in place, crossing from `from` to `to` by the square of the window over its first samples (comb_filter, 4.3.7.1). The decoder's post-filter adds to each sample the filtered samples a period before it; the encoder's pre-filter, its inverse, takes off the unfiltered ones, so it runs from the last sample back.
pub(super) fn filter(samples: &mut [f32], at: usize, len: usize, [from, to]: [Pitch; 2], window: &[f32], is_pre: bool) {
    if from.gain == 0.0 && to.gain == 0.0 {
        return;
    }
    let (old, new) = (from.period.max(MIN_PERIOD), to.period.max(MIN_PERIOD));
    let sign = if is_pre { -1.0 } else { 1.0 };
    let (old_taps, new_taps) = (from.taps().map(|tap| sign * tap), to.taps().map(|tap| sign * tap));
    let around = |samples: &[f32], x: usize, period: usize, taps: [f32; 3]| {
        let centre = x - period;
        taps[0] * samples[centre]
            + taps[1] * (samples[centre - 1] + samples[centre + 1])
            + taps[2] * (samples[centre - 2] + samples[centre + 2])
    };
    for index in 0..len {
        let step = if is_pre { len - 1 - index } else { index };
        let x = at + step;
        let new_part = around(samples, x, new, new_taps);
        samples[x] += match window.get(step) {
            Some(&weight) => {
                let fade = weight * weight;
                (1.0 - fade) * around(samples, x, old, old_taps) + fade * new_part
            }
            None => new_part,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn filter_adds_the_period_before() {
        let mut samples = vec![0.0f32; 200];
        samples[100] = 1.0;
        let pitch = Pitch { period: 20, gain: 0.5, tapset: 1 };
        filter(&mut samples, 100, 100, [pitch, pitch], &[], false);
        let taps = TAPSETS[1].map(|tap| 0.5 * tap);
        assert_eq!(samples[100], 1.0, "nothing a period before the impulse");
        assert_eq!((samples[119], samples[120], samples[121]), (taps[1], taps[0], taps[1]), "the taps a period on");
        assert!((samples[140] - (taps[0] * taps[0] + 2.0 * taps[1] * taps[1])).abs() < 1.0e-6, "and filtered again");
    }

    #[test]
    fn filter_crosses_between_pitches() {
        let window = [0.0, 1.0];
        let mut samples = vec![1.0f32; 64];
        let (from, to) = (Pitch { period: 15, gain: 0.25, tapset: 2 }, Pitch { period: 30, gain: 0.5, tapset: 0 });
        filter(&mut samples, 40, 2, [from, to], &window, false);
        let (old, new): (f32, f32) = (TAPSETS[2].iter().sum(), TAPSETS[0].iter().sum());
        let (old, new) = (0.25 * (2.0 * old - TAPSETS[2][0]), 0.5 * (2.0 * new - TAPSETS[0][0]));
        assert!((samples[40] - (1.0 + old)).abs() < 1.0e-6, "the old filter where the window is nought");
        assert!((samples[41] - (1.0 + new)).abs() < 1.0e-6, "the new where it is one");
        let mut silent = vec![2.0f32; 64];
        filter(&mut silent, 40, 20, [Pitch::default(), Pitch::default()], &window, false);
        assert_eq!(silent, vec![2.0f32; 64], "a filter of no gain leaves the samples");
    }

    #[test]
    fn post_filter_undoes_pre_filter() {
        let mut state = 0x0DD5_EED5_u32;
        let original: alloc::vec::Vec<f32> =
            (0..600).map(|_| (crate::base::math::next_random(&mut state) >> 16) as f32 - 32_768.0).collect();
        let window = [0.1, 0.4, 0.8, 1.0];
        let (from, to) = (Pitch { period: 37, gain: 0.375, tapset: 0 }, Pitch { period: 90, gain: 0.75, tapset: 2 });
        let mut samples = original.clone();
        filter(&mut samples, 100, 500, [from, to], &window, true);
        assert_ne!(samples[100..], original[100..], "the pre-filter takes the period out");
        assert_eq!(samples[..100], original[..100], "and leaves what lies before");
        let unfiltered = samples.clone();
        filter(&mut samples, 100, 500, [from, to], &window, false);
        let worst = samples.iter().zip(&original).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // Measured at 0.0039, one step of an f32 near 32768; the bound lies 0.006 over.
        assert!(worst < 0.01, "the post-filter puts it back, {worst} off");
        let taps = to.taps();
        let around = |x: usize| {
            taps[0] * original[x - 90]
                + taps[1] * (original[x - 91] + original[x - 89])
                + taps[2] * (original[x - 92] + original[x - 88])
        };
        for x in [200, 350, 599] {
            assert_eq!(
                unfiltered[x],
                original[x] - around(x),
                "sample {x}: the pre-filter takes off the unfiltered samples a period before"
            );
        }
    }
}
