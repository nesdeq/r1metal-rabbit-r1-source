//! The time and the date set by hand, within the years the device takes.

use super::offset::{to_local, to_utc};
use super::{MONTHS_PER_YEAR, Moment, SECONDS_PER_DAY, SECONDS_PER_HOUR, SECONDS_PER_MINUTE};
use alloc::format;
use alloc::string::String;
use core::ops::RangeInclusive;

const SETTABLE_YEARS: RangeInclusive<u32> = 2026..=2099;
const FIRST_SETTABLE: u64 = Moment::midnight(*SETTABLE_YEARS.start(), 1, 1).to_seconds();
const LAST_SETTABLE: u64 = Moment::midnight(*SETTABLE_YEARS.end(), 12, 31).to_seconds() + SECONDS_PER_DAY - 1;

pub(crate) const fn is_settable(seconds: u64) -> bool {
    seconds >= FIRST_SETTABLE && seconds <= LAST_SETTABLE
}

/// A settable field of the date and time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Hour,
    Minute,
    Day,
    Month,
    Year,
}

impl Part {
    // The field as the log names it.
    const fn to_word(self) -> &'static str {
        match self {
            Part::Hour => "Hour",
            Part::Minute => "Minute",
            Part::Day => "Day",
            Part::Month => "Month",
            Part::Year => "Year",
        }
    }
}

fn step_month(now: Moment, steps: i64) -> u64 {
    let month_count = i64::from(now.year) * MONTHS_PER_YEAR as i64 + i64::from(now.month) - 1 + steps;
    let year = month_count.div_euclid(MONTHS_PER_YEAR as i64);
    let month = month_count.rem_euclid(MONTHS_PER_YEAR as i64) as u8 + 1;
    if year < i64::from(*SETTABLE_YEARS.start()) {
        FIRST_SETTABLE
    } else if year > i64::from(*SETTABLE_YEARS.end()) {
        LAST_SETTABLE
    } else {
        now.with_date(year as u32, month)
    }
}

fn step_part(seconds: u64, part: Part, steps: i32) -> u64 {
    let moment = Moment::from_seconds(seconds);
    let settable_year = moment.year.clamp(*SETTABLE_YEARS.start(), *SETTABLE_YEARS.end());
    let start = if settable_year == moment.year { seconds } else { moment.with_date(settable_year, moment.month) };
    let (now, steps) = (Moment::from_seconds(start), i64::from(steps));
    let result = match part {
        Part::Hour => start.saturating_add_signed(steps * SECONDS_PER_HOUR as i64),
        Part::Minute => (start - start % SECONDS_PER_MINUTE).saturating_add_signed(steps * SECONDS_PER_MINUTE as i64),
        Part::Day => start.saturating_add_signed(steps * SECONDS_PER_DAY as i64),
        Part::Month => step_month(now, steps),
        Part::Year => {
            let (first, last) = (i64::from(*SETTABLE_YEARS.start()), i64::from(*SETTABLE_YEARS.end()));
            now.with_date((i64::from(now.year) + steps).clamp(first, last) as u32, now.month)
        }
    };
    result.clamp(FIRST_SETTABLE, LAST_SETTABLE)
}

/// Steps a field of a UTC time on a clock `offset` minutes ahead of UTC.
pub fn step_local_part(utc: u64, offset: i16, part: Part, steps: i32) -> u64 {
    to_utc(step_part(to_local(utc, offset), part, steps), offset)
}

/// Log line of a manual clock change.
pub fn to_moved_line(part: Part, steps: i32, utc: u64) -> String {
    format!("clock: {} moved by {steps} by hand, to {utc} s of UTC", part.to_word())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_part_and_bounds() {
        let at = |year: u32, month: u8, day: u8, hour: u8, minute: u8, second: u8| {
            Moment { year, month, day, weekday: 0, hour, minute, second }.to_seconds()
        };
        let friday = at(2026, 10, 9, 21, 58, 31);
        for (part, steps, to) in [
            (Part::Hour, 1, at(2026, 10, 9, 22, 58, 31)),
            (Part::Hour, 3, at(2026, 10, 10, 0, 58, 31)),
            (Part::Minute, 1, at(2026, 10, 9, 21, 59, 0)),
            (Part::Minute, -1, at(2026, 10, 9, 21, 57, 0)),
            (Part::Minute, 0, at(2026, 10, 9, 21, 58, 0)),
            (Part::Day, 23, at(2026, 11, 1, 21, 58, 31)),
            (Part::Month, 3, at(2027, 1, 9, 21, 58, 31)),
            (Part::Month, -9, at(2026, 1, 9, 21, 58, 31)),
        ] {
            assert_eq!(step_part(friday, part, steps), to, "{part:?} {steps}");
        }
        assert_eq!(
            [step_part(at(2027, 1, 31, 8, 0, 0), Part::Month, 1), step_part(at(2028, 1, 31, 8, 0, 0), Part::Month, 1)],
            [at(2027, 2, 28, 8, 0, 0), at(2028, 2, 29, 8, 0, 0)]
        );
        assert_eq!(step_part(at(2028, 2, 29, 8, 0, 0), Part::Year, 1), at(2029, 2, 28, 8, 0, 0));
        assert_eq!(
            [step_part(friday, Part::Year, 1), step_part(friday, Part::Year, -1), step_part(friday, Part::Year, 500)],
            [at(2027, 10, 9, 21, 58, 31), at(2026, 10, 9, 21, 58, 31), at(2099, 10, 9, 21, 58, 31)]
        );
        assert_eq!(step_part(at(2025, 5, 16, 10, 50, 31), Part::Year, -1), at(2026, 5, 16, 10, 50, 31));
        assert_eq!(step_local_part(at(2027, 1, 30, 23, 0, 0), 120, Part::Month, 1), at(2027, 2, 27, 23, 0, 0));
        assert_eq!(step_local_part(at(2026, 10, 10, 2, 0, 20), -210, Part::Minute, 1), at(2026, 10, 10, 2, 1, 0));
        assert_eq!(step_local_part(friday, 0, Part::Hour, -2), at(2026, 10, 9, 19, 58, 31));
        let (first, last) = (at(2026, 1, 1, 0, 0, 0), at(2099, 12, 31, 23, 59, 59));
        assert_eq!(
            [
                step_part(friday, Part::Month, -10),
                step_part(friday, Part::Day, -1000),
                step_part(30, Part::Hour, -1),
                step_part(30, Part::Day, -1),
                step_part(30, Part::Month, -1),
                step_part(90, Part::Minute, -5)
            ],
            [first; 6]
        );
        assert_eq!(
            (step_part(at(2025, 5, 16, 12, 50, 31), Part::Hour, 1), step_part(90, Part::Minute, 5)),
            (at(2026, 5, 16, 13, 50, 31), at(2026, 1, 1, 0, 6, 0))
        );
        assert_eq!(
            [
                step_part(last, Part::Minute, 1),
                step_part(last, Part::Hour, 1),
                step_part(last, Part::Month, 1),
                step_part(last, Part::Day, 1)
            ],
            [last; 4]
        );
        assert_eq!(step_part(at(2150, 3, 1, 8, 0, 0), Part::Minute, 0), at(2099, 3, 1, 8, 0, 0));
        assert!(is_settable(first) && is_settable(last) && !is_settable(first - 1) && !is_settable(last + 1));
    }

    #[test]
    fn moved_line() {
        assert_eq!(to_moved_line(Part::Minute, -2, 90), "clock: Minute moved by -2 by hand, to 90 s of UTC");
        assert_eq!(
            [Part::Hour, Part::Minute, Part::Day, Part::Month, Part::Year].map(Part::to_word),
            ["Hour", "Minute", "Day", "Month", "Year"],
            "each field by its word in the log"
        );
    }
}
