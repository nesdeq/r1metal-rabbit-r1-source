//! A pass of the main loop: what it owes the screen, how long it waits, and how it ends.

use super::frames::{Frames, Stream, should_draw};
use super::light::Light;
use crate::domain::camera::Viewfinder;
use crate::gfx::refresh::{Pacer, REFRESH_NS};
use crate::hw::power::Power;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

fn next_motion_ns(is_animating: bool, unchanged_ns: Option<u64>) -> Option<u64> {
    is_animating.then_some(unchanged_ns.unwrap_or(0))
}

fn wait_time(
    stalls_at_ns: Option<u64>,
    is_due: bool,
    motion_ns: Option<u64>,
    is_awake: bool,
    deadline_ns: Option<u64>,
    now_ns: u64,
) -> Option<Duration> {
    let target_ns = match (stalls_at_ns, motion_ns) {
        (Some(stall), _) => stall,
        _ if is_due => return Some(Duration::ZERO),
        (None, Some(next)) => next,
        (None, None) if !is_awake => return None,
        (None, None) => deadline_ns?,
    };
    Some(Duration::from_nanos(target_ns.saturating_sub(now_ns)))
}

/// What the main loop keeps from pass to pass of the screen and of what moves on it.
pub struct Cycle {
    pacer: Pacer,
    frames: Frames,
    stream: Stream,
    flipped_ns: u64,
    is_owed: bool,
    is_moving: bool,
    was_animating: bool,
    unchanged_ns: Option<u64>,
}

impl Default for Cycle {
    fn default() -> Cycle {
        Cycle {
            pacer: Pacer::new(REFRESH_NS),
            frames: Frames::default(),
            stream: Stream::default(),
            flipped_ns: 0,
            is_owed: true,
            is_moving: false,
            was_animating: false,
            unchanged_ns: None,
        }
    }
}

impl Cycle {
    /// The clock of a pass that begins at `real_ns`.
    pub fn begin_pass(&mut self, real_ns: u64) -> u64 {
        self.pacer.advance(real_ns, self.is_moving)
    }

    /// The clock of what the wait found at `real_ns`: while something moves it stays the frame's.
    pub fn after_wait(&mut self, now_ns: u64, real_ns: u64) -> u64 {
        if self.is_moving { now_ns } else { self.pacer.advance(real_ns, false) }
    }

    /// Takes whether the screen changed: a frame is owed where it did.
    pub const fn note_change(&mut self, is_changed: bool) {
        self.is_owed |= is_changed;
    }

    /// True where a frame is drawn now.
    pub const fn begin_draw(&mut self, is_animating: bool, is_awake: bool, light: &Light) -> bool {
        self.was_animating = is_animating;
        self.unchanged_ns = None;
        let is_due = should_draw(is_awake, light, &self.stream, self.is_owed || is_animating || self.is_moving);
        self.is_owed &= !is_due;
        is_due
    }

    /// Takes a frame that changed nothing, begun and drawn at those times.
    pub fn note_unchanged(&mut self, started_ns: u64, drawn_ns: u64) {
        let (duration_ns, is_animating) = (drawn_ns.saturating_sub(started_ns), self.was_animating);
        let at_ns = self.frames.note_unchanged(&mut self.stream, duration_ns, drawn_ns, self.flipped_ns, is_animating);
        self.unchanged_ns = Some(at_ns);
    }

    /// Takes a frame shown: the number the frame records give it, where it is of a run that flows.
    pub fn note_shown(&mut self, started_ns: u64, drawn_ns: u64, presented_ns: u64) -> Option<u64> {
        let (duration_ns, is_animating) = (drawn_ns.saturating_sub(started_ns), self.was_animating);
        self.flipped_ns = presented_ns;
        self.frames.note_shown(&mut self.stream, duration_ns, drawn_ns, presented_ns, is_animating)
    }

    /// Ends the drawing of a pass: the log lines of the runs that ended.
    pub fn end_draw(&mut self, is_animating: bool) -> Vec<String> {
        self.is_moving = self.was_animating || is_animating;
        self.frames.take_missed_lines(&mut self.stream, self.is_moving)
    }

    /// How long the loop waits: until a flowing viewfinder's stall, else for nothing where anything of `is_due` is, else by what moves and the deadline.
    pub fn to_wait(
        &self,
        is_due: [bool; 2],
        is_awake: bool,
        deadline_ns: Option<u64>,
        now_ns: u64,
    ) -> Option<Duration> {
        let (stalls_ns, motion_ns) = (self.stream.stalls_at_ns(), next_motion_ns(self.is_moving, self.unchanged_ns));
        wait_time(stalls_ns, is_due.contains(&true), motion_ns, is_awake, deadline_ns, now_ns)
    }

    pub fn take_picture(&mut self, viewfinder: Option<Viewfinder>, arrival_ns: u64, is_in_step: bool) {
        self.stream.take_picture(viewfinder, arrival_ns, self.flipped_ns, is_in_step);
    }

    /// The camera stopped the stream for a photo.
    pub const fn stop_stream(&mut self) {
        self.stream.stop();
    }

    /// Takes the viewfinder that shows, none asleep.
    pub fn note_viewfinder(&mut self, viewfinder: Option<Viewfinder>, now_ns: u64) {
        self.stream.note_viewfinder(viewfinder, now_ns);
    }

    /// When the last frame reached the screen.
    pub const fn flipped_ns(&self) -> u64 {
        self.flipped_ns
    }
}

/// The log line of the way down a pass leaves the loop by.
pub fn to_exit_line(power: Power) -> String {
    format!("{} requested", power.to_word())
}

/// What was asked of a pass: the first way down.
#[derive(Default)]
pub struct Pass {
    power: Option<Power>,
}

impl Pass {
    /// Takes a way down that was asked for: the first stands; true where this one does.
    pub fn add_power(&mut self, power: Option<Power>) -> bool {
        let is_first = self.power.is_none() && power.is_some();
        self.power = self.power.or(power);
        is_first
    }

    /// The way down the pass ends by.
    pub const fn to_exit(&self) -> Option<Power> {
        self.power
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_owed_until_drawn() {
        let (mut cycle, light) = (Cycle::default(), Light::default());
        assert!(cycle.begin_draw(false, true, &light), "the first frame");
        assert!(!cycle.begin_draw(false, true, &light));
        cycle.note_change(true);
        assert!(!cycle.begin_draw(false, false, &light), "asleep none");
        assert!(cycle.begin_draw(false, true, &light), "it stays owed");
        assert!(cycle.begin_draw(true, true, &light), "what moves draws a frame");
    }

    #[test]
    fn motion_redraws() {
        let (mut cycle, light) = (Cycle::default(), Light::default());
        cycle.begin_draw(true, true, &light);
        assert!(cycle.end_draw(false).is_empty(), "the frame that began moving counts");
        assert_eq!(cycle.to_wait([false; 2], true, None, 2_000), Some(Duration::ZERO));
        assert_eq!(cycle.after_wait(1_000, 5_000), 1_000, "the frame's clock stands while it moves");
        assert!(cycle.begin_draw(false, true, &light), "and the frame after it is drawn too");
        cycle.end_draw(false);
        assert_eq!(cycle.to_wait([false; 2], true, None, 2_000), None);
        assert_eq!(cycle.to_wait([true, false], true, None, 2_000), Some(Duration::ZERO));
        assert_eq!(cycle.to_wait([false; 2], true, Some(7_000), 2_000), Some(Duration::from_nanos(5_000)));
        assert_eq!(cycle.after_wait(1_000, 5_000), 5_000);
    }

    #[test]
    fn unchanged_frame_waits_refresh() {
        let (mut cycle, light) = (Cycle::default(), Light::default());
        cycle.begin_draw(true, true, &light);
        assert_eq!(cycle.note_shown(0, 1_000, 2_000), Some(1));
        assert_eq!(cycle.flipped_ns(), 2_000);
        cycle.end_draw(true);
        cycle.begin_draw(true, true, &light);
        cycle.note_unchanged(3_000, 4_000);
        cycle.end_draw(true);
        assert_eq!(cycle.to_wait([false; 2], true, None, 4_000), Some(Duration::from_nanos(REFRESH_NS - 2_000)));
    }

    #[test]
    fn wait_times() {
        assert_eq!(
            wait_time(Some(1_000), true, None, true, Some(0), 400),
            Some(Duration::from_nanos(600)),
            "pictures that flow are waited for, whatever is due"
        );
        assert_eq!(wait_time(Some(1_000), false, Some(0), true, None, 2_000), Some(Duration::ZERO), "one overdue");
        assert_eq!(wait_time(None, true, Some(9_000), true, None, 0), Some(Duration::ZERO), "nor while one is due");
        assert_eq!(wait_time(None, false, None, false, Some(7_000), 0), None, "asleep until anything comes");
        assert_eq!(wait_time(None, false, None, true, None, 0), None, "nor while nothing is due");
        assert_eq!(wait_time(None, false, None, true, Some(7_000), 2_000), Some(Duration::from_nanos(5_000)));
        assert_eq!(
            wait_time(None, false, Some(0), true, None, 2_000),
            Some(Duration::ZERO),
            "what moves at once after a frame shown"
        );
        assert_eq!(
            wait_time(None, false, Some(3_000), true, Some(9_000), 2_000),
            Some(Duration::from_nanos(1_000)),
            "and at the next refresh after one that changed nothing, before the deadline"
        );
        assert_eq!(
            [next_motion_ns(true, None), next_motion_ns(true, Some(3_000)), next_motion_ns(false, Some(3_000))],
            [Some(0), Some(3_000), None],
            "what moves is drawn at once after a frame shown, at the refresh after one that changed nothing, and not at rest"
        );
    }

    #[test]
    fn first_exit_stands() {
        let mut pass = Pass::default();
        assert_eq!(pass.to_exit(), None);
        assert!(!pass.add_power(None));
        assert!(pass.add_power(Some(Power::PowerOff)) && !pass.add_power(Some(Power::Reboot)));
        assert_eq!(pass.to_exit(), Some(Power::PowerOff));
        assert_eq!(
            [Power::PowerOff, Power::Reboot, Power::Bootloader].map(to_exit_line),
            ["PowerOff requested", "Reboot requested", "Bootloader requested"],
            "each way down by its word"
        );
    }
}
