//! The daily edition: its rundowns, hour and alarm.

pub mod document;
pub mod editor;
pub mod feed;
pub mod fetch;
pub mod request;
pub mod rundowns;
pub mod silent;
pub mod state;
pub mod stories;
pub mod topics;

use crate::base::time::offset::{to_local, to_utc};
use crate::base::time::{Moment, SECONDS_PER_HOUR, next_of_day};
use alloc::format;
use alloc::string::String;

const HOUR: u8 = 8;
/// The directory of the last day tried and the pending rundowns.
pub const STATE_DIR: &str = ".r1metal/news";
/// The log line of a device with no partition.
pub const NO_PARTITION_LINE: &str = "news: no data partition to keep an edition on";

/// The daily edition as the owner sets it: the hour of the clock on screen it is made at, none for no daily edition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edition {
    pub hour: Option<u8>,
}

impl Default for Edition {
    fn default() -> Edition {
        Edition { hour: Some(HOUR) }
    }
}

impl Edition {
    /// The local time of the next edition after a local time.
    pub fn next_after(self, local: u64) -> Option<u64> {
        Some(next_of_day(local, u64::from(self.hour?) * SECONDS_PER_HOUR))
    }

    /// The next edition after `utc`, none while there is no daily edition, and the alarm's time: that edition or an earlier look.
    pub fn next_alarm(self, utc: u64, offset: i16, look_s: Option<u64>) -> (Option<u64>, Option<u64>) {
        let due_s = self.next_after(to_local(utc, offset)).map(|local| to_utc(local, offset));
        let alarm_s = [due_s, look_s].into_iter().flatten().min();
        (due_s, alarm_s)
    }
}

/// The log line of the next edition, due at `due_s` of UTC.
pub fn to_due_line(due_s: u64, offset: i16, is_waking: bool) -> String {
    let due = Moment::from_seconds(to_local(due_s, offset));
    format!(
        "news: the next edition is due on {} at {:02}:00 by the clock on screen{}",
        due.to_iso_date(),
        due.hour,
        if is_waking { ", and the alarm wakes the device for it" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_line() {
        let due = Moment { year: 2026, month: 10, day: 13, weekday: 0, hour: 6, minute: 0, second: 0 }.to_seconds();
        assert_eq!(
            to_due_line(due, 120, false),
            "news: the next edition is due on 2026-10-13 at 08:00 by the clock on screen"
        );
        assert_eq!(
            to_due_line(due + 9 * 3600, 120, true),
            "news: the next edition is due on 2026-10-13 at 17:00 by the clock on screen, and the alarm wakes the device for it"
        );
    }

    #[test]
    fn edition_hour() {
        let at = |day: u8, hour: u8, minute: u8| {
            Moment { year: 2026, month: 10, day, weekday: 0, hour, minute, second: 30 }.to_seconds()
        };
        let edition = Edition::default();
        assert_eq!(
            (edition.next_after(at(13, 7, 59)), edition.next_after(at(13, 8, 0))),
            (Some(at(13, 8, 0) - 30), Some(at(14, 8, 0) - 30))
        );
        let evening = Edition { hour: Some(19) };
        assert_eq!(evening.next_after(at(13, 8, 0)), Some(at(13, 19, 0) - 30), "at the hour the owner set");
        assert_eq!(Edition { hour: None }.next_after(at(13, 8, 0)), None, "and none while it is off");
    }

    #[test]
    fn alarm_time() {
        let utc = Moment { year: 2026, month: 10, day: 13, weekday: 0, hour: 9, minute: 15, second: 7 }.to_seconds();
        let edition = Edition::default();
        for offset in [0, 120, -210, 840, -660] {
            let (Some(due), at) = edition.next_alarm(utc, offset, None) else { panic!("an edition is due") };
            let local = Moment::from_seconds(to_local(due, offset));
            assert_eq!((local.hour, local.minute, local.second), (HOUR, 0, 0));
            assert!(due > utc && due <= utc + 24 * 3600);
            assert_eq!(at, Some(due));
        }
        let (Some(due), _) = edition.next_alarm(utc, 0, None) else { panic!("an edition is due") };
        assert_eq!(edition.next_alarm(utc, 0, Some(due - 1)), (Some(due), Some(due - 1)));
        assert_eq!(edition.next_alarm(utc, 0, Some(due + 60)), (Some(due), Some(due)));
        assert_eq!(edition.next_alarm(utc, 0, Some(0)), (Some(due), Some(0)));
        let off = Edition { hour: None };
        assert_eq!(
            (off.next_alarm(utc, 0, None), off.next_alarm(utc, 0, Some(7))),
            ((None, None), (None, Some(7))),
            "with no daily edition the alarm is set for a look alone"
        );
    }
}
