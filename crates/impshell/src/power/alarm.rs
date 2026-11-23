use crate::log;
use crate::sys;
use r1_funcore::hw::power::alarm::Reason;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const WAKE_CLOCK: libc::clockid_t = libc::CLOCK_REALTIME_ALARM;
const ABSOLUTE_FLAGS: libc::c_int = libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET;

fn timer(clock: libc::clockid_t) -> io::Result<OwnedFd> {
    let fd = sys::check(unsafe { libc::timerfd_create(clock, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A timer on the wall clock.
pub struct Alarm {
    timer: OwnedFd,
    /// True if it wakes a suspended system; otherwise it is due only while the system runs.
    pub can_wake: bool,
}

impl Alarm {
    /// An alarm for `reason`; without a waking clock in the kernel, one due only while the device runs, and a log line.
    pub fn new(reason: Reason) -> io::Result<Alarm> {
        // A timer that wakes a suspended system needs a clock to wake by, which the kernel tells of by its resolution, and the leave to set it.
        let result =
            sys::check(unsafe { libc::clock_getres(WAKE_CLOCK, std::ptr::null_mut()) }).and_then(|_| timer(WAKE_CLOCK));
        match result {
            Ok(timer) => Ok(Alarm { timer, can_wake: true }),
            Err(e) => {
                log!("alarm: nothing wakes a sleeping device for {}: {e}", reason.name());
                Ok(Alarm { timer: timer(libc::CLOCK_REALTIME)?, can_wake: false })
            }
        }
    }

    /// Readable once its time has come, and when the clocks were set.
    pub fn fd(&self) -> RawFd {
        self.timer.as_raw_fd()
    }

    /// False where the clocks were set while it was armed to a time and neither a read nor a setting to a time took that in since: it is armed all the same, and its owner works its time out anew.
    pub fn arm(&self, at: Option<u64>) -> io::Result<bool> {
        match at {
            Some(utc) => self.set(utc),
            None => self.clear().map(|()| true),
        }
    }

    fn set(&self, utc: u64) -> io::Result<bool> {
        let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let at = libc::itimerspec { it_interval: zero, it_value: libc::timespec { tv_sec: utc as _, tv_nsec: 0 } };
        let result =
            unsafe { libc::timerfd_settime(self.timer.as_raw_fd(), ABSOLUTE_FLAGS, &at, std::ptr::null_mut()) };
        match sys::check(result) {
            Ok(_) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::ECANCELED) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn clear(&self) -> io::Result<()> {
        let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let at = libc::itimerspec { it_interval: zero, it_value: zero };
        sys::check(unsafe { libc::timerfd_settime(self.timer.as_raw_fd(), 0, &at, std::ptr::null_mut()) }).map(drop)
    }

    /// Clears what made it readable, its time come or the clocks set.
    pub fn acknowledge(&self) {
        let mut times = 0u64;
        unsafe { libc::read(self.timer.as_raw_fd(), (&raw mut times).cast(), size_of::<u64>()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use std::time::Duration;

    #[test]
    fn due_once_at_its_time() {
        let alarm = Alarm::new(Reason::DailyNews).unwrap();
        let mut epoll = Epoll::new().unwrap();
        epoll.add_wakeup(alarm.fd(), 7).unwrap();
        let now = sys::realtime_seconds();
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "one that is not set says nothing");
        // A clock that was set in between has it set again, as whoever sets it does.
        let set = |utc: u64| alarm.arm(Some(utc)).unwrap() || alarm.arm(Some(utc)).unwrap();
        assert!(set(now + 3600));
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "an hour ahead it is not due");
        assert!(set(now.saturating_sub(1)));
        assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "a time that has come is due at once");
        alarm.acknowledge();
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "and is heard of once");
        assert!(set(now + 3600), "set anew, it waits again");
        alarm.acknowledge();
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new());
        assert!(alarm.arm(None).unwrap() && set(now.saturating_sub(1)) && alarm.arm(None).unwrap());
        assert_eq!(
            epoll.wait(Some(Duration::from_millis(50))).unwrap(),
            Vec::<u64>::new(),
            "disarmed, it is never due"
        );
    }
}
