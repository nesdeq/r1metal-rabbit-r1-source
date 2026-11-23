use crate::pool::ThreadPool;
use crate::sys::lock;
use crate::{log, sys};
use r1_funcore::base::time::Moment;
use r1_funcore::base::time::offset::to_local;
use r1_funcore::base::time::setting::{Part, step_local_part};
use r1_funcore::domain::clock::{Wall, ahead_by, to_set_by_line};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::Mutex;

const RTC: &str = "/dev/rtc0";
const RTC_SET_TIME: u32 = sys::ioc(sys::IOC_WRITE, b'p', 0x0a, size_of::<RtcTime>());
const RTC_FIRST_YEAR: u32 = 1900;

// The kernel's `struct rtc_time`.
#[repr(C)]
#[derive(Default, Debug, PartialEq, Eq)]
struct RtcTime {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
}

// The fields the hardware clock takes for a time in seconds of UTC: its months count from 0, its years from 1900.
fn to_rtc_time(seconds: u64) -> RtcTime {
    let now = Moment::from_seconds(seconds);
    RtcTime {
        sec: now.second.into(),
        min: now.minute.into(),
        hour: now.hour.into(),
        mday: now.day.into(),
        mon: i32::from(now.month) - 1,
        year: now.year.saturating_sub(RTC_FIRST_YEAR) as i32,
        ..RtcTime::default()
    }
}

static WALL: Mutex<Wall> = Mutex::new(Wall::new());
static RTC_WRITER: ThreadPool = ThreadPool::new("rtc");

/// The offset of the clock on screen from UTC, in minutes.
pub fn offset() -> i16 {
    lock(&WALL).offset
}

/// Sets the offset every thread tells local time by.
pub fn set_offset(minutes: i16) {
    lock(&WALL).offset = minutes;
}

/// Local time in seconds, by which files are named.
pub fn local() -> u64 {
    to_local(sys::realtime_seconds(), offset())
}

/// Sets the system clock at once and the hardware clock on a thread of its own.
pub fn set(utc: u64) -> io::Result<()> {
    sys::set_realtime(utc)?;
    let is_new = lock(&WALL).ask_rtc();
    if is_new && let Err(e) = RTC_WRITER.run(write_rtc) {
        lock(&WALL).drop_rtc();
        return Err(io::Error::new(e.kind(), format!("no thread for the hardware clock: {e}")));
    }
    Ok(())
}

fn write_rtc() {
    while lock(&WALL).next_rtc_write() {
        let rtc = File::open(RTC);
        let mut time = to_rtc_time(sys::realtime_seconds());
        let result = rtc.and_then(|rtc| unsafe { sys::ioctl(rtc.as_raw_fd(), RTC_SET_TIME, &mut time) });
        if let Err(e) = result {
            log!("clock: the hardware clock is not set: {e}");
        }
    }
}

/// Steps a part of the time at `offset` minutes from UTC and sets the clocks; returns the new time.
pub fn move_by(part: Part, steps: i32, offset: i16) -> io::Result<u64> {
    let target = step_local_part(sys::realtime_seconds(), offset, part, steps);
    set(target).map(|()| target)
}

/// Sets the clocks by a server's time unless they agree; returns the seconds the clock was ahead, if set.
pub fn set_from_server(time: u64, server: &str) -> io::Result<Option<i64>> {
    let Some(seconds) = ahead_by(sys::realtime_seconds(), time) else {
        return Ok(None);
    };
    set(time)?;
    log!("{}", to_set_by_line(server, seconds));
    Ok(Some(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_set_time_number() {
        assert_eq!(RTC_SET_TIME, 0x4024_700a);
    }

    #[test]
    fn rtc_fields() {
        let expected = RtcTime { sec: 31, min: 50, hour: 10, mday: 16, mon: 4, year: 125, ..RtcTime::default() };
        assert_eq!(to_rtc_time(1_747_392_631), expected);
    }

    #[test]
    fn local_time_offset() {
        let previous = offset();
        set_offset(-210);
        let (local_time, utc_time) = (local(), sys::realtime_seconds());
        assert!(offset() == -210 && (utc_time - local_time).abs_diff(210 * 60) <= 1, "{local_time} {utc_time}");
        set_offset(previous);
    }

    #[test]
    fn right_clock_stays() {
        assert_eq!(set_from_server(sys::realtime_seconds(), "the test").ok(), Some(None));
    }
}
