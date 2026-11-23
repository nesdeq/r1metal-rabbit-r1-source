// A stream's frames kept in step with a screen's refreshes by their length, so that every picture is ready at one place between two flips.

use crate::base::time::NANOS_PER_SECOND;
use crate::hw::camera::exposure::LINE_LENGTH;
use crate::hw::camera::exposure::PIXEL_CLOCK_HZ;
use crate::hw::camera::exposure::Pace;
use crate::hw::camera::stream::READY_AFTER_FLIP_NS;

const LINE_NS: u64 = LINE_LENGTH * NANOS_PER_SECOND / PIXEL_CLOCK_HZ;
const SAMPLES: usize = 3;
const SKIP_FRAMES: u32 = 3;
const IN_STEP_NS: i64 = 2_000_000;
const LOST_NS: i64 = 3_000_000;
const MEAN_OVER: i64 = 16;
const HOLD_FRAMES: u32 = 32;
const RETURN_SHARE: i64 = 2;
const BASE_SHARE: i64 = 8;
const REACH_LINES: u32 = 16;

pub(super) struct Genlock {
    refresh_ns: i64,
    nominal: u32,
    lines: u32,
    set_lines: u32,
    samples: [i64; SAMPLES],
    sample_count: usize,
    skips: u32,
    pub(super) is_in_step: bool,
    mean_ns: i64,
    hold_count: u32,
    base_ns: i64,
}

impl Genlock {
    pub(super) const fn new(pace: Pace, refresh_ns: u64) -> Genlock {
        Genlock {
            refresh_ns: refresh_ns as i64,
            nominal: pace.frame_lines,
            lines: pace.frame_lines,
            set_lines: pace.frame_lines,
            samples: [0; SAMPLES],
            sample_count: 0,
            skips: 0,
            is_in_step: false,
            mean_ns: 0,
            hold_count: 0,
            base_ns: 0,
        }
    }

    pub(super) fn take_ready(&mut self, ready_ns: u64, flipped_ns: u64) -> Option<u32> {
        if self.refresh_ns == 0 || flipped_ns == 0 {
            self.is_in_step = true;
            return None;
        }
        let (since_flip, half) =
            ((ready_ns as i64 - flipped_ns as i64).rem_euclid(self.refresh_ns), self.refresh_ns / 2);
        // How much later than its place the picture is: within half a refresh either way, as one flip is like the next.
        let late = (since_flip - READY_AFTER_FLIP_NS as i64 + half).rem_euclid(self.refresh_ns) - half;
        let lines = if self.is_in_step { self.keep_place(late) } else { self.find_place(late) };
        (core::mem::replace(&mut self.lines, lines) != lines).then_some(lines)
    }

    fn find_place(&mut self, late: i64) -> u32 {
        if self.skips > 0 {
            self.skips -= 1;
            return self.set_lines;
        }
        self.samples[self.sample_count] = late;
        self.sample_count += 1;
        if self.sample_count < SAMPLES {
            return self.set_lines;
        }
        self.sample_count = 0;
        self.samples.sort_unstable();
        let middle = self.samples[SAMPLES / 2];
        if middle.abs() <= IN_STEP_NS {
            (self.is_in_step, self.mean_ns, self.hold_count) = (true, middle, 0);
            return self.set_lines;
        }
        // Frames can only be made longer: pictures that are late are brought round to the place after the next flip.
        let delay = if middle < 0 { -middle } else { self.refresh_ns - middle };
        self.skips = SKIP_FRAMES;
        self.set_lines + (delay as u64 / LINE_NS) as u32
    }

    fn keep_place(&mut self, late: i64) -> u32 {
        self.mean_ns += (late - self.mean_ns) / MEAN_OVER;
        self.hold_count += 1;
        // Pictures that come far too early are brought back at once: later is the way frames can be moved fast.
        if self.mean_ns < -LOST_NS {
            self.is_in_step = false;
            return self.find_place(late);
        }
        if self.hold_count < HOLD_FRAMES {
            return self.lines;
        }
        self.hold_count = 0;
        // A line more on every frame until the next weighing brings the pictures later by this much.
        let line = (LINE_NS * u64::from(HOLD_FRAMES)) as i64;
        let limit = line * i64::from(REACH_LINES);
        self.base_ns = (self.base_ns - self.mean_ns / BASE_SHARE).clamp(0, limit);
        let extra = (self.base_ns - self.mean_ns / RETURN_SHARE).clamp(0, limit);
        self.set_lines = self.nominal + ((extra + line / 2) / line) as u32;
        self.set_lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    const SCREEN_NS: u64 = 16_666_666;
    const PANEL_NS: f64 = 16_670_000.0;
    const MAKE_NS: f64 = 9_300_000.0;
    const LINE: f64 = 3800.0 / 288e6 * 1e9;
    const LINES: u32 = Pace::LIVE.frame_lines;

    #[derive(Clone, Copy, Debug)]
    struct Frame {
        late_ns: f64,
        lines: u32,
        is_in_step: bool,
    }

    struct Bench {
        panel_ns: f64,
        shows: usize,
    }

    impl Bench {
        const R1: Bench = Bench { panel_ns: PANEL_NS, shows: 3 };

        fn run(
            &self,
            genlock: &mut Genlock,
            frames: usize,
            start_ns: f64,
            make_ns: impl Fn(usize) -> f64,
        ) -> Vec<Frame> {
            let (mut end, mut asked) = (start_ns, LINES);
            let mut pending: VecDeque<u32> = core::iter::repeat_n(LINES, self.shows - 1).collect();
            let mut log = Vec::new();
            for frame in 0..frames {
                let ready = end + make_ns(frame);
                let flipped = (ready / self.panel_ns).floor() * self.panel_ns;
                asked = genlock.take_ready(ready as u64, flipped as u64).unwrap_or(asked);
                let half = self.panel_ns / 2.0;
                let late = (ready - flipped - READY_AFTER_FLIP_NS as f64 + half).rem_euclid(self.panel_ns) - half;
                log.push(Frame { late_ns: late, lines: asked, is_in_step: genlock.is_in_step });
                pending.push_back(asked);
                end += f64::from(pending.pop_front().unwrap_or(asked)) * LINE;
            }
            log
        }
    }

    fn uneven(frame: usize) -> f64 {
        let slow = if frame % 97 == 96 { 6_000_000.0 } else { 0.0 };
        MAKE_NS + ((frame * 7919) % 3000) as f64 * 1000.0 - 1_500_000.0 + slow
    }

    fn farthest(frames: &[Frame]) -> f64 {
        frames.iter().map(|frame| frame.late_ns.abs()).fold(0.0, f64::max)
    }

    #[test]
    fn line_and_frame_time() {
        assert_eq!(LINE_NS, 13_194);
        let frame = f64::from(LINES) * LINE;
        assert!(
            frame < PANEL_NS && frame + LINE > PANEL_NS,
            "the panel's refresh lies between the pace's frame and one a line longer: {frame}"
        );
        let round = PANEL_NS / (PANEL_NS - frame);
        assert!((3000.0..3200.0).contains(&round), "left alone, a stream drifts through every place in {round} frames");
    }

    #[test]
    fn finds_place() {
        for shows in 2..=4 {
            for sixteenth in 0..16 {
                let mut genlock = Genlock::new(Pace::LIVE, SCREEN_NS);
                assert!(!genlock.is_in_step);
                let start = 1e9 + f64::from(sixteenth) * PANEL_NS / 16.0;
                let frames = Bench { panel_ns: PANEL_NS, shows }.run(&mut genlock, 3000, start, |_| MAKE_NS);
                let found = frames.iter().position(|frame| frame.is_in_step).expect("the place is found");
                assert!(
                    found <= 2 * SAMPLES + SKIP_FRAMES as usize,
                    "a length that shows {shows} frames on, begun {sixteenth} sixteenths into a refresh: in step after {found} frames"
                );
                let in_step = &frames[found..];
                assert!(
                    in_step.iter().all(|frame| frame.is_in_step) && farthest(in_step) <= IN_STEP_NS as f64,
                    "{shows} frames on, {sixteenth} sixteenths in: at most {} ns from its place once in step",
                    farthest(in_step)
                );
                let settled = &frames[600..];
                assert!(
                    settled.iter().all(|frame| frame.lines == LINES || frame.lines == LINES + 1),
                    "in step, the frames have the pace's lines or one more"
                );
                let changes = settled.windows(2).filter(|pair| pair[0].lines != pair[1].lines).count();
                assert!(
                    (10..=settled.len() / HOLD_FRAMES as usize).contains(&changes) && farthest(settled) < 600_000.0,
                    "{changes} changes of the length in {} frames, at most {} ns from the place",
                    settled.len(),
                    farthest(settled)
                );
            }
        }
    }

    #[test]
    fn uneven_pictures() {
        for sixteenth in 0..16 {
            let mut genlock = Genlock::new(Pace::LIVE, SCREEN_NS);
            let start = 1e9 + f64::from(sixteenth) * PANEL_NS / 16.0;
            let frames = Bench::R1.run(&mut genlock, 6000, start, uneven);
            let found = frames.iter().position(|frame| frame.is_in_step).expect("the place is found");
            assert!(
                found <= 4 * (SAMPLES + SKIP_FRAMES as usize) && frames[found..].iter().all(|frame| frame.is_in_step),
                "{sixteenth} sixteenths in: in step from frame {found} on"
            );
            let usual: Vec<Frame> = (600..6000).filter(|frame| frame % 97 != 96).map(|frame| frames[frame]).collect();
            assert!(
                farthest(&usual) < 2_500_000.0,
                "{sixteenth} sixteenths in: the usual picture is at most {} ns from its place",
                farthest(&usual)
            );
            assert!(
                frames[600..].iter().all(|frame| frame.lines <= LINES + 1),
                "and neither a slow picture nor an uneven one moves a frame's length"
            );
        }
    }

    #[test]
    fn slower_and_faster_panels() {
        let slower = (f64::from(LINES) + 3.5) * LINE;
        let mut genlock = Genlock::new(Pace::LIVE, slower as u64);
        let frames = Bench { panel_ns: slower, shows: 3 }.run(&mut genlock, 6000, 1e9, |_| MAKE_NS);
        let settled = &frames[3000..];
        assert!(
            settled.iter().all(|frame| frame.is_in_step && (3..=4).contains(&(frame.lines - LINES)))
                && farthest(settled) < 500_000.0,
            "three or four lines more, at most {} ns from the place",
            farthest(settled)
        );
        let faster = (f64::from(LINES) - 0.5) * LINE;
        let mut genlock = Genlock::new(Pace::LIVE, faster as u64);
        let frames = Bench { panel_ns: faster, shows: 3 }.run(&mut genlock, 6000, 1e9, |_| MAKE_NS);
        assert!(
            frames.iter().any(|frame| frame.is_in_step && frame.late_ns > 3_000_000.0),
            "frames cannot be made shorter than the pace has them: such a stream falls behind its place"
        );
    }

    #[test]
    fn early_pictures_return() {
        let mut genlock = Genlock::new(Pace::LIVE, SCREEN_NS);
        let make_ns = |frame: usize| if frame < 1000 { MAKE_NS } else { MAKE_NS - 5_000_000.0 };
        let frames = Bench::R1.run(&mut genlock, 2000, 1e9, make_ns);
        assert!(frames[999].is_in_step && frames[999].late_ns.abs() < 400_000.0);
        assert!(frames[1001].late_ns < -4_500_000.0, "five milliseconds early");
        let back = (1000..2000).find(|&frame| frames[frame].late_ns.abs() < 1_000_000.0).expect("back at its place");
        assert!(back <= 1060, "back within a millisecond of its place at frame {back}");
        assert!(frames[1100..].iter().all(|frame| frame.is_in_step) && farthest(&frames[1100..]) < 1_000_000.0);
    }

    #[test]
    fn no_screen() {
        let mut genlock = Genlock::new(Pace::LIVE, SCREEN_NS);
        assert_eq!(
            (genlock.take_ready(1_000_000_000, 0), genlock.is_in_step),
            (None, true),
            "a screen that never flipped"
        );
        let mut genlock = Genlock::new(Pace::LIVE, 0);
        assert_eq!(
            (genlock.take_ready(1_000_000_000, 900_000_000), genlock.is_in_step),
            (None, true),
            "no refresh at all"
        );
    }
}
