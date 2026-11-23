//! The panel's refresh: its grid, the motion clock, and which stream pictures are drawn.

use crate::base::time::NANOS_PER_SECOND;

pub(crate) const REFRESH_HZ: u64 = 60;
pub const REFRESH_NS: u64 = NANOS_PER_SECOND / REFRESH_HZ;

pub(crate) const fn next_refresh(flipped_ns: u64, now_ns: u64) -> u64 {
    flipped_ns + (now_ns.saturating_sub(flipped_ns) / REFRESH_NS + 1) * REFRESH_NS
}

#[derive(Clone, Copy)]
pub(crate) struct Catchup {
    left_ns: u64,
    is_armed: bool,
    has_begun: bool,
    has_waited: bool,
}

impl Catchup {
    pub(crate) const fn new(left_ns: u64) -> Catchup {
        Catchup { left_ns, is_armed: true, has_begun: false, has_waited: false }
    }

    pub(crate) fn should_take(&mut self, handed_ns: u64, flipped_ns: u64, is_in_step: bool) -> bool {
        let has_waited = handed_ns < flipped_ns;
        // A stream's first picture is passed over wherever the stream stands; a later one only if the next comes in time, and not again while the screen is too slow for that to help.
        let is_passed = has_waited && self.is_armed && (is_in_step || !self.has_begun);
        self.is_armed &= !is_passed;
        self.has_waited = has_waited;
        !is_passed
    }

    pub(crate) fn note_shown(&mut self, draw_ns: u64) {
        self.is_armed |= !self.has_waited && (draw_ns <= self.left_ns || !self.has_begun);
        self.has_begun = true;
    }
}

// Moves by whole refreshes, so each frame of a motion shows the same time step.
pub(crate) struct Pacer {
    period_ns: u64,
    start_ns: (u64, u64),
    now_ns: u64,
}

impl Pacer {
    pub(crate) const fn new(period_ns: u64) -> Self {
        Pacer { period_ns, start_ns: (0, 0), now_ns: 0 }
    }

    pub(crate) fn advance(&mut self, real_ns: u64, is_flowing: bool) -> u64 {
        if is_flowing && self.period_ns > 0 {
            let refreshes = (real_ns.saturating_sub(self.start_ns.0) + self.period_ns / 2) / self.period_ns;
            self.now_ns = self.now_ns.max(self.start_ns.1 + refreshes * self.period_ns);
        } else {
            self.now_ns = self.now_ns.max(real_ns);
            self.start_ns = (real_ns, self.now_ns);
        }
        self.now_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn clock_moves_by_refresh() {
        let mut pacer = Pacer::new(1_000);
        assert_eq!(pacer.advance(50_000, false), 50_000);
        let times: [u64; 5] = [51_040, 51_960, 53_030, 54_000, 54_970];
        let clock: [u64; 5] = times.map(|real| pacer.advance(real, true));
        assert_eq!(clock, [51_000, 52_000, 53_000, 54_000, 55_000]);
    }

    #[test]
    fn missed_refresh_moves_two() {
        let mut pacer = Pacer::new(1_000);
        pacer.advance(10_000, false);
        assert_eq!(pacer.advance(11_010, true), 11_000);
        assert_eq!(pacer.advance(13_020, true), 13_000);
        assert_eq!(pacer.advance(13_990, true), 14_000);
    }

    #[test]
    fn new_motion_takes_real_time() {
        let mut pacer = Pacer::new(1_000);
        pacer.advance(10_000, false);
        assert_eq!(pacer.advance(11_400, true), 11_000);
        assert_eq!(pacer.advance(900_000, false), 900_000);
        assert_eq!(pacer.advance(901_100, true), 901_000);
        assert_eq!(pacer.advance(901_600, true), 902_000);
        assert_eq!(pacer.advance(901_700, false), 902_000);
        assert_eq!(pacer.advance(902_600, true), 903_000);
    }

    #[test]
    fn next_refresh_on_grid() {
        assert_eq!(next_refresh(1_000, 1_000), 1_000 + REFRESH_NS, "the refresh after the flip");
        assert_eq!(next_refresh(1_000, 1_000 + REFRESH_NS), 1_000 + 2 * REFRESH_NS, "a refresh that is now is past");
        assert_eq!(next_refresh(1_000, 1_000 + 5 * REFRESH_NS / 2), 1_000 + 3 * REFRESH_NS);
        assert_eq!(next_refresh(0, 7), REFRESH_NS, "the grid of the clock before the first flip");
    }

    fn run_screen(
        pictures: u64,
        handed: impl Fn(u64) -> u64,
        draw: impl Fn(u64) -> u64,
        is_in_step: bool,
    ) -> (Vec<(u64, u64)>, Vec<u64>) {
        let (mut catchup, mut shown, mut passed) = (Catchup::new(640), Vec::new(), Vec::new());
        let (mut free, mut flipped, mut next) = (0u64, 0u64, 0u64);
        while next < pictures {
            let mut picture = next;
            while picture + 1 < pictures && handed(picture + 1) <= free {
                picture += 1;
            }
            let at = free.max(handed(picture));
            next = picture + 1;
            if !catchup.should_take(handed(picture), flipped, is_in_step) {
                passed.push(picture);
                free = at;
                continue;
            }
            let flip = (at + draw(picture)).div_ceil(1000) * 1000;
            catchup.note_shown(at + draw(picture) - handed(picture));
            shown.push((picture, flip));
            (free, flipped) = (flip, flip);
        }
        (shown, passed)
    }

    #[test]
    fn passing_one_regains_refresh() {
        let late = |picture: u64| 10_000 + picture * 1000 + 360 + if picture == 5 { 500 } else { 0 };
        let (shown, passed) = run_screen(12, late, |_| 300, true);
        assert_eq!(passed, [6], "the picture that waited while the late one was shown");
        let flips: Vec<u64> = shown.iter().map(|&(_, flip)| flip).collect();
        assert_eq!(
            flips,
            [11_000, 12_000, 13_000, 14_000, 15_000, 17_000, 18_000, 19_000, 20_000, 21_000, 22_000],
            "one refresh is missed, by the late picture itself, and none after it"
        );
        assert!(
            shown.iter().filter(|&&(picture, _)| picture != 5).all(|&(picture, flip)| flip == late(picture) + 640),
            "every other picture is on screen at the flip after it was handed over"
        );
        let (shown, passed) = run_screen(12, late, |_| 300, false);
        assert_eq!(passed, [], "a stream that is not in step is not known to bring its next picture in time");
        assert!(
            shown[6..].iter().all(|&(picture, flip)| flip == late(picture) + 1640),
            "it stays a refresh behind, and misses none"
        );
    }

    #[test]
    fn first_picture_passed_if_it_waited() {
        let steady = |picture: u64| 10_000 + picture * 1000 + 360;
        let mut catchup = Catchup::new(640);
        assert!(
            !catchup.should_take(steady(0), steady(0) + 100, false),
            "it waited through a flip of what showed before"
        );
        assert!(catchup.should_take(steady(1), steady(0) + 100, false));
        catchup.note_shown(900);
        assert!(catchup.should_take(steady(2), steady(2) - 360, false), "and the stream is under way");
        // A screen that takes 800 for every frame misses the flip after each picture: passing over would halve its rate.
        let (shown, passed) = run_screen(40, steady, |_| 800, true);
        assert_eq!(passed, [1], "one try, which did not bring a frame in time");
        assert!(
            shown.windows(2).skip(2).all(|pair| pair[1].1 - pair[0].1 == 1000 && pair[1].0 == pair[0].0 + 1),
            "after it every picture is shown, a refresh apart"
        );
        let twice = |picture: u64| steady(picture) + if picture == 5 || picture == 7 { 500 } else { 0 };
        let (shown, passed) = run_screen(14, twice, |_| 300, true);
        assert_eq!(passed, [6, 8]);
        assert!(shown[7..].iter().all(|&(picture, flip)| flip == twice(picture) + 640), "{shown:?}");
    }

    #[test]
    fn no_period_is_real_time() {
        let mut pacer = Pacer::new(0);
        assert_eq!(pacer.advance(5, true), 5);
        assert_eq!(pacer.advance(9, true), 9);
    }
}
