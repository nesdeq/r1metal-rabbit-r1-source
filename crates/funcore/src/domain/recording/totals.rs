use crate::base::time::MICROS_PER_SECOND;
use alloc::format;
use alloc::string::String;

/// What one recording has sent, for the log.
#[derive(Default)]
pub struct Totals {
    frames: u64,
    skips: u64,
    bytes: u64,
    start_us: Option<u64>,
}

impl Totals {
    /// A recording that begins at `at_us`.
    pub const fn from_start(at_us: u64) -> Totals {
        Totals { frames: 0, skips: 0, bytes: 0, start_us: Some(at_us) }
    }

    /// Counts a frame sent after `skips` passed over.
    pub fn frame(&mut self, skips: u32) {
        self.frames += 1;
        self.skips += u64::from(skips);
    }

    /// Counts a record of `bytes` bytes.
    pub fn record(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
    }

    /// Log line of a recording's end at `end_us`.
    pub fn to_log_line(&self, end_us: u64) -> String {
        let duration = self.start_us.map_or_else(String::new, |start_us| {
            format!(" in {:.1} s", end_us.saturating_sub(start_us) as f64 / MICROS_PER_SECOND as f64)
        });
        format!(
            "recording: ended, {} frames sent and {} passed over, {} bytes{duration}",
            self.frames, self.skips, self.bytes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_totals() {
        let mut totals = Totals::from_start(1_000_000);
        [0, 1, 0].into_iter().for_each(|skips| totals.frame(skips));
        [1000, 234].into_iter().for_each(|bytes| totals.record(bytes));
        assert_eq!(
            totals.to_log_line(3_500_000),
            "recording: ended, 3 frames sent and 1 passed over, 1234 bytes in 2.5 s"
        );
        assert_eq!(Totals::default().to_log_line(9), "recording: ended, 0 frames sent and 0 passed over, 0 bytes");
    }
}
