//! Offsets from UTC, in minutes.

use super::{MINUTES_PER_HOUR, SECONDS_PER_MINUTE};
use alloc::string::String;
use core::fmt::Write;

pub(crate) const OFFSETS: [i16; 39] = [
    -660, -600, -570, -540, -480, -420, -360, -300, -240, -210, -180, -150, -120, -60, 0, 60, 120, 180, 210, 240, 270,
    300, 330, 345, 360, 390, 420, 480, 525, 540, 570, 600, 630, 660, 720, 765, 780, 825, 840,
];

/// Local seconds on a clock `offset` minutes ahead of UTC.
pub const fn to_local(utc: u64, offset: i16) -> u64 {
    utc.saturating_add_signed(offset as i64 * SECONDS_PER_MINUTE as i64)
}

pub(crate) const fn to_utc(local: u64, offset: i16) -> u64 {
    to_local(local, offset.saturating_neg())
}

pub(crate) fn step(offset: i16, steps: i32) -> i16 {
    let last = OFFSETS.len() as i64 - 1;
    let index = match steps {
        0 => return offset,
        1.. => OFFSETS.partition_point(|listed| *listed <= offset) as i64 - 1 + i64::from(steps),
        _ => OFFSETS.partition_point(|listed| *listed < offset) as i64 + i64::from(steps),
    };
    OFFSETS[index.clamp(0, last) as usize]
}

pub(crate) fn write_text(into: &mut String, offset: i16) {
    let (sign, minutes) = (if offset < 0 { '-' } else { '+' }, u64::from(offset.unsigned_abs()));
    let _ = write!(into, "UTC{sign}{:02}:{:02}", minutes / MINUTES_PER_HOUR, minutes % MINUTES_PER_HOUR);
}

pub(crate) fn to_text(offset: i16) -> String {
    let mut text = String::new();
    write_text(&mut text, offset);
    text
}

// The offset a text names as `write_text` writes it, with or without `UTC` and the minutes, where a time zone uses it.
pub(crate) fn parse_text(text: &str) -> Option<i16> {
    let text = text.trim();
    let text = text.strip_prefix("UTC").unwrap_or(text);
    let (sign, rest) = match (text.strip_prefix('+'), text.strip_prefix('-')) {
        (Some(rest), _) => (1, rest),
        (_, Some(rest)) => (-1, rest),
        _ => return None,
    };
    let (hours, minutes) = rest.split_once(':').unwrap_or((rest, "0"));
    let minutes = minutes.parse::<u8>().ok().filter(|minutes| u64::from(*minutes) < MINUTES_PER_HOUR)?;
    let offset = sign * (i16::from(hours.parse::<u8>().ok()?) * MINUTES_PER_HOUR as i16 + i16::from(minutes));
    OFFSETS.contains(&offset).then_some(offset)
}

pub(crate) fn checked(minutes: i64) -> Option<i16> {
    let (west, east) = (OFFSETS[0], OFFSETS[OFFSETS.len() - 1]);
    (i64::from(west)..=i64::from(east)).contains(&minutes).then_some(minutes as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_and_offset_steps() {
        assert_eq!(
            (to_local(1_747_392_631, 120), to_local(1_747_392_631, -210), to_local(1_747_392_631, 0)),
            (1_747_399_831, 1_747_380_031, 1_747_392_631)
        );
        assert_eq!((to_local(60, -120), to_local(u64::MAX, 840)), (0, u64::MAX));
        assert!(OFFSETS.is_sorted() && OFFSETS.contains(&0) && OFFSETS.iter().all(|offset| offset % 15 == 0));
        assert_eq!((OFFSETS[0], OFFSETS[OFFSETS.len() - 1]), (-660, 840));
        assert_eq!(
            [step(0, 1), step(0, -1), step(0, 2), step(330, 1), step(345, -1), step(120, 0)],
            [60, -60, 120, 345, 330, 120]
        );
        assert_eq!([step(840, 1), step(-660, -1), step(0, 100), step(0, -100)], [840, -660, 840, -660]);
        assert_eq!(
            [step(100, 1), step(100, -1), step(-5000, 1), step(5000, -1), step(100, 0)],
            [120, 60, -660, 840, 100]
        );
        assert_eq!(
            [0, 120, -210, 345, -660, 840, 5].map(to_text),
            ["UTC+00:00", "UTC+02:00", "UTC-03:30", "UTC+05:45", "UTC-11:00", "UTC+14:00", "UTC+00:05"]
        );
        assert_eq!(
            [-661, -660, 0, 5, 840, 841, i64::MAX].map(checked),
            [None, Some(-660), Some(0), Some(5), Some(840), None, None]
        );
        assert!(OFFSETS.iter().all(|offset| parse_text(&to_text(*offset)) == Some(*offset)), "as it is written");
        assert_eq!(
            [" UTC+02:00 ", "+2", "-3:30", "+05:45", "-00:00", "+14"].map(parse_text),
            [Some(120), Some(120), Some(-210), Some(345), Some(0), Some(840)]
        );
        assert_eq!(
            ["", "UTC", "02:00", "+02:10", "+15", "-12", "+2:60", "+2:", "+:30", "+300", "+ 2", "+2:-30"]
                .map(parse_text),
            [None; 12],
            "no sign, no time zone's offset, or no number"
        );
    }
}
