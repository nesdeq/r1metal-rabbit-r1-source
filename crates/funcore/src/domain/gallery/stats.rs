//! The log's figures of a played video.

use super::show::ClockReport;
use crate::base::time::NANOS_PER_MICRO;
use crate::codec::mp4::Movie;
use crate::input::pose::Pose;
use alloc::format;
use alloc::string::String;

/// The log line of a video on show: its pictures' size, its file's figures, the shown size, and the pose.
pub fn to_shown_line(
    name: &str,
    (width, height): (u32, u32),
    movie: &Movie,
    (shown_width, shown_height): (usize, usize),
    pose: Pose,
) -> String {
    format!(
        "gallery: {name}: {width}x{height} pictures that the file turns by {} quarters, {} samples of {}/{} s, shown at {shown_width}x{shown_height}, held {pose:?}",
        movie.quarters,
        movie.samples.len(),
        movie.frame_ticks(),
        movie.ticks_per_second
    )
}

/// The decoding and drawing figures of a video since the last log line.
#[derive(Default)]
pub struct PlayStats {
    sample_count: u32,
    flawed_count: u32,
    skipped_count: u32,
    decoding_us: u64,
    slowest_decode_us: u64,
    drawn_count: u32,
    drawing_ns: u64,
    slowest_draw_ns: u64,
    busy_ns: u64,
}

impl PlayStats {
    /// Counts a sample the decoder made no picture of.
    pub fn add_skipped(&mut self) {
        self.skipped_count += 1;
    }

    /// Counts a decoded sample, the codec block locked `lock_us` for it.
    pub fn add_decoded(&mut self, is_flawed: bool, lock_us: u64) {
        self.sample_count += 1;
        self.flawed_count += u32::from(is_flawed);
        self.decoding_us += lock_us;
        self.slowest_decode_us = self.slowest_decode_us.max(lock_us);
    }

    /// Counts a drawn picture, the process busy `busy_ns` meanwhile.
    pub fn add_drawn(&mut self, duration_ns: u64, busy_ns: u64) {
        self.drawn_count += 1;
        self.drawing_ns += duration_ns;
        self.slowest_draw_ns = self.slowest_draw_ns.max(duration_ns);
        self.busy_ns += busy_ns;
    }

    /// The video's log line; the count starts over.
    pub fn take_log_line(&mut self, name: &str, clock: ClockReport) -> String {
        let stats = core::mem::take(self);
        let mean = |total: u64, count: u32| total / u64::from(count.max(1));
        format!(
            "gallery: {name}: {} samples decoded, {} of them flawed, {} more made no picture, the decoder taken {} us for each ({} at most); {} pictures drawn in {} us each ({} at most), the process busy {} us meanwhile; the screen showed {}, found none ready {} times and passed over {}",
            stats.sample_count,
            stats.flawed_count,
            stats.skipped_count,
            mean(stats.decoding_us, stats.sample_count),
            stats.slowest_decode_us,
            stats.drawn_count,
            mean(stats.drawing_ns / NANOS_PER_MICRO, stats.drawn_count),
            stats.slowest_draw_ns / NANOS_PER_MICRO,
            mean(stats.busy_ns / NANOS_PER_MICRO, stats.drawn_count),
            clock.shown_count,
            clock.starved_count,
            clock.skipped_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::mp4::Sample;

    #[test]
    fn shown_line() {
        let sample = Sample { at: 0, bytes: 1, time: 0, is_key: true };
        let mut movie = Movie::default();
        (movie.quarters, movie.ticks_per_second, movie.samples) = (1, 90_000, alloc::vec![sample; 2]);
        assert_eq!(
            to_shown_line("a.mp4", (1280, 720), &movie, (270, 480), Pose::RightUp),
            "gallery: a.mp4: 1280x720 pictures that the file turns by 1 quarters, 2 samples of 0/90000 s, shown at 270x480, held RightUp"
        );
    }

    #[test]
    fn log_line_and_reset() {
        let mut stats = PlayStats::default();
        stats.add_decoded(false, 7000);
        stats.add_decoded(true, 9000);
        stats.add_skipped();
        stats.add_decoded(false, 0);
        stats.add_drawn(2_000_500, 4_000_000);
        stats.add_drawn(3_000_900, 2_000_000);
        let clock = ClockReport { shown_count: 2, starved_count: 1, skipped_count: 0 };
        assert_eq!(
            stats.take_log_line("a.mp4", clock),
            "gallery: a.mp4: 3 samples decoded, 1 of them flawed, 1 more made no picture, the decoder taken 5333 us for each (9000 at most); 2 pictures drawn in 2500 us each (3000 at most), the process busy 3000 us meanwhile; the screen showed 2, found none ready 1 times and passed over 0",
            "a sample the decoder made no picture of is none of the decoded"
        );
        assert!(
            stats
                .take_log_line("a.mp4", ClockReport::default())
                .contains("0 samples decoded, 0 of them flawed, 0 more made no picture, the decoder taken 0 us"),
            "told once"
        );
    }
}
