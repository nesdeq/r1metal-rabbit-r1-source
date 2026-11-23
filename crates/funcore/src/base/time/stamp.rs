//! Dates and times written into names, and ISO dates.

use super::{
    FIRST_YEAR, HOURS_PER_DAY, MINUTES_PER_HOUR, MONTHS_PER_YEAR, Moment, SECONDS_PER_HOUR, SECONDS_PER_MINUTE,
    days_in_month,
};
use alloc::format;
use alloc::string::String;

const STAMP_DATE: [(usize, usize); 3] = [(0, 4), (4, 2), (6, 2)];
const STAMP_TIME: [(usize, usize); 3] = [(9, 2), (11, 2), (13, 2)];
const STAMP_SEPARATOR_AT: usize = 8;
const STAMP_SEPARATOR: u8 = b'_';
const ISO_DATE: [(usize, usize); 3] = [(0, 4), (5, 2), (8, 2)];
const ISO_DASHES: [usize; 2] = [4, 7];
const ISO_LENGTH: usize = 10;

/// File name stem of a local time, `yyyymmdd_hhmmss`.
pub fn to_stamp(seconds: u64) -> String {
    let now = Moment::from_seconds(seconds);
    format!("{}_{:02}{:02}{:02}", now.to_compact_date(), now.hour, now.minute, now.second)
}

fn parse_digits(text: &str, (start, digits): (usize, usize)) -> Option<u32> {
    let part = text.get(start..start + digits)?;
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

fn parse_date(text: &str, parts: [(usize, usize); 3]) -> Option<Moment> {
    let (year, month, day) =
        (parse_digits(text, parts[0])?, parse_digits(text, parts[1])? as u8, parse_digits(text, parts[2])? as u8);
    let is_real = year >= FIRST_YEAR
        && (1..=MONTHS_PER_YEAR as u8).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day);
    is_real.then(|| Moment::from_seconds(Moment::midnight(year, month, day).to_seconds()))
}

pub(crate) fn parse_stamp(name: &str) -> Option<Moment> {
    let day = parse_date(name, STAMP_DATE)?;
    if name.as_bytes().get(STAMP_SEPARATOR_AT) != Some(&STAMP_SEPARATOR) {
        return None;
    }
    let [hour, minute, second] = STAMP_TIME.map(|part| parse_digits(name, part).map(u64::from));
    let (hour, minute, second) = (hour?, minute?, second?);
    if hour >= HOURS_PER_DAY || minute >= MINUTES_PER_HOUR || second >= SECONDS_PER_MINUTE {
        return None;
    }
    Some(Moment::from_seconds(day.to_seconds() + hour * SECONDS_PER_HOUR + minute * SECONDS_PER_MINUTE + second))
}

/// Midnight of a `yyyymmdd` name.
pub fn parse_compact_day(name: &str) -> Option<Moment> {
    parse_date(name, STAMP_DATE).filter(|_| name.len() == STAMP_SEPARATOR_AT)
}

/// True for a `yyyymmdd` name of a real date.
pub fn is_compact_day(name: &str) -> bool {
    parse_compact_day(name).is_some()
}

pub(crate) fn parse_iso_day(text: &str) -> Option<Moment> {
    let has_dashes = ISO_DASHES.iter().all(|at| text.as_bytes().get(*at) == Some(&b'-'));
    parse_date(text, ISO_DATE).filter(|_| has_dashes && text.len() == ISO_LENGTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_and_days() {
        assert_eq!(to_stamp(1_747_392_631), "20250516_105031");
        assert_eq!(parse_stamp("20250516_105031"), Some(Moment::from_seconds(1_747_392_631)));
        assert_eq!(
            parse_stamp("20250516_105031_1.m4a").map(|moment| (moment.weekday_name(), moment.second)),
            Some(("friday", 31))
        );
        assert_eq!(parse_stamp("20240229_235959").map(|moment| (moment.month, moment.day)), Some((2, 29)));
        for none in [
            "",
            "note",
            "20250516",
            "20250516-105031",
            "20250516_10503",
            "2025051x_105031",
            "+0250516_105031",
            "20251316_105031",
            "20250229_105031",
            "20250500_105031",
            "20250516_245031",
            "20250516_106031",
            "20250516_105060",
            "19691231_235959",
            "\u{E4}0250516_105031",
        ] {
            assert_eq!(parse_stamp(none), None, "{none:?}");
        }
        assert_eq!(
            parse_compact_day("20261013").map(|day| (day.weekday_name(), day.day, day.hour)),
            Some(("tuesday", 13, 0))
        );
        for none in ["", "2026101", "202610130", "20261313", "20260229", "19691231", "2026101x", "20261013_000000"] {
            assert_eq!(parse_compact_day(none), None, "{none:?}");
        }
        let day_of = |seconds| Moment::from_seconds(seconds).to_compact_date();
        assert_eq!(day_of(1_791_697_759), "20261011");
        assert_eq!((day_of(0), day_of(951_782_400)), ("19700101".into(), "20000229".into()));
        assert!(
            is_compact_day("20261011") && is_compact_day(&day_of(0)) && !is_compact_day("20260230"),
            "a day is a date of the calendar"
        );
        let fact = Moment::from_seconds(1_792_159_320);
        assert_eq!(parse_iso_day(&fact.to_iso_date()).map(|day| (day.day, day.hour)), Some((16, 0)));
        for none in ["", "Anna-Maria", "2026-13-01", "2026-02-29", "20261016", "2026-10-1", "2026-10-160", "2026_10_16"]
        {
            assert_eq!(parse_iso_day(none), None, "{none:?}");
        }
    }
}
