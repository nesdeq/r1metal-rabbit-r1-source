//! Rate control for the hardware encoder: the next quantiser, from the sizes of the frames before.

use crate::hw::venc::COARSEST;

const START: u32 = 30;
const FINEST: u32 = 18;
const COARSEST_USED: u32 = 44;
const _: () = assert!(COARSEST_USED <= COARSEST);
const KEY_FINER: u32 = 2;
const KEY_SHARE: u64 = 4;
const PERIOD_FRAMES: u32 = 6;
const STEP: i32 = 2;
const WINDOW_FRAMES: i64 = 60;
const LEAST_TARGET_HALVES: i64 = 1;
const MOST_TARGET_HALVES: i64 = 3;
const ONE_STEP: u64 = 73_562;
const THREE_STEPS: u64 = 92_682;
const UNIT: u64 = 1 << 16;

/// The rate control of one stream.
pub struct Rate {
    inter_bits: i64,
    frame_bits: i64,
    quantiser: u32,
    debt: i64,
    is_keys_only: bool,
    period_frames: u32,
    period_bits: u64,
}

fn steps(bits: u64, target: u64) -> i32 {
    let to_steps = |high: u64, low: u64| {
        if high * UNIT >= low * THREE_STEPS {
            STEP
        } else if high * UNIT >= low * ONE_STEP {
            1
        } else {
            0
        }
    };
    if bits >= target { to_steps(bits, target.max(1)) } else { -to_steps(target, bits.max(1)) }
}

impl Rate {
    /// A control for a mean of `frame_bits` bits a frame and a key frame every `group` frames.
    pub fn new(frame_bits: u64, group: u32) -> Rate {
        let group = u64::from(group.max(1));
        let inter = if group == 1 { frame_bits } else { frame_bits * group / (group - 1 + KEY_SHARE) };
        Rate {
            inter_bits: inter as i64,
            frame_bits: frame_bits as i64,
            quantiser: START,
            debt: 0,
            is_keys_only: group == 1,
            period_frames: 0,
            period_bits: 0,
        }
    }

    /// The quantiser of the next frame.
    pub fn quantiser(&self, is_key: bool) -> u32 {
        if is_key && !self.is_keys_only { self.quantiser.saturating_sub(KEY_FINER).max(FINEST) } else { self.quantiser }
    }

    /// Takes the size of the last frame; a key frame among inter frames counts toward the budget alone.
    pub fn take_frame(&mut self, is_key: bool, bytes: usize) {
        let bits = 8 * bytes as i64;
        let window = WINDOW_FRAMES * self.frame_bits;
        self.debt = (self.debt + bits - self.frame_bits).clamp(-window, window);
        if is_key && !self.is_keys_only {
            return;
        }
        self.period_frames += 1;
        self.period_bits += bits as u64;
        if self.period_frames < PERIOD_FRAMES {
            return;
        }
        let mean = self.period_bits / u64::from(self.period_frames);
        (self.period_frames, self.period_bits) = (0, 0);
        let share = self.inter_bits;
        let target = (share - share * self.debt / window.max(1))
            .clamp(share * LEAST_TARGET_HALVES / 2, share * MOST_TARGET_HALVES / 2);
        let next = self.quantiser as i32 + steps(mean, target.max(1) as u64);
        self.quantiser = (next.max(0) as u32).clamp(FINEST, COARSEST_USED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const GROUP: u32 = 60;
    const FRAME_BITS: u64 = 266_666;
    const SMOOTH_CURVE: [u64; 6] = [1024, 912, 813, 724, 645, 575];
    const STEEP_CURVE: [u64; 6] = [1024, 700, 478, 327, 223, 153];

    fn frame(activity: u64, curve: [u64; 6], is_key: bool, quantiser: u32) -> usize {
        let mut bits = activity * if is_key { 5 } else { 1 } * curve[(quantiser % 6) as usize] / 1024;
        for _ in 0..quantiser / 6 {
            bits = bits * curve[5] * curve[5] / (curve[4] * 1024);
        }
        (bits / 8) as usize
    }

    fn run(rate: &mut Rate, activity: u64, curve: [u64; 6], frames: u32) -> (u64, Vec<u32>) {
        let (mut bits, mut quantisers) = (0, Vec::new());
        for index in 0..frames {
            let is_key = index % GROUP == 0;
            let quantiser = rate.quantiser(is_key);
            let bytes = frame(activity, curve, is_key, quantiser);
            rate.take_frame(is_key, bytes);
            if index >= frames - GROUP {
                bits += 8 * bytes as u64;
                quantisers.push(quantiser);
            }
        }
        (bits, quantisers)
    }

    #[test]
    fn step_borders() {
        assert_eq!([100, 112, 113, 141, 142, 400].map(|bits| steps(bits, 100)), [0, 0, 1, 1, 2, 2]);
        assert_eq!([100, 90, 89, 71, 70, 10, 0].map(|bits| steps(bits, 100)), [0, 0, -1, -1, -2, -2, -2]);
        assert_eq!(steps(5, 0), 2, "no target is any frame too large for");
        let power = |value: u64, times: u32| (0..times).fold(1.0f64, |product, _| product * value as f64 / UNIT as f64);
        assert!(power(ONE_STEP - 1, 6) < 2.0 && power(ONE_STEP + 1, 6) > 2.0, "the sixth root of two");
        assert!(power(THREE_STEPS - 1, 2) < 2.0 && power(THREE_STEPS + 1, 2) > 2.0, "the square root of two");
    }

    #[test]
    fn scene_curves() {
        let size = |curve, quantiser| frame(8_000_000, curve, false, quantiser) as f64;
        assert!(
            (size(SMOOTH_CURVE, 6) / size(SMOOTH_CURVE, 0) - 0.5).abs() < 0.01
                && (size(SMOOTH_CURVE, 31) / size(SMOOTH_CURVE, 25) - 0.5).abs() < 0.01
        );
        assert!(
            (size(STEEP_CURVE, 25) / size(STEEP_CURVE, 24) - 0.68).abs() < 0.02
                && (size(STEEP_CURVE, 30) / size(STEEP_CURVE, 29) - 0.68).abs() < 0.03
        );
        assert!((frame(8_000_000, STEEP_CURVE, true, 12) as f64 / size(STEEP_CURVE, 12) - 5.0).abs() < 0.01);
    }

    #[test]
    fn settles_on_budget() {
        for (curve, scenes) in [
            (SMOOTH_CURVE, [2_000_000u64, 8_000_000, 30_000_000]),
            (STEEP_CURVE, [2_000_000_000, 20_000_000_000, 200_000_000_000]),
        ] {
            for activity in scenes {
                let mut rate = Rate::new(FRAME_BITS, GROUP);
                let (bits, quantisers) = run(&mut rate, activity, curve, 20 * GROUP);
                let budget = FRAME_BITS * u64::from(GROUP);
                assert!(
                    bits.abs_diff(budget) < budget / 4,
                    "a scene of {activity} ends at {bits} bits a group against {budget}, quantisers {quantisers:?}"
                );
                let (low, high) = (quantisers.iter().min().unwrap(), quantisers.iter().max().unwrap());
                assert!(high - low <= 4, "a scene of {activity} swings between {low} and {high}");
                let total: u64 = (0..10).map(|_| run(&mut rate, activity, curve, GROUP).0).sum();
                assert!(
                    total.abs_diff(10 * budget) < 10 * budget / 16,
                    "over ten seconds a scene of {activity} takes {total} bits against {}",
                    10 * budget
                );
            }
        }
    }

    #[test]
    fn quantiser_period() {
        for (curve, activity) in [(SMOOTH_CURVE, 30_000_000u64), (STEEP_CURVE, 200_000_000_000)] {
            let mut rate = Rate::new(FRAME_BITS, GROUP);
            let (_, quantisers) = run(&mut rate, activity, curve, GROUP);
            let changes: Vec<usize> =
                (2..quantisers.len()).filter(|&index| quantisers[index] != quantisers[index - 1]).collect();
            assert!(
                !changes.is_empty() && changes.windows(2).all(|pair| pair[1] - pair[0] >= PERIOD_FRAMES as usize),
                "changes at {changes:?} of {quantisers:?}"
            );
        }
    }

    #[test]
    fn quantiser_limits() {
        let mut rate = Rate::new(FRAME_BITS, GROUP);
        let (bits, quantisers) = run(&mut rate, 100_000, SMOOTH_CURVE, 10 * GROUP);
        assert!(quantisers.iter().all(|&quantiser| quantiser == FINEST), "{quantisers:?}");
        assert!(bits < FRAME_BITS * u64::from(GROUP) / 4);
        let (bits, quantisers) = run(&mut rate, 4_000_000_000, SMOOTH_CURVE, 10 * GROUP);
        assert!(quantisers[1..].iter().all(|&quantiser| quantiser == COARSEST_USED), "{quantisers:?}");
        assert_eq!(quantisers[0], COARSEST_USED - KEY_FINER);
        assert!(bits > FRAME_BITS * u64::from(GROUP));
    }

    #[test]
    fn scene_change() {
        let mut rate = Rate::new(FRAME_BITS, GROUP);
        run(&mut rate, 4_000_000, SMOOTH_CURVE, 10 * GROUP);
        let first = rate.quantiser(false);
        let (_, quantisers) = run(&mut rate, 16_000_000, SMOOTH_CURVE, GROUP);
        assert!(quantisers[59] >= first + 10, "four times the bits want twelve steps: {first} then {quantisers:?}");
        assert!(quantisers.windows(2).skip(1).all(|pair| pair[1].abs_diff(pair[0]) <= STEP as u32), "{quantisers:?}");
        let total: u64 = (0..10).map(|_| run(&mut rate, 16_000_000, SMOOTH_CURVE, GROUP).0).sum();
        let budget = 10 * FRAME_BITS * u64::from(GROUP);
        assert!(
            total < budget,
            "the bits the change cost are taken off the seconds after it: {total} against {budget}"
        );
    }

    #[test]
    fn key_frames() {
        let rate = Rate::new(FRAME_BITS, GROUP);
        assert_eq!((rate.quantiser(false), rate.quantiser(true)), (START, START - KEY_FINER));
        assert!(
            (63 * rate.inter_bits).abs_diff(60 * FRAME_BITS as i64) < 64,
            "59 inter frames and a key frame of four times their share add up to the budget of 60"
        );
        let mut all_keys = Rate::new(FRAME_BITS, 1);
        assert_eq!(
            (all_keys.inter_bits, all_keys.quantiser(true)),
            (FRAME_BITS as i64, START),
            "a stream of key frames gives each the whole share"
        );
        (0..PERIOD_FRAMES).for_each(|_| all_keys.take_frame(true, 10_000_000));
        assert_eq!(all_keys.quantiser(true), START + 2, "and follows their sizes");
        assert_eq!(Rate::new(FRAME_BITS, 0).inter_bits, FRAME_BITS as i64, "a group of no frames counts as one of one");
        let mut fine = Rate::new(FRAME_BITS, GROUP);
        (0..40).for_each(|_| fine.take_frame(false, 1));
        assert_eq!(
            (fine.quantiser(false), fine.quantiser(true)),
            (FINEST, FINEST),
            "no key frame finer than the finest"
        );
        let before = rate.quantiser(false);
        let mut keyed = rate;
        (0..PERIOD_FRAMES).for_each(|_| keyed.take_frame(true, 10_000_000));
        assert_eq!(keyed.quantiser(false), before, "a key frame's size moves the budget, not the quantiser");
        assert!(keyed.debt > 0);
    }
}
