use super::check;
use r1_funcore::base::time::{NANOS_PER_MS, NANOS_PER_SECOND};
use std::io;
use std::time::Duration;

fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(clock, &mut now) };
    now.tv_sec as u64 * NANOS_PER_SECOND + now.tv_nsec as u64
}

/// Time the system has been running, in nanoseconds.
pub fn monotonic_ns() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC)
}

/// Time the system has been running, as a span.
pub fn monotonic() -> Duration {
    Duration::from_nanos(monotonic_ns())
}

/// Time since boot with suspends, in nanoseconds.
pub fn boottime_ns() -> u64 {
    clock_ns(libc::CLOCK_BOOTTIME)
}

/// Time since boot with suspends, in milliseconds.
pub fn boottime_ms() -> u64 {
    boottime_ns() / NANOS_PER_MS
}

/// Time since boot with suspends, in seconds.
pub fn boottime_seconds() -> u64 {
    boottime_ns() / NANOS_PER_SECOND
}

/// Processor time of all this process's threads, in nanoseconds.
pub fn cpu_ns() -> u64 {
    clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID)
}

/// The wall clock, in seconds since the start of 1970.
pub fn realtime_seconds() -> u64 {
    clock_ns(libc::CLOCK_REALTIME) / NANOS_PER_SECOND
}

/// The wall clock, in milliseconds since the start of 1970.
pub fn realtime_ms() -> u64 {
    clock_ns(libc::CLOCK_REALTIME) / NANOS_PER_MS
}

/// Sets the wall clock to `seconds` since the start of 1970.
pub fn set_realtime(seconds: u64) -> io::Result<()> {
    let time = libc::timespec { tv_sec: seconds as _, tv_nsec: 0 };
    check(unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &time) })?;
    Ok(())
}
