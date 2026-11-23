use super::check;
use r1_funcore::base::time::timeout_ms;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

/// A descriptor to wait on for reading.
pub const fn readable(fd: RawFd) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

fn timeout_until(deadline: Option<Instant>) -> libc::c_int {
    deadline.map_or(-1, |deadline| timeout_ms(deadline.saturating_duration_since(Instant::now())))
}

/// Waits until one of `fds` is ready or `deadline` comes; returns the ready count, resuming after a signal.
pub fn poll(fds: &mut [libc::pollfd], deadline: Option<Instant>) -> io::Result<usize> {
    loop {
        let timeout = timeout_until(deadline);
        match check(unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) }) {
            Ok(ready) => return Ok(ready as usize),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// An eventfd one thread raises to wake another's wait.
pub struct Wake {
    fd: OwnedFd,
}

impl Wake {
    pub fn new() -> io::Result<Self> {
        let fd = check(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) })?;
        Ok(Wake { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Makes the fd readable until the next `clear`.
    pub fn raise(&self) {
        let count = 1u64;
        unsafe { libc::write(self.fd.as_raw_fd(), (&raw const count).cast(), size_of::<u64>()) };
    }

    pub fn clear(&self) {
        let mut count = 0u64;
        unsafe { libc::read(self.fd.as_raw_fd(), (&raw mut count).cast(), size_of::<u64>()) };
    }

    /// True while raised; does not wait.
    pub fn is_raised(&self) -> bool {
        poll(&mut [readable(self.fd.as_raw_fd())], Some(Instant::now())).is_ok_and(|ready| ready == 1)
    }
}

pub struct Epoll {
    fd: OwnedFd,
    ready: Vec<libc::epoll_event>,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        let fd = check(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })?;
        Ok(Epoll { fd: unsafe { OwnedFd::from_raw_fd(fd) }, ready: vec![libc::epoll_event { events: 0, u64: 0 }] })
    }

    /// An epoll on `fd` alone, as token 7, for tests.
    #[cfg(test)]
    pub fn with_fd(fd: RawFd) -> Epoll {
        let mut epoll = Epoll::new().unwrap();
        epoll.add(fd, 7).unwrap();
        epoll
    }

    fn control(&self, op: libc::c_int, fd: RawFd, token: u64, interest: libc::c_int) -> io::Result<()> {
        let mut event = libc::epoll_event { events: interest as u32, u64: token };
        check(unsafe { libc::epoll_ctl(self.fd.as_raw_fd(), op, fd, &mut event) })?;
        Ok(())
    }

    fn watch(&mut self, fd: RawFd, token: u64, interest: libc::c_int) -> io::Result<()> {
        self.control(libc::EPOLL_CTL_ADD, fd, token, interest)?;
        self.ready.push(libc::epoll_event { events: 0, u64: 0 });
        Ok(())
    }

    /// Watches `fd` for reading and reports it as `token`.
    pub fn add(&mut self, fd: RawFd, token: u64) -> io::Result<()> {
        self.watch(fd, token, libc::EPOLLIN)
    }

    /// `add` for an alarm's `fd`; EPOLLWAKEUP holds a suspend off from its expiry until the next wait.
    pub fn add_wakeup(&mut self, fd: RawFd, token: u64) -> io::Result<()> {
        self.watch(fd, token, libc::EPOLLIN | libc::EPOLLWAKEUP)
    }

    pub fn set_writable(&self, fd: RawFd, token: u64, is_writable: bool) -> io::Result<()> {
        self.control(libc::EPOLL_CTL_MOD, fd, token, libc::EPOLLIN | if is_writable { libc::EPOLLOUT } else { 0 })
    }

    pub fn remove(&mut self, fd: RawFd) -> io::Result<()> {
        self.control(libc::EPOLL_CTL_DEL, fd, 0, 0)?;
        self.ready.pop();
        Ok(())
    }

    /// The ready tokens, waiting at most `limit` for the first.
    pub fn wait(&mut self, limit: Option<Duration>) -> io::Result<Vec<u64>> {
        let mut tokens = Vec::new();
        self.wait_into(limit, &mut tokens)?;
        Ok(tokens)
    }

    /// `wait` into a reused list, which takes no memory; a wait a signal cut short goes on for what is left of `limit`.
    pub fn wait_into(&mut self, limit: Option<Duration>, tokens: &mut Vec<u64>) -> io::Result<()> {
        tokens.clear();
        let deadline = limit.and_then(|limit| Instant::now().checked_add(limit));
        loop {
            let (capacity, timeout) = (self.ready.len() as libc::c_int, timeout_until(deadline));
            match check(unsafe { libc::epoll_wait(self.fd.as_raw_fd(), self.ready.as_mut_ptr(), capacity, timeout) }) {
                Ok(count) => {
                    tokens.extend(self.ready[..count as usize].iter().map(|event| event.u64));
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timeout() {
        let (limit, start) = (Duration::from_micros(20_500), Instant::now());
        assert_eq!(Epoll::new().unwrap().wait(Some(limit)).unwrap(), Vec::<u64>::new());
        assert!(start.elapsed() >= limit, "a limit that is no whole millisecond is rounded up");
    }

    #[test]
    fn poll_counts_ready() {
        let (wake, other) = (Wake::new().unwrap(), Wake::new().unwrap());
        let mut fds = [readable(wake.fd()), readable(other.fd())];
        assert_eq!(poll(&mut fds, Some(Instant::now())).unwrap(), 0, "a deadline that has come looks once");
        let start = Instant::now();
        assert_eq!(poll(&mut fds, Some(start + Duration::from_millis(20))).unwrap(), 0);
        assert!(start.elapsed() >= Duration::from_millis(20), "and none before it comes");
        other.raise();
        assert_eq!(poll(&mut fds, None).unwrap(), 1, "without a deadline until one is ready");
        assert!(fds[0].revents == 0 && fds[1].revents != 0, "and which one is told");
    }

    #[test]
    fn wake_readable_until_clear() {
        let wake = Wake::new().unwrap();
        let mut epoll = Epoll::with_fd(wake.fd());
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new());
        assert!(!wake.is_raised());
        wake.raise();
        wake.raise();
        assert!(wake.is_raised(), "a look tells it without taking it down");
        assert_eq!(epoll.wait(None).unwrap(), vec![7]);
        let mut tokens = vec![1, 2, 3];
        let start_count = crate::alloc_count::allocations();
        epoll.wait_into(None, &mut tokens).unwrap();
        assert_eq!(
            (tokens.as_slice(), crate::alloc_count::allocations()),
            (&[7][..], start_count),
            "into a list that is kept"
        );
        wake.clear();
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new());
        epoll.wait_into(Some(Duration::ZERO), &mut tokens).unwrap();
        assert!(tokens.is_empty(), "nothing ready empties the list");
        wake.clear();
    }
}
