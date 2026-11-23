//! Frame timing: a frame's line, span statistics and missed refreshes.

use crate::base::text::to_tenths;
use crate::base::time::{NANOS_PER_MICRO, NANOS_PER_MS};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const TENTHS: u64 = 10;
const PACING_ON_LINE: &str = "pacing test on";

/// Writes a frame's line into `line`: number, screen time, draw and present times in microseconds.
pub fn frame_line(line: &mut String, seq: u64, started_ns: u64, drawn_ns: u64, presented_ns: u64) {
    let us = |ns: u64| ns / NANOS_PER_MICRO;
    line.clear();
    let _ = write!(
        line,
        "frame {seq} t_us={} draw_us={} present_us={}",
        us(presented_ns),
        us(drawn_ns.saturating_sub(started_ns)),
        us(presented_ns.saturating_sub(drawn_ns))
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Summary {
    pub(crate) count: u64,
    pub(crate) min_ns: u64,
    pub(crate) mean_ns: u64,
    pub(crate) max_ns: u64,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Spans {
    count: u64,
    min_ns: u64,
    max_ns: u64,
    sum_ns: u64,
}

impl Spans {
    pub(crate) fn record(&mut self, span_ns: u64) {
        self.min_ns = if self.count == 0 { span_ns } else { self.min_ns.min(span_ns) };
        self.max_ns = self.max_ns.max(span_ns);
        self.sum_ns += span_ns;
        self.count += 1;
    }

    pub(crate) fn summary(&self) -> Option<Summary> {
        (self.count > 0).then(|| Summary {
            count: self.count,
            min_ns: self.min_ns,
            mean_ns: self.sum_ns / self.count,
            max_ns: self.max_ns,
        })
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct FrameStats {
    last_ns: Option<u64>,
    intervals: Spans,
}

impl FrameStats {
    pub(crate) fn record(&mut self, now_ns: u64) {
        if let Some(last) = self.last_ns.replace(now_ns) {
            self.intervals.record(now_ns.saturating_sub(last));
        }
    }

    pub(crate) fn summary(&self) -> Option<Summary> {
        self.intervals.summary()
    }
}

pub(crate) fn to_millis(ns: u64) -> String {
    to_tenths((ns / (NANOS_PER_MS / TENTHS)) as i64)
}

fn to_spread(summary: &Summary) -> String {
    format!("{}/{}/{}", to_millis(summary.min_ns), to_millis(summary.mean_ns), to_millis(summary.max_ns))
}

fn to_span_line(label: &str, summary: Option<Summary>) -> String {
    match summary {
        Some(summary) => format!("{label} ms {}", to_spread(&summary)),
        None => format!("{label} ms: off"),
    }
}

/// The frame pacing test's state and measures.
#[derive(Clone, Copy, Default)]
pub struct Pacing {
    pub(crate) is_on: bool,
    pub(crate) frames: FrameStats,
    pub(crate) draw: Spans,
    pub(crate) present: Spans,
}

impl Pacing {
    fn to_lines(self) -> [String; 3] {
        let frame = match self.frames.summary() {
            Some(summary) => format!("frame ms {} n={}", to_spread(&summary), summary.count),
            None => "frame ms min/avg/max: off".into(),
        };
        [frame, to_span_line("draw", self.draw.summary()), to_span_line("present", self.present.summary())]
    }

    /// The host's answer: the measures, none while it runs.
    pub fn to_answer(&self) -> Vec<String> {
        if self.is_on { Vec::new() } else { Vec::from(self.to_lines()) }
    }

    /// Log lines of a switch: that it runs, or the measures.
    pub fn to_log_lines(&self) -> Vec<String> {
        if self.is_on { Vec::from([String::from(PACING_ON_LINE)]) } else { Vec::from(self.to_lines()) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Misses {
    pub(crate) frames: u64,
    pub(crate) late: u64,
    pub(crate) longest_ns: u64,
    pub(crate) slowest_draw_ns: u64,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Run {
    last_ns: Option<u64>,
    misses: Misses,
}

impl Run {
    pub(crate) fn record(&mut self, draw_ns: u64, presented_ns: u64, period_ns: u64) {
        let misses = &mut self.misses;
        if let Some(last) = self.last_ns.replace(presented_ns) {
            let interval = presented_ns.saturating_sub(last);
            misses.late += u64::from(2 * interval > 3 * period_ns);
            misses.longest_ns = misses.longest_ns.max(interval);
        }
        misses.frames += 1;
        misses.slowest_draw_ns = misses.slowest_draw_ns.max(draw_ns);
    }

    pub(crate) fn note_repeat(&mut self, draw_ns: u64, at_ns: u64, period_ns: u64) {
        if self.last_ns.is_some() {
            self.record(draw_ns, at_ns, period_ns);
        }
    }

    pub(crate) fn end(&mut self) -> Option<Misses> {
        let misses = core::mem::take(self).misses;
        (misses.late > 0).then_some(misses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_line_allocates_nothing() {
        let mut line = String::from("what the frame before left");
        frame_line(&mut line, 7, 1_000_000, 8_500_000, 17_600_000);
        assert_eq!(line, "frame 7 t_us=17600 draw_us=7500 present_us=9100");
        let before = crate::base::alloc_count::allocations();
        frame_line(&mut line, 8, 17_600_000, 20_000_000, 34_300_000);
        assert_eq!(
            (line.as_str(), crate::base::alloc_count::allocations()),
            ("frame 8 t_us=34300 draw_us=2400 present_us=14300", before)
        );
    }

    #[test]
    fn intervals_from_second_frame() {
        let mut stats = FrameStats::default();
        stats.record(0);
        assert_eq!(stats.summary(), None, "one frame, no interval");
        for t in [10, 40, 60] {
            stats.record(t);
        }
        assert_eq!(stats.summary(), Some(Summary { count: 3, min_ns: 10, mean_ns: 20, max_ns: 30 }));
    }

    #[test]
    fn pacing_lines() {
        let off = ["frame ms min/avg/max: off", "draw ms: off", "present ms: off"];
        let unmeasured = Pacing::default();
        assert_eq!(unmeasured.to_answer(), off, "off, never run");
        assert_eq!(unmeasured.to_log_lines(), off);
        let mut measured = Pacing::default();
        for now_ns in [0, 16_600_000, 33_400_000, 50_000_000] {
            measured.frames.record(now_ns);
        }
        measured.draw.record(2_000_000);
        measured.draw.record(4_500_000);
        measured.present.record(9_140_000);
        let lines = ["frame ms 16.6/16.6/16.8 n=3", "draw ms 2.0/3.2/4.5", "present ms 9.1/9.1/9.1"];
        assert_eq!(
            measured.to_answer(),
            lines,
            "off after a run: the least, the mean and the most of each span in tenths of a millisecond, cut off"
        );
        assert_eq!(measured.to_log_lines(), lines);
        let running = Pacing { is_on: true, ..measured };
        assert_eq!(running.to_answer(), Vec::<String>::new(), "while it runs the host is answered nothing");
        assert_eq!(running.to_log_lines(), ["pacing test on"], "and the log is told that it runs");
    }

    #[test]
    fn run_counts_missed_refreshes() {
        let mut run = Run::default();
        assert_eq!(run.end(), None, "no frames, nothing missed");
        for (draw, at) in [(300, 1_000), (400, 2_040), (350, 2_990), (500, 4_480)] {
            run.record(draw, at, 1_000);
        }
        assert_eq!(run.end(), None, "a frame up to half a refresh late still came with its refresh");
        for (draw, at) in [(300, 10_000), (900, 11_000), (350, 12_600), (200, 13_600), (200, 16_600)] {
            run.record(draw, at, 1_000);
        }
        assert_eq!(run.end(), Some(Misses { frames: 5, late: 2, longest_ns: 3_000, slowest_draw_ns: 900 }));
        run.record(100, 50_000, 1_000);
        assert_eq!(run.end(), None, "and the next run begins anew: its first frame follows none");
        run.note_repeat(300, 10_000, 1_000);
        assert_eq!(run.end(), None, "a refresh that showed the frame before begins no run");
        for (draw, at) in [(300, 20_000), (300, 21_000)] {
            run.record(draw, at, 1_000);
        }
        run.note_repeat(300, 22_000, 1_000);
        run.note_repeat(300, 23_000, 1_000);
        run.record(300, 24_000, 1_000);
        assert_eq!(run.end(), None, "refreshes whose frames changed nothing are none that frames missed");
        run.record(300, 30_000, 1_000);
        run.note_repeat(1_500, 32_000, 1_000);
        assert_eq!(
            run.end(),
            Some(Misses { frames: 2, late: 1, longest_ns: 2_000, slowest_draw_ns: 1_500 }),
            "but one drawn too slowly for the refresh after the frame before came late"
        );
    }
}
