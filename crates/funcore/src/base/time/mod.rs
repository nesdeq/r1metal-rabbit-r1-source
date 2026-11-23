//! Calendar time from seconds since 1970.

pub mod offset;
pub mod setting;
pub mod stamp;

use crate::base::text::to_capitalized;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;
use core::time::Duration;

pub const NANOS_PER_SECOND: u64 = 1_000_000_000;
pub const NANOS_PER_MS: u64 = 1_000_000;
pub const NANOS_PER_MICRO: u64 = 1_000;
pub(crate) const MICROS_PER_SECOND: u64 = 1_000_000;
pub(crate) const MILLIS_PER_SECOND: u64 = 1000;
pub(crate) const SECONDS_PER_MINUTE: u64 = 60;
pub(crate) const MINUTES_PER_HOUR: u64 = 60;
const HOURS_PER_DAY: u64 = 24;
pub(crate) const SECONDS_PER_HOUR: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;
pub const SECONDS_PER_DAY: u64 = SECONDS_PER_HOUR * HOURS_PER_DAY;
const DAYS_PER_WEEK: u64 = 7;
const FIRST_WEEKDAY: u64 = 3;
const DAYS_TO_1970: u64 = 719_468;
const YEARS_PER_ERA: u64 = 400;
const DAYS_PER_ERA: u64 = 146_097;
const DAYS_PER_CENTURY: u64 = 36_524;
const DAYS_PER_FOUR_YEARS: u64 = 1_460;
const DAYS_PER_YEAR: u64 = 365;
const DAYS_PER_FIVE_MONTHS: u64 = 153;
const MONTHS_PER_YEAR: u64 = 12;
const MARCH: u64 = 3;

const WEEKDAYS: [&str; DAYS_PER_WEEK as usize] =
    ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
const MONTHS: [&str; MONTHS_PER_YEAR as usize] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const MONTH_DAYS: [u8; MONTHS_PER_YEAR as usize] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const FEBRUARY: u8 = 2;
const FIRST_YEAR: u32 = 1970;

/// A calendar date and time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Moment {
    pub year: u32,
    /// 1 for January.
    pub month: u8,
    pub day: u8,
    pub(crate) weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

const fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(YEARS_PER_ERA as u32))
}

const fn days_in_month(year: u32, month: u8) -> u8 {
    let days = MONTH_DAYS[(month as usize + MONTH_DAYS.len() - 1) % MONTH_DAYS.len()];
    if month == FEBRUARY && is_leap(year) { days + 1 } else { days }
}

const fn day_of_year_from_march(month: u8, day: u8) -> u64 {
    let months_since_march = (month as u64 + MONTHS_PER_YEAR - MARCH) % MONTHS_PER_YEAR;
    (DAYS_PER_FIVE_MONTHS * months_since_march + 2) / 5 + day as u64 - 1
}

impl Moment {
    /// The moment `seconds` after the start of 1970 (days to date after Howard Hinnant).
    pub const fn from_seconds(seconds: u64) -> Moment {
        let (whole_days, second_of_day) = (seconds / SECONDS_PER_DAY, seconds % SECONDS_PER_DAY);
        let from_era_zero = whole_days + DAYS_TO_1970;
        let day_of_era = from_era_zero % DAYS_PER_ERA;
        let leap_days =
            day_of_era / DAYS_PER_FOUR_YEARS - day_of_era / DAYS_PER_CENTURY + day_of_era / (DAYS_PER_ERA - 1);
        let year_of_era = (day_of_era - leap_days) / DAYS_PER_YEAR;
        let day_of_year = day_of_era - (DAYS_PER_YEAR * year_of_era + year_of_era / 4 - year_of_era / 100);
        let months_since_march = (5 * day_of_year + 2) / DAYS_PER_FIVE_MONTHS;
        let month = (months_since_march + MARCH - 1) % MONTHS_PER_YEAR + 1;
        let year_of_month = year_of_era + from_era_zero / DAYS_PER_ERA * YEARS_PER_ERA + (month < MARCH) as u64;
        Moment {
            year: year_of_month as u32,
            month: month as u8,
            day: (day_of_year - (DAYS_PER_FIVE_MONTHS * months_since_march + 2) / 5 + 1) as u8,
            weekday: ((whole_days + FIRST_WEEKDAY) % DAYS_PER_WEEK) as u8,
            hour: (second_of_day / SECONDS_PER_HOUR) as u8,
            minute: (second_of_day / SECONDS_PER_MINUTE % MINUTES_PER_HOUR) as u8,
            second: (second_of_day % SECONDS_PER_MINUTE) as u8,
        }
    }

    const fn midnight(year: u32, month: u8, day: u8) -> Moment {
        Moment { year, month, day, weekday: 0, hour: 0, minute: 0, second: 0 }
    }

    pub(crate) const fn to_seconds(self) -> u64 {
        let first = if self.year < FIRST_YEAR { FIRST_YEAR } else { self.year };
        let march_year = first as u64 - (self.month < MARCH as u8) as u64;
        let year_of_era = march_year % YEARS_PER_ERA;
        let day_of_era = year_of_era * DAYS_PER_YEAR + year_of_era / 4 - year_of_era / 100
            + day_of_year_from_march(self.month, self.day);
        let whole_days = (march_year / YEARS_PER_ERA * DAYS_PER_ERA + day_of_era).saturating_sub(DAYS_TO_1970);
        whole_days * SECONDS_PER_DAY
            + self.hour as u64 * SECONDS_PER_HOUR
            + self.minute as u64 * SECONDS_PER_MINUTE
            + self.second as u64
    }

    pub(crate) const fn weekday_name(&self) -> &'static str {
        WEEKDAYS[self.weekday as usize % WEEKDAYS.len()]
    }

    pub(crate) const fn month_name(&self) -> &'static str {
        MONTHS[(self.month as usize + MONTHS.len() - 1) % MONTHS.len()]
    }

    /// Date as `yyyymmdd`.
    pub fn to_compact_date(&self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }

    /// Date as `yyyy-mm-dd`.
    pub fn to_iso_date(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Time of day as `hh:mm`.
    pub fn to_time_text(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    // Date and time of day as `yyyy-mm-dd hh:mm`.
    pub(crate) fn to_iso_minute(self) -> String {
        format!("{} {}", self.to_iso_date(), self.to_time_text())
    }

    // Date in words as `16 october 2026`, behind its weekday where asked, each name with a capital where asked.
    pub(crate) fn to_date_words(self, has_weekday: bool, has_capitals: bool) -> String {
        let name = |name: &str| if has_capitals { to_capitalized(name) } else { String::from(name) };
        let weekday = if has_weekday { name(self.weekday_name()) + " " } else { String::new() };
        format!("{weekday}{} {} {}", self.day, name(self.month_name()), self.year)
    }

    fn with_date(self, year: u32, month: u8) -> u64 {
        Moment { year, month, day: self.day.min(days_in_month(year, month)), ..self }.to_seconds()
    }
}

pub(crate) const fn next_of_day(seconds: u64, second_of_day: u64) -> u64 {
    let today = seconds - seconds % SECONDS_PER_DAY + second_of_day;
    if today > seconds { today } else { today + SECONDS_PER_DAY }
}

pub(crate) fn parse_minute_of_day(text: &str) -> Option<u16> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let (hours, minutes): (u64, u64) = (hours.trim().parse().ok()?, minutes.trim().parse().ok()?);
    if hours >= HOURS_PER_DAY || minutes >= MINUTES_PER_HOUR {
        return None;
    }
    Some((hours * MINUTES_PER_HOUR + minutes) as u16)
}

pub(crate) const fn seconds_to_next_minute(seconds: u64) -> u64 {
    SECONDS_PER_MINUTE - seconds % SECONDS_PER_MINUTE
}

/// The whole days since the start of 1970.
pub const fn day_number(seconds: u64) -> u64 {
    seconds / SECONDS_PER_DAY
}

pub(crate) fn write_duration(into: &mut String, seconds: u64) {
    let (minutes, second) = (seconds / SECONDS_PER_MINUTE, seconds % SECONDS_PER_MINUTE);
    let hours = minutes / MINUTES_PER_HOUR;
    let _ = if hours > 0 {
        write!(into, "{hours}:{:02}:{second:02}", minutes % MINUTES_PER_HOUR)
    } else {
        write!(into, "{minutes}:{second:02}")
    };
}

pub(crate) fn to_duration_text(seconds: u64) -> String {
    let mut text = String::new();
    write_duration(&mut text, seconds);
    text
}

/// Nanoseconds suspended between two readings of (the monotonic clock, the boot clock).
pub const fn suspended_ns(before: (u64, u64), after: (u64, u64)) -> u64 {
    let awake = after.0.saturating_sub(before.0);
    after.1.saturating_sub(before.1).saturating_sub(awake)
}

/// A span as a kernel millisecond timeout, rounded up and capped.
pub fn timeout_ms(span: Duration) -> i32 {
    let milliseconds = span.as_nanos().div_ceil(u128::from(NANOS_PER_MS));
    i32::try_from(milliseconds).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(year: u32, month: u8, day: u8, weekday: u8, hour: u8, minute: u8, second: u8) -> Moment {
        Moment { year, month, day, weekday, hour, minute, second }
    }

    #[test]
    fn from_seconds_calendar() {
        for (seconds, expected) in [
            (0, moment(1970, 1, 1, 3, 0, 0, 0)),
            (1_747_392_631, moment(2025, 5, 16, 4, 10, 50, 31)),
            (951_782_400, moment(2000, 2, 29, 1, 0, 0, 0)),
            (951_868_800, moment(2000, 3, 1, 2, 0, 0, 0)),
            (1_709_251_199, moment(2024, 2, 29, 3, 23, 59, 59)),
            (1_709_251_200, moment(2024, 3, 1, 4, 0, 0, 0)),
            (4_107_542_399, moment(2100, 2, 28, 6, 23, 59, 59)),
            (4_107_542_400, moment(2100, 3, 1, 0, 0, 0, 0)),
            (1_767_225_599, moment(2025, 12, 31, 2, 23, 59, 59)),
            (1_767_225_600, moment(2026, 1, 1, 3, 0, 0, 0)),
        ] {
            assert_eq!(Moment::from_seconds(seconds), expected, "{seconds}");
        }
        assert_eq!(Moment::from_seconds(0).weekday_name(), "thursday");
    }

    #[test]
    fn date_words() {
        let moment = Moment::from_seconds(1_792_159_320);
        assert_eq!(
            [(true, true), (true, false), (false, true), (false, false)]
                .map(|(has_weekday, has_capitals)| moment.to_date_words(has_weekday, has_capitals)),
            ["Friday 16 October 2026", "friday 16 october 2026", "16 October 2026", "16 october 2026"]
        );
        assert_eq!(moment.to_iso_minute(), "2026-10-16 14:02");
    }

    #[test]
    fn duration_text() {
        assert_eq!(
            [0, 42, 60, 3599, 3600, 37_230].map(to_duration_text),
            ["0:00", "0:42", "1:00", "59:59", "1:00:00", "10:20:30"]
        );
        let mut text = String::from("RECORDING ");
        write_duration(&mut text, 7);
        assert_eq!(text, "RECORDING 0:07");
    }

    #[test]
    fn to_seconds_round_trip() {
        for seconds in [0, 59, 951_782_400, 951_868_799, 1_709_251_199, 1_747_392_631, 1_767_225_600, 4_107_542_400] {
            assert_eq!(Moment::from_seconds(seconds).to_seconds(), seconds);
        }
        let mut before = Moment::from_seconds(0);
        for day in 1..48_000u64 {
            let seconds = day * SECONDS_PER_DAY + 45_296;
            let now = Moment::from_seconds(seconds);
            assert_eq!(now.to_seconds(), seconds, "day {day}: {now:?}");
            let next = if before.day < days_in_month(before.year, before.month) {
                (before.year, before.month, before.day + 1)
            } else if before.month < 12 {
                (before.year, before.month + 1, 1)
            } else {
                (before.year + 1, 1, 1)
            };
            assert_eq!((now.year, now.month, now.day), next, "the day after {before:?}");
            before = now;
        }
        assert_eq!(
            [(2024, 2), (2025, 2), (2000, 2), (2100, 2), (2026, 4), (2026, 12)]
                .map(|(year, month)| days_in_month(year, month)),
            [29, 28, 29, 28, 30, 31]
        );
        assert_eq!(moment(1969, 12, 31, 0, 23, 0, 0).to_seconds(), moment(1970, 12, 31, 0, 23, 0, 0).to_seconds());
        assert_eq!(
            (Moment::from_seconds(1_747_392_631).month_name(), Moment::from_seconds(0).month_name()),
            ("may", "january")
        );
    }

    #[test]
    fn next_of_day_today_or_tomorrow() {
        let at =
            |day: u8, hour: u8, minute: u8, second: u8| moment(2026, 10, day, 0, hour, minute, second).to_seconds();
        let eight = 8 * SECONDS_PER_HOUR;
        assert_eq!(next_of_day(at(13, 7, 59, 59), eight), at(13, 8, 0, 0));
        assert_eq!(next_of_day(at(13, 0, 0, 0), eight), at(13, 8, 0, 0));
        assert_eq!(next_of_day(at(13, 8, 0, 0), eight), at(14, 8, 0, 0));
        assert_eq!(next_of_day(at(13, 23, 59, 59), eight), at(14, 8, 0, 0));
        assert_eq!(next_of_day(at(31, 12, 0, 0), 0), moment(2026, 11, 1, 0, 0, 0, 0).to_seconds());
        assert_eq!(next_of_day(0, eight), 8 * 3600);
        assert_eq!(
            [next_of_day(at(13, 7, 29, 59), 27_000), next_of_day(at(13, 7, 30, 0), 27_000)],
            [at(13, 7, 30, 0), at(14, 7, 30, 0)]
        );
    }

    #[test]
    fn parses_minute_of_day() {
        assert_eq!(
            ["07:30", "7:30", " 23:59 ", "0:00", "00:5"].map(parse_minute_of_day),
            [Some(450), Some(450), Some(1439), Some(0), Some(5)]
        );
        assert_eq!(
            ["24:00", "12:60", "7.30", "730", "", "a:b", "-1:00", "7:30:00"].map(parse_minute_of_day),
            [None; 8]
        );
    }

    #[test]
    fn suspended_time_and_timeout() {
        let second = NANOS_PER_SECOND;
        assert_eq!(suspended_ns((10 * second, 12 * second), (11 * second, 5413 * second)), 5400 * second);
        assert_eq!(suspended_ns((10, 12), (15, 17)), 0);
        assert_eq!(suspended_ns((10, 12), (15, 16)), 0);
        assert_eq!(
            [Duration::ZERO, Duration::from_micros(20_500), Duration::from_millis(3), Duration::MAX].map(timeout_ms),
            [0, 21, 3, i32::MAX]
        );
    }

    #[test]
    fn next_minute_within_a_minute() {
        assert_eq!(seconds_to_next_minute(0), 60);
        assert_eq!(seconds_to_next_minute(61), 59);
        assert_eq!(seconds_to_next_minute(119), 1);
    }

    #[test]
    fn day_number_counts_whole_days() {
        assert_eq!(
            [day_number(0), day_number(SECONDS_PER_DAY - 1), day_number(SECONDS_PER_DAY), day_number(1_791_697_759)],
            [0, 0, 1, 20_737]
        );
    }
}
