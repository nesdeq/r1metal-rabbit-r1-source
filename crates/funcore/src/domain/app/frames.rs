use super::light::Light;
use crate::domain::camera::Viewfinder;
use crate::gfx::frame_stats::{Misses, Run, to_millis};
use crate::gfx::refresh::{Catchup, REFRESH_NS, next_refresh};
use crate::hw::camera::stream::READY_AFTER_FLIP_NS;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const STALL_REFRESHES: u64 = 3;
const DRAW_MARGIN_NS: u64 = REFRESH_NS.saturating_sub(READY_AFTER_FLIP_NS);

pub(super) struct Stream {
    viewfinder: Option<Viewfinder>,
    has_picture: bool,
    is_timely: bool,
    picture_ns: u64,
    catchup: Catchup,
    run: Run,
}

impl Default for Stream {
    fn default() -> Stream {
        Stream {
            viewfinder: None,
            has_picture: false,
            is_timely: false,
            picture_ns: 0,
            catchup: Catchup::new(DRAW_MARGIN_NS),
            run: Run::default(),
        }
    }
}

impl Stream {
    pub(super) fn take_picture(
        &mut self,
        viewfinder: Option<Viewfinder>,
        arrival_ns: u64,
        flipped_ns: u64,
        is_in_step: bool,
    ) {
        self.has_picture = true;
        self.viewfinder = viewfinder;
        self.picture_ns = arrival_ns;
        self.is_timely = self.catchup.should_take(arrival_ns, flipped_ns, is_in_step);
    }

    pub(super) const fn stop(&mut self) {
        self.viewfinder = None;
        self.has_picture = false;
    }

    pub(super) fn note_viewfinder(&mut self, viewfinder: Option<Viewfinder>, now_ns: u64) {
        let has_stalled = now_ns.saturating_sub(self.picture_ns) > STALL_REFRESHES * REFRESH_NS;
        self.viewfinder = self.viewfinder.filter(|current| Some(*current) == viewfinder && !has_stalled);
        if self.viewfinder.is_none() {
            self.has_picture = false;
            self.catchup = Catchup::new(DRAW_MARGIN_NS);
        }
    }

    const fn is_due(&self) -> bool {
        self.has_picture && self.is_timely
    }

    fn note_shown(&mut self, duration_ns: u64, drawn_ns: u64, presented_ns: u64) -> bool {
        let had_picture = core::mem::take(&mut self.has_picture);
        if had_picture {
            self.run.record(duration_ns, presented_ns, REFRESH_NS);
            self.catchup.note_shown(drawn_ns.saturating_sub(self.picture_ns));
        }
        had_picture
    }

    pub(super) const fn stalls_at_ns(&self) -> Option<u64> {
        if self.viewfinder.is_some() { Some(self.picture_ns + STALL_REFRESHES * REFRESH_NS) } else { None }
    }

    fn take_missed(&mut self) -> Option<Misses> {
        if self.viewfinder.is_some() { None } else { self.run.end() }
    }
}

pub(super) const fn should_draw(is_awake: bool, light: &Light, stream: &Stream, is_owed: bool) -> bool {
    !light.is_blank(is_awake) && if stream.viewfinder.is_some() { stream.is_due() } else { is_owed }
}

#[derive(Default)]
pub(super) struct Frames {
    run: Run,
    count: u64,
}

fn to_missed_line(run_name: &str, misses: Misses) -> String {
    format!(
        "frames: {} of {run_name} of {} came late, the latest {} ms after the one before it, the slowest drawn in {} ms",
        misses.late,
        misses.frames,
        to_millis(misses.longest_ns),
        to_millis(misses.slowest_draw_ns)
    )
}

impl Frames {
    pub(super) fn note_shown(
        &mut self,
        stream: &mut Stream,
        duration_ns: u64,
        drawn_ns: u64,
        presented_ns: u64,
        is_animating: bool,
    ) -> Option<u64> {
        if is_animating {
            self.run.record(duration_ns, presented_ns, REFRESH_NS);
        }
        let is_flowing = stream.note_shown(duration_ns, drawn_ns, presented_ns) || is_animating;
        match (is_flowing, stream.viewfinder) {
            (true, _) => {
                self.count += 1;
                Some(self.count)
            }
            (false, None) => {
                self.count = 0;
                None
            }
            (false, Some(_)) => None,
        }
    }

    pub(super) fn note_unchanged(
        &mut self,
        stream: &mut Stream,
        duration_ns: u64,
        drawn_ns: u64,
        flipped_ns: u64,
        is_animating: bool,
    ) -> u64 {
        let at_ns = next_refresh(flipped_ns, drawn_ns);
        if is_animating {
            self.run.note_repeat(duration_ns, at_ns, REFRESH_NS);
        }
        if core::mem::take(&mut stream.has_picture) {
            stream.run.note_repeat(duration_ns, at_ns, REFRESH_NS);
        }
        at_ns
    }

    pub(super) fn take_missed_lines(&mut self, stream: &mut Stream, is_animating: bool) -> Vec<String> {
        let motion_misses = if is_animating { None } else { self.run.end() };
        [(motion_misses, "a run"), (stream.take_missed(), "a viewfinder's run")]
            .into_iter()
            .filter_map(|(misses, run_name)| Some(to_missed_line(run_name, misses?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gallery::Kind;
    use crate::hw::camera::swivel::Facing;

    const PHOTO: Viewfinder =
        Viewfinder { kind: Kind::Photo, facing: Facing::Rear, is_scanning: false, is_calling: false, size: (480, 480) };
    const FRONT: Viewfinder = Viewfinder { facing: Facing::Front, ..PHOTO };

    #[test]
    fn viewfinder_stream() {
        let mut stream = Stream::default();
        assert_eq!((stream.viewfinder, stream.is_due(), stream.stalls_at_ns()), (None, false, None));
        stream.take_picture(Some(PHOTO), 400, 500, true);
        assert_eq!(
            (stream.viewfinder, stream.is_due()),
            (Some(PHOTO), false),
            "the first picture waited through a flip"
        );
        stream.take_picture(Some(PHOTO), 20_000_000, 10_000_000, true);
        assert!(stream.is_due(), "a picture that came after the flip is drawn");
        assert!(stream.note_shown(4_000_000, 24_000_000, 26_000_000) && !stream.is_due(), "drawn once");
        assert!(!stream.note_shown(4_000_000, 40_000_000, 42_000_000), "a frame without a new picture is none of its");
        assert_eq!(stream.stalls_at_ns(), Some(20_000_000 + 3 * REFRESH_NS));
        stream.note_viewfinder(Some(PHOTO), 20_000_000 + 3 * REFRESH_NS);
        assert_eq!(stream.viewfinder, Some(PHOTO), "a stream waits three refreshes for its next picture");
        assert_eq!(stream.take_missed(), None);
        stream.note_viewfinder(Some(FRONT), 21_000_000);
        assert_eq!(
            (stream.viewfinder, stream.stalls_at_ns()),
            (None, None),
            "turned round, it has no stream until one comes"
        );
        assert_eq!(stream.take_missed(), None, "and its run missed nothing");
        stream.take_picture(Some(PHOTO), 30_000_000, 20_000_000, true);
        stream.stop();
        assert_eq!(
            (stream.viewfinder, stream.is_due()),
            (None, false),
            "a stream stopped for a photo has none, nor is the picture before the stop drawn"
        );
        stream.take_picture(None, 31_000_000, 20_000_000, true);
        assert_eq!(stream.viewfinder, None, "nor is one where no viewfinder shows");
        stream.take_picture(Some(PHOTO), 40_000_000, 30_000_000, true);
        stream.note_viewfinder(Some(PHOTO), 40_000_000 + 3 * REFRESH_NS + 1);
        assert_eq!(stream.viewfinder, None, "pictures that stopped coming end the stream");
    }

    #[test]
    fn draw_decision() {
        let (mut light, mut stream) = (Light::default(), Stream::default());
        assert!(
            should_draw(true, &light, &stream, true) && !should_draw(true, &light, &stream, false),
            "where one is owed"
        );
        assert!(light.is_blank(false) && !should_draw(false, &light, &stream, true), "asleep none");
        light.set_dark(true, true);
        assert!(light.is_blank(true) && !should_draw(true, &light, &stream, true), "nor in the dark under a task");
        light.set_dark(true, false);
        stream.take_picture(Some(PHOTO), 400, 500, true);
        assert!(!should_draw(true, &light, &stream, true), "while pictures flow a frame waits for its picture");
        stream.take_picture(Some(PHOTO), 20_000_000, 10_000_000, true);
        assert!(should_draw(true, &light, &stream, false), "and is drawn once it has come, owed or not");
    }

    #[test]
    fn run_numbers_and_misses() {
        let (mut frames, mut stream) = (Frames::default(), Stream::default());
        assert_eq!(
            frames.note_shown(&mut stream, 4_000_000, 10_000_000, 12_000_000, true),
            Some(1),
            "a motion's frame"
        );
        assert_eq!(frames.note_shown(&mut stream, 3_000_000, 50_000_000, 52_000_000, true), Some(2), "and one late");
        assert!(frames.take_missed_lines(&mut stream, true).is_empty(), "nothing ends while something moves");
        assert_eq!(frames.note_shown(&mut stream, 1_000_000, 60_000_000, 62_000_000, false), None, "a frame of no run");
        assert_eq!(
            frames.take_missed_lines(&mut stream, false),
            [
                "frames: 1 of a run of 2 came late, the latest 40.0 ms after the one before it, the slowest drawn in 4.0 ms"
            ]
        );
        assert!(frames.take_missed_lines(&mut stream, false).is_empty(), "told once");
        assert_eq!(
            frames.note_shown(&mut stream, 4_000_000, 70_000_000, 72_000_000, true),
            Some(1),
            "a new run begins anew"
        );
        stream.take_picture(Some(PHOTO), 80_000_000, 70_000_000, true);
        assert_eq!(
            frames.note_shown(&mut stream, 4_000_000, 84_000_000, 86_000_000, false),
            Some(2),
            "a viewfinder's flows on"
        );
        assert_eq!(
            frames.note_shown(&mut stream, 1_000_000, 90_000_000, 92_000_000, false),
            None,
            "a frame without a picture leaves a viewfinder's run as it stands"
        );
        stream.take_picture(Some(PHOTO), 96_000_000, 92_000_000, true);
        assert_eq!(frames.note_shown(&mut stream, 4_000_000, 100_000_000, 102_000_000, false), Some(3));
    }

    #[test]
    fn unchanged_frames() {
        let (mut frames, mut stream) = (Frames::default(), Stream::default());
        frames.note_shown(&mut stream, 1_000_000, 10_000_000, 10_000_000, true);
        assert_eq!(
            frames.note_unchanged(&mut stream, 1_000_000, 11_000_000, 10_000_000, true),
            10_000_000 + REFRESH_NS,
            "the panel's next refresh after the flip shows the frame before"
        );
        frames.note_shown(&mut stream, 1_000_000, 10_000_000, 10_000_000 + 2 * REFRESH_NS, true);
        assert!(frames.take_missed_lines(&mut stream, false).is_empty(), "the refresh between showed the frame before");
        stream.take_picture(Some(PHOTO), 400, 500, true);
        stream.take_picture(Some(PHOTO), 20_000_000, 10_000_000, true);
        assert!(stream.is_due());
        frames.note_unchanged(&mut stream, 1_000_000, 30_000_000, 20_000_000, false);
        assert!(!stream.is_due(), "a picture whose frame changed nothing counts as drawn");
    }
}
