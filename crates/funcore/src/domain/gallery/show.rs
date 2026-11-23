//! The playing video's queued pictures and its clock.

use crate::base::time::NANOS_PER_SECOND;
use crate::gfx::Picture;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub(super) const QUEUE_LENGTH: usize = 4;
const REFRESH_TOLERANCE: u64 = 10;
const TOLERANCE_UNIT: u64 = 1000;

/// The multiplier and divisor that turn a video clock's ticks into nanoseconds.
pub fn time_scale(pace: u64, ticks_per_second: u32, refresh_ns: u64) -> (u64, u64) {
    let exact_scale = (NANOS_PER_SECOND, u64::from(ticks_per_second.max(1)));
    let pace_ns = pace * exact_scale.0 / exact_scale.1;
    let is_near = pace_ns.abs_diff(refresh_ns).saturating_mul(TOLERANCE_UNIT) <= refresh_ns * REFRESH_TOLERANCE;
    if pace > 0 && is_near { (refresh_ns, pace) } else { exact_scale }
}

/// The due time of a picture `ticks` after the track's first.
pub fn due_ns(ticks: u64, (multiplier, divisor): (u64, u64)) -> u64 {
    (u128::from(ticks) * u128::from(multiplier) / u128::from(divisor)) as u64
}

/// A drawn picture, with the time on the video's clock from which it shows.
pub struct Frame {
    pub picture: Picture,
    due_ns: u64,
}

/// The screen's counts against the clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ClockReport {
    /// Pictures the screen took.
    pub shown_count: u32,
    pub(super) starved_count: u32,
    pub(super) skipped_count: u32,
}

/// The playing video's pictures and its clock, which moves a refresh in each refresh a picture waits.
#[derive(Default)]
pub struct Show {
    pub(super) is_playing: bool,
    queue: VecDeque<Frame>,
    has_last: bool,
    spares: Vec<Picture>,
    position_ns: Option<u64>,
    report: ClockReport,
    ask_ns: u64,
    last_refresh: Option<(u64, u64)>,
}

impl Show {
    /// Starts playing, the clock at the video's start.
    pub fn play(&mut self) {
        self.is_playing = true;
        self.has_last = false;
        self.position_ns = None;
        self.report = ClockReport::default();
        self.last_refresh = None;
    }

    /// A picture to draw into.
    pub fn take_blank(&mut self) -> Picture {
        self.spares.pop().unwrap_or_default()
    }

    /// Takes back a picture the screen is done with.
    pub fn give_back(&mut self, picture: Picture) {
        self.spares.push(picture);
    }

    /// Queues a drawn picture.
    pub fn add(&mut self, picture: Picture, due_ns: u64) {
        self.queue.push_back(Frame { picture, due_ns });
    }

    /// The pictures still to draw for a full queue.
    pub fn missing_count(&self) -> usize {
        QUEUE_LENGTH.saturating_sub(self.queue.len())
    }

    pub(super) fn mark_last(&mut self) -> bool {
        self.has_last = true;
        self.queue.is_empty()
    }

    /// Stops playing: the newest picture is handed out to stand, every other is drawn over.
    pub fn hold(&mut self) -> (ClockReport, Option<Frame>) {
        let report = core::mem::take(&mut self.report);
        self.position_ns = None;
        self.is_playing = false;
        self.has_last = false;
        let newest = self.queue.pop_back();
        self.spares.extend(self.queue.drain(..).map(|frame| frame.picture));
        (report, newest)
    }

    /// Stops playing and hands out every picture, to drop where that stalls nothing.
    pub fn clear(&mut self) -> (VecDeque<Frame>, Vec<Picture>) {
        self.is_playing = false;
        self.has_last = false;
        self.position_ns = None;
        self.report = ClockReport::default();
        (core::mem::take(&mut self.queue), core::mem::take(&mut self.spares))
    }

    fn next_position(&mut self, refresh_ns: u64) -> Option<u64> {
        match (self.position_ns, self.queue.front()) {
            (None, Some(first)) if self.queue.len() >= QUEUE_LENGTH || self.has_last => Some(first.due_ns),
            (None, _) => None,
            (Some(position), Some(_)) => Some(position + refresh_ns),
            (Some(position), None) => {
                self.report.starved_count += u32::from(!self.has_last);
                Some(position)
            }
        }
    }

    /// Once a refresh, moves the clock and swaps the due picture into `picture`.
    pub fn take_frame(
        &mut self,
        picture: &mut Picture,
        refresh_ns: u64,
        (flipped_ns, now_ns): (u64, u64),
    ) -> Option<(u64, bool)> {
        if !self.is_playing {
            return None;
        }
        let refresh = (flipped_ns, now_ns.saturating_sub(flipped_ns) / refresh_ns.max(1));
        if self.last_refresh == Some(refresh) {
            return self.position_ns.map(|position| (position, false));
        }
        let position = self.next_position(refresh_ns)?;
        self.position_ns = Some(position);
        self.ask_ns = now_ns;
        self.last_refresh = Some(refresh);
        let mut is_new = false;
        while let Some(mut frame) = self.queue.pop_front_if(|frame| frame.due_ns <= position) {
            core::mem::swap(picture, &mut frame.picture);
            self.spares.push(frame.picture);
            self.report.skipped_count += u32::from(is_new);
            is_new = true;
        }
        self.report.shown_count += u32::from(is_new);
        Some((position, is_new))
    }

    /// The clock's position at the screen's last ask, and that ask's time: what the sound goes by.
    pub fn last_ask(&self) -> Option<(u64, u64)> {
        Some((self.position_ns?, self.ask_ns))
    }

    /// The sample that sounds at `now_ns` once the `pending` samples have, by `clock`.
    pub fn sound_place(&self, clock: &SoundClock, now_ns: u64, pending: usize) -> Option<f64> {
        let (position_ns, ask_ns) = self.last_ask()?;
        Some(clock.sample_at(position_ns, now_ns.saturating_sub(ask_ns), pending))
    }
}

/// The speaker's place in a video's sound, by the clock its pictures show by.
#[derive(Clone, Copy)]
pub struct SoundClock {
    pub scale: (u64, u64),
    pub ticks_per_second: u32,
    /// The sound's samples a second.
    pub rate: u32,
    pub refresh_ns: u64,
}

impl SoundClock {
    fn sample_at(&self, position_ns: u64, elapsed_ns: u64, pending: usize) -> f64 {
        let (multiplier, divisor) = self.scale;
        let rate = f64::from(self.rate);
        // The picture the clock stood at shows a refresh after it was asked for, and stands until the next is.
        let elapsed = elapsed_ns.min(self.refresh_ns) as f64;
        let pending_ns = pending as f64 * NANOS_PER_SECOND as f64 / rate;
        let shown_ns = position_ns as f64 + elapsed - self.refresh_ns as f64 + pending_ns;
        shown_ns * divisor as f64 / multiplier as f64 * rate / f64::from(self.ticks_per_second.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const PANEL: u64 = 16_666_666;
    const REFRESH: u64 = 1000;

    #[test]
    fn clock_near_refresh_rate() {
        assert_eq!(time_scale(7999, 480_000, PANEL), (PANEL, 7999), "our recordings, 60.0075 pictures a second");
        assert_eq!(
            (0..4).map(|picture| due_ns(picture * 7999, (PANEL, 7999))).collect::<Vec<u64>>(),
            [0, PANEL, 2 * PANEL, 3 * PANEL]
        );
        assert_eq!(
            due_ns(2 * 7999, (PANEL, 7999)),
            2 * PANEL,
            "a picture that lasts twice as long takes two refreshes"
        );
        assert_eq!(time_scale(1001, 60_000, PANEL), (PANEL, 1001), "59.94 a second");
        assert_eq!(time_scale(16_000, 480_000, PANEL), (1_000_000_000, 480_000), "30 a second keeps its own time");
        assert_eq!(due_ns(16_000, time_scale(16_000, 480_000, PANEL)), 33_333_333);
        assert_eq!(
            (time_scale(1000, 50_000, PANEL), time_scale(1000, 61_000, PANEL)),
            ((1_000_000_000, 50_000), (1_000_000_000, 61_000)),
            "50 and 61 a second are too far off"
        );
        assert_eq!(time_scale(1000, 60_500, PANEL), (PANEL, 1000), "within a hundredth");
        assert_eq!(
            (time_scale(0, 600, PANEL), time_scale(100, 0, PANEL)),
            ((1_000_000_000, 600), (1_000_000_000, 1)),
            "a track without pictures or without a clock"
        );
        assert_eq!(
            due_ns(u64::from(u32::MAX) * 7999, (PANEL, 7999)),
            u64::from(u32::MAX) * PANEL,
            "hours of video do not overflow"
        );
    }

    fn marked(word: u32) -> Picture {
        Picture { words: vec![word], width: 1 }
    }

    fn ask(show: &mut Show, shown: &mut Picture, at: &mut u64) -> Option<(u64, bool)> {
        *at += 1;
        show.take_frame(shown, REFRESH, (0, *at * REFRESH))
    }

    fn play(show: &mut Show, first: u32, gap: u64) {
        show.play();
        for number in 0..QUEUE_LENGTH as u32 {
            show.add(marked(first + number), u64::from(number) * gap);
        }
    }

    #[test]
    fn one_picture_per_refresh() {
        let (mut show, mut shown, mut at) = (Show::default(), marked(99), 0);
        show.add(marked(0), 0);
        assert_eq!(ask(&mut show, &mut shown, &mut at), None, "nothing plays yet");
        show.play();
        assert!(show.is_playing);
        for number in 1..QUEUE_LENGTH as u32 {
            assert_eq!(ask(&mut show, &mut shown, &mut at), None, "{number} drawn ahead are too few to begin");
            show.add(marked(number), u64::from(number) * REFRESH);
        }
        assert_eq!(show.missing_count(), 0);
        for number in 0..QUEUE_LENGTH as u32 {
            assert_eq!(ask(&mut show, &mut shown, &mut at), Some((u64::from(number) * REFRESH, true)));
            assert_eq!(shown.words, [number]);
        }
        assert_eq!(
            show.take_blank().words,
            [QUEUE_LENGTH as u32 - 2],
            "what the screen has done with comes back to be drawn over"
        );
        assert_eq!(show.missing_count(), QUEUE_LENGTH);
        assert!(
            [ask(&mut show, &mut shown, &mut at), ask(&mut show, &mut shown, &mut at)]
                .iter()
                .all(|answer| !answer.unwrap().1),
            "nothing ready"
        );
        show.add(marked(7), QUEUE_LENGTH as u64 * REFRESH);
        assert_eq!(
            ask(&mut show, &mut shown, &mut at),
            Some((QUEUE_LENGTH as u64 * REFRESH, true)),
            "the clock stood while nothing was ready, so the next picture is due at once"
        );
        assert_eq!(
            (&shown.words[..], show.last_ask()),
            (&[7][..], Some((QUEUE_LENGTH as u64 * REFRESH, at * REFRESH)))
        );
        let (clock, newest) = show.hold();
        assert_eq!(
            (clock, newest.is_none(), show.is_playing, show.last_ask()),
            (
                ClockReport { shown_count: QUEUE_LENGTH as u32 + 1, starved_count: 2, skipped_count: 0 },
                true,
                false,
                None
            )
        );
    }

    #[test]
    fn slow_and_fast_videos() {
        let (mut show, mut shown, mut at) = (Show::default(), marked(99), 0);
        play(&mut show, 0, 2 * REFRESH);
        let news: Vec<bool> = (0..6).map(|_| ask(&mut show, &mut shown, &mut at).unwrap().1).collect();
        assert_eq!(
            (news, &shown.words[..]),
            (vec![true, false, true, false, true, false], &[2u32][..]),
            "half the screen's rate: every picture for two refreshes"
        );
        assert_eq!(show.hold().0, ClockReport { shown_count: 3, starved_count: 0, skipped_count: 0 });
        play(&mut show, 10, REFRESH / 2);
        assert!(
            ask(&mut show, &mut shown, &mut at).unwrap().1 && shown.words == [10],
            "the first picture alone is due at the start"
        );
        assert!(
            ask(&mut show, &mut shown, &mut at).unwrap().1 && shown.words == [12],
            "twice the screen's rate: every second picture"
        );
        assert_eq!(show.hold().0.skipped_count, 1);
    }

    #[test]
    fn second_ask_in_a_refresh() {
        let (mut show, mut shown) = (Show::default(), marked(99));
        play(&mut show, 0, REFRESH);
        assert_eq!(show.take_frame(&mut shown, REFRESH, (0, 0)), Some((0, true)));
        assert_eq!(
            (show.take_frame(&mut shown, REFRESH, (0, REFRESH / 2)), &shown.words[..]),
            (Some((0, false)), &[0u32][..]),
            "a second ask in the same refresh changes nothing"
        );
        assert_eq!(
            show.take_frame(&mut shown, REFRESH, (0, REFRESH)),
            Some((REFRESH, true)),
            "the next refresh moves it"
        );
        assert_eq!(
            show.take_frame(&mut shown, REFRESH, (REFRESH + 3, REFRESH + 5)),
            Some((2 * REFRESH, true)),
            "and so does the first ask after a flip"
        );
        assert_eq!(show.last_ask(), Some((2 * REFRESH, REFRESH + 5)), "the sound goes by the ask that moved the clock");
        show.take_frame(&mut shown, REFRESH, (REFRESH + 3, REFRESH + 9));
        assert_eq!(show.last_ask(), Some((2 * REFRESH, REFRESH + 5)), "and not by one that did not");
        let clock = show.hold().0;
        assert_eq!((clock.shown_count, clock.starved_count), (3, 0), "each refresh counted once");
    }

    #[test]
    fn last_pictures_begin_short_unstarved() {
        let (mut show, mut shown, mut at) = (Show::default(), marked(99), 0);
        show.play();
        show.add(marked(1), 0);
        show.add(marked(2), REFRESH);
        assert_eq!(ask(&mut show, &mut shown, &mut at), None);
        assert!(!show.mark_last(), "two wait still");
        let news: Vec<bool> = (0..3).map(|_| ask(&mut show, &mut shown, &mut at).unwrap().1).collect();
        assert_eq!(news, [true, true, false]);
        assert!(show.mark_last(), "every picture is shown");
        assert_eq!(
            (shown.words.clone(), show.hold().0.starved_count),
            (vec![2], 0),
            "a video that has ended lacks no picture"
        );
    }

    #[test]
    fn hold_and_clear() {
        let (mut show, mut shown, mut at) = (Show::default(), marked(99), 0);
        play(&mut show, 0, REFRESH);
        assert!(ask(&mut show, &mut shown, &mut at).unwrap().1);
        let (_, newest) = show.hold();
        assert_eq!(
            newest.map(|frame| frame.picture.words),
            Some(vec![QUEUE_LENGTH as u32 - 1]),
            "the last picture drawn"
        );
        assert_eq!(ask(&mut show, &mut shown, &mut at), None, "nothing plays any more");
        assert_eq!(show.missing_count(), QUEUE_LENGTH, "none waits");
        let (queue, spares) = show.clear();
        assert_eq!(
            (queue.len(), spares.len()),
            (0, QUEUE_LENGTH - 1),
            "every other picture was there to be drawn over"
        );
        assert!(show.take_blank().words.is_empty(), "and is let go of with the clearing");
    }

    #[test]
    fn sound_place_by_last_ask() {
        let clock = SoundClock {
            scale: (NANOS_PER_SECOND, 480_000),
            ticks_per_second: 480_000,
            rate: 48_000,
            refresh_ns: PANEL,
        };
        let (mut show, mut shown, mut at) = (Show::default(), marked(99), 0);
        assert_eq!(show.sound_place(&clock, 0, 0), None, "nothing plays");
        play(&mut show, 0, REFRESH);
        ask(&mut show, &mut shown, &mut at);
        let (position_ns, ask_ns) = show.last_ask().unwrap();
        assert_eq!(show.sound_place(&clock, ask_ns + 5, 9), Some(clock.sample_at(position_ns, 5, 9)));
        assert_eq!(show.sound_place(&clock, 0, 9), Some(clock.sample_at(position_ns, 0, 9)), "a clock behind the ask");
    }

    #[test]
    fn sample_a_refresh_back() {
        let clock = SoundClock {
            scale: (NANOS_PER_SECOND, 480_000),
            ticks_per_second: 480_000,
            rate: 48_000,
            refresh_ns: PANEL,
        };
        assert!((clock.sample_at(0, PANEL, 480) - 480.0).abs() < 1e-6, "what is on its way sounds after it");
        assert!(
            (clock.sample_at(NANOS_PER_SECOND, PANEL, 0) - 48_000.0).abs() < 1e-6,
            "a second in, a second of sound"
        );
        assert_eq!(
            clock.sample_at(NANOS_PER_SECOND, 10 * PANEL, 0),
            clock.sample_at(NANOS_PER_SECOND, PANEL, 0),
            "a refresh at most"
        );
        assert!(
            (clock.sample_at(NANOS_PER_SECOND, 0, 0) - (48_000.0 - PANEL as f64 * 48_000.0 / 1e9)).abs() < 1e-6,
            "just asked, the clock's picture is a refresh from showing"
        );
        let recorded = SoundClock { scale: (PANEL, 7999), ..clock };
        assert!(
            (recorded.sample_at(PANEL, PANEL, 0) - 799.9).abs() < 1e-6,
            "a picture of our recordings is 7999 ticks, 799.9 samples"
        );
    }
}
