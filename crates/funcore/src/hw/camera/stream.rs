//! A viewfinder's stream: the frames a mode passes over, its timeouts, when its pictures show, and when a click is taken.

use crate::hw::camera::exposure::FRAMES_PER_SECOND;
use crate::hw::camera::exposure::Pace;
use crate::hw::camera::genlock::Genlock;
use core::time::Duration;

const UNSETTLED_FRAMES: u64 = 3;
const HEALTH_FRAMES: u64 = 2;
const SETTLE_FRAMES: u64 = UNSETTLED_FRAMES.saturating_sub(HEALTH_FRAMES);

/// The slot each frame of a photo's burst of `frames` goes into: the frames the mode passes over into the first, which the burst's first frame then fills anew.
pub fn burst_slots(frames: usize) -> impl Iterator<Item = usize> {
    core::iter::repeat_n(0, SETTLE_FRAMES as usize).chain(0..frames)
}
const TIMEOUT_FRAMES: u64 = 4;
pub(crate) const READY_AFTER_FLIP_NS: u64 = 6_000_000;
const FIND_FRAMES: u64 = 24;
const LOOK_FRAMES: u64 = FRAMES_PER_SECOND as u64 / 2;

/// A stream's pictures kept at their place after the screen's flips, and held back until they are there.
pub struct FrameSync {
    genlock: Genlock,
    is_showing: bool,
    frames: u64,
}

impl FrameSync {
    /// For a stream at a pace beside a screen that refreshes every `refresh_ns`, from its first frame.
    pub const fn new(pace: Pace, refresh_ns: u64) -> FrameSync {
        FrameSync { genlock: Genlock::new(pace, refresh_ns), is_showing: false, frames: 0 }
    }

    /// Takes when the current frame's picture was ready and the screen last flipped; the sensor's new frame lines where they change.
    pub fn take_ready(&mut self, ready_ns: u64, flipped_ns: u64) -> Option<u32> {
        self.update(ready_ns, flipped_ns, self.frames)
    }

    fn update(&mut self, ready_ns: u64, flipped_ns: u64, frames: u64) -> Option<u32> {
        let lines = self.genlock.take_ready(ready_ns, flipped_ns);
        self.is_showing |=
            frames >= SETTLE_FRAMES && (self.genlock.is_in_step || frames >= SETTLE_FRAMES + FIND_FRAMES);
        lines
    }

    /// Counts a frame of the stream as done with.
    pub const fn note_frame(&mut self) {
        self.frames += 1;
    }

    /// The frames of the stream done with.
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// True once the stream has settled and found its place, or has had its time to find it.
    pub const fn is_showing(&self) -> bool {
        self.is_showing
    }

    /// True while the stream's pictures come at their place after the screen's flips.
    pub const fn is_in_step(&self) -> bool {
        self.genlock.is_in_step
    }
}

/// How long the receiver gets to show whole frames, and a frame to arrive, for frames `frame_micros` long.
pub const fn timeout(frame_micros: u64) -> Duration {
    Duration::from_micros(TIMEOUT_FRAMES * frame_micros)
}

pub(crate) const fn is_settled(frames: u64) -> bool {
    frames > SETTLE_FRAMES
}

// The camera's warmth and a recording's room are looked at on these frames.
pub(crate) const fn is_look_frame(frames: u64) -> bool {
    frames.is_multiple_of(LOOK_FRAMES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::refresh::REFRESH_NS;
    use crate::hw::camera::develop;
    use crate::hw::camera::exposure::Auto;
    use crate::hw::camera::exposure::LINE_LENGTH;
    use crate::hw::camera::exposure::PIXEL_CLOCK_HZ;
    use crate::hw::camera::recording;
    use alloc::vec::Vec;

    #[test]
    fn live_pace() {
        let second = |lines: u32| u64::from(lines) * LINE_LENGTH * u64::from(FRAMES_PER_SECOND);
        assert!(
            second(Pace::LIVE.frame_lines) <= PIXEL_CLOCK_HZ && second(Pace::LIVE.frame_lines + 1) > PIXEL_CLOCK_HZ,
            "the longest frames of which 60 fit a second of the pixel clock"
        );
        assert!(
            Pace::LIVE.frame_lines > recording::MODE.frame_lines(),
            "longer than the driver has slim video's, the video viewfinder's mode"
        );
        assert!(
            Pace::LIVE.frame_lines < develop::VIEW_MODE.frame_lines(),
            "and shorter than it has the binned full frame's, the photo viewfinder's"
        );
        assert_eq!(
            (Auto::live().pace(), Auto::live().exposure()),
            (Pace::LIVE, Pace::LIVE.start()),
            "both viewfinders' automatic"
        );
        let still = Auto::live().still().pace;
        assert_eq!(still.frame_lines, develop::MODE.frame_lines(), "a photo's frame is as the driver has it");
        assert!(
            Pace::LIVE.start().lines + 6 <= Pace::LIVE.frame_lines,
            "the longest exposure of a stream fits its frame with the sensor's margin"
        );
    }

    #[test]
    fn four_frame_timeout() {
        assert_eq!(timeout(16_666), Duration::from_micros(66_664), "of a viewfinder's frames");
        assert_eq!(timeout(33_329), Duration::from_micros(133_316), "of a photo's");
    }

    #[test]
    fn when_pictures_show() {
        let mut alone = FrameSync::new(Pace::LIVE, 0);
        assert_eq!((alone.update(1_000, 0, 0), alone.is_showing()), (None, false), "the frame a mode passes over");
        alone.update(2_000, 0, SETTLE_FRAMES);
        assert!(alone.is_showing() && alone.is_in_step(), "beside no screen there is no place to wait for");
        let (mut lost, flipped) = (FrameSync::new(Pace::LIVE, REFRESH_NS), 1_000_000_000);
        let shows: Vec<bool> = (0..=SETTLE_FRAMES + FIND_FRAMES)
            .map(|frames| {
                lost.update(flipped + READY_AFTER_FLIP_NS + 8_000_000, flipped, frames);
                lost.is_showing()
            })
            .collect();
        assert!(
            !lost.is_in_step() && shows.iter().rev().skip(1).all(|shows| !shows) && shows.last() == Some(&true),
            "pictures that never come at their place show once the stream has looked for it as long as it gets: {shows:?}"
        );
    }

    #[test]
    fn click_and_look_frames() {
        assert!(
            !is_settled(SETTLE_FRAMES) && is_settled(SETTLE_FRAMES + 1),
            "a click on a frame a mode passes over waits"
        );
        assert_eq!((1..=u64::from(FRAMES_PER_SECOND)).filter(|&frames| is_look_frame(frames)).count(), 2);
        assert_eq!(
            burst_slots(3).collect::<alloc::vec::Vec<_>>(),
            [0, 0, 1, 2],
            "the frame the driver says to pass over once the receiver is healthy, then the burst's, each in a slot of its own"
        );
    }
}
