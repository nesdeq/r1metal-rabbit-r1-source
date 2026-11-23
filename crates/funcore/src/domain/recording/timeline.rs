use crate::base::time::MICROS_PER_SECOND;

/// Places a recording's frames on a constant-rate grid.
pub struct Timeline {
    rate: u64,
    first_us: Option<u64>,
    places: u64,
}

impl Timeline {
    /// A grid of `rate` places a second.
    pub const fn new(rate: u16) -> Timeline {
        Timeline { rate: rate as u64, first_us: None, places: 0 }
    }

    const fn place(&self, first_us: u64, time_us: u64) -> u64 {
        time_us.saturating_sub(first_us).saturating_mul(self.rate).saturating_add(MICROS_PER_SECOND / 2)
            / MICROS_PER_SECOND
    }

    /// Places the current picture fills before a frame at `time_us`; the first frame begins the grid.
    pub fn fill_before(&mut self, time_us: u64) -> u64 {
        let Some(first_us) = self.first_us else {
            self.first_us = Some(time_us);
            return 0;
        };
        let fills = self.place(first_us, time_us).saturating_sub(self.places);
        self.places += fills;
        fills
    }

    /// Places the current picture fills up to the end at `time_us`: at least one, none without a frame.
    pub fn fill_until(&mut self, time_us: u64) -> u64 {
        let Some(first_us) = self.first_us else {
            return 0;
        };
        let fills = self.place(first_us, time_us).max(self.places + 1) - self.places;
        self.places += fills;
        fills
    }

    /// The place the current picture fills where the end never came: one, none without a frame.
    pub const fn fill_last(&mut self) -> u64 {
        if self.first_us.is_none() {
            return 0;
        }
        self.places += 1;
        1
    }

    /// Places filled so far.
    pub const fn places(&self) -> u64 {
        self.places
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_on_constant_rate_grid() {
        const PERIOD_US: u64 = 16_667;
        let mut timeline = Timeline::new(60);
        assert_eq!(timeline.fill_until(5_000_000), 0, "no frame, no video");
        assert_eq!(timeline.fill_before(1_000_000), 0, "the first frame begins the grid");
        assert_eq!(
            timeline.fill_before(1_000_000 + PERIOD_US + 900),
            1,
            "a frame late by a little takes the next place"
        );
        assert_eq!(timeline.fill_before(1_000_000 + 2 * PERIOD_US - 700), 1, "and one early by a little too");
        assert_eq!(timeline.fill_before(1_000_000 + 2 * PERIOD_US + 300), 0, "a frame in the same place takes it over");
        assert_eq!(
            timeline.fill_before(1_000_000 + 60 * PERIOD_US),
            58,
            "a picture that stands is repeated until the next"
        );
        assert_eq!(timeline.fill_until(1_000_000 + 60 * PERIOD_US + 100), 1, "the last frame is seen at least once");
        assert_eq!(timeline.places(), 61);
        let mut timeline = Timeline::new(60);
        assert_eq!(timeline.fill_last(), 0, "a recording cut short before its first frame has no video");
        timeline.fill_before(0);
        assert_eq!(timeline.fill_until(2_000_000), 120, "two seconds of a picture that stands");
        let mut timeline = Timeline::new(60);
        timeline.fill_before(0);
        timeline.fill_before(10 * PERIOD_US);
        assert_eq!(
            (timeline.fill_last(), timeline.places()),
            (1, 11),
            "one whose end never came shows its last frame once"
        );
        let mut wild = Timeline::new(60);
        wild.fill_before(0);
        assert_eq!(
            wild.fill_before(u64::MAX),
            u64::MAX / MICROS_PER_SECOND,
            "a time no recording reaches is the last place the grid counts, and no sum runs over"
        );
    }
}
