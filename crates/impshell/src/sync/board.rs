use super::{wait, wait_for, wait_while_within};
use crate::sys::{Wake, lock, try_lock};
use r1_funcore::base::handoff::Loan;
use std::io;
use std::os::fd::RawFd;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// State a thread and the main loop share under one lock; sleepers wake on a change, and the flag tells the main loop.
pub struct Board<T> {
    state: Mutex<T>,
    changed: Condvar,
    flag: Wake,
}

impl<T> Board<T> {
    pub fn new(state: T) -> io::Result<Board<T>> {
        Ok(Board { state: Mutex::new(state), changed: Condvar::new(), flag: Wake::new()? })
    }

    /// The state under its lock, poisoned or not.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        lock(&self.state)
    }

    /// The state under its lock if free; never waits.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        try_lock(&self.state)
    }

    /// Sleeps until `find` returns a value.
    pub fn wait<R>(&self, find: impl FnMut(&mut T) -> Option<R>) -> R {
        wait_for(&self.changed, &self.state, find)
    }

    /// Reads a value on loan once it is back.
    pub fn read_loan<V, R>(&self, loan: impl Fn(&T) -> &Loan<V>, mut read: impl FnMut(&V) -> R) -> R {
        self.wait(|state| loan(state).get().map(&mut read))
    }

    /// Takes a value on loan once it is back, changes it outside the lock, and gives it back.
    pub fn change_loan<V, R>(&self, loan: impl Fn(&mut T) -> &mut Loan<V>, change: impl FnOnce(&mut V) -> R) -> R {
        let mut value = self.wait(|state| loan(state).take());
        let result = change(&mut value);
        loan(&mut self.lock()).give(value);
        self.notify();
        result
    }

    /// Sleeps until the board changes or `limit` passes.
    pub fn wait_change(&self, limit: Duration) {
        drop(self.sleep_within(self.lock(), limit));
    }

    /// Sleeps until the board changes, and retakes the lock.
    pub fn sleep<'a>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        wait(&self.changed, guard)
    }

    /// `sleep` for `limit` at most.
    pub fn sleep_within<'a>(&self, guard: MutexGuard<'a, T>, limit: Duration) -> MutexGuard<'a, T> {
        self.changed.wait_timeout(guard, limit).unwrap_or_else(|e| e.into_inner()).0
    }

    /// Sleeps while `should_wait` holds, for `limit` at most; false if it still holds.
    pub fn wait_while_within(
        &self,
        guard: MutexGuard<'_, T>,
        limit: Duration,
        should_wait: impl FnMut(&mut T) -> bool,
    ) -> bool {
        wait_while_within(&self.changed, guard, limit, should_wait).1
    }

    /// Wakes the sleepers.
    pub fn notify(&self) {
        self.changed.notify_all();
    }

    /// Raises the flag for the main loop.
    pub fn raise(&self) {
        self.flag.raise();
    }

    /// Clears the flag.
    pub fn clear(&self) {
        self.flag.clear();
    }

    /// Takes what the flag told of off the state: the flag first, so that what is posted meanwhile raises it anew.
    pub fn take<R>(&self, take: impl FnOnce(&mut T) -> R) -> R {
        self.clear();
        take(&mut self.lock())
    }

    /// Readable while the flag is up.
    pub fn fd(&self) -> RawFd {
        self.flag.fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use std::sync::Arc;

    #[test]
    fn notify_wakes_and_flag_tells_loop() {
        let board = Arc::new(Board::new(None::<u32>).unwrap());
        let mut epoll = Epoll::with_fd(board.fd());
        let sleeper = Arc::clone(&board);
        let waiting = std::thread::spawn(move || {
            let value = sleeper.wait(Option::take);
            sleeper.raise();
            value
        });
        *board.lock() = Some(5);
        board.notify();
        assert_eq!(waiting.join().unwrap(), 5, "what the board was given is taken off it");
        assert_eq!(epoll.wait(None).unwrap(), vec![7], "and the loop is told");
        board.clear();
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "until the flag is taken down");
        assert_eq!(*board.lock(), None);
        let guard = board.lock();
        let changer = Arc::clone(&board);
        let changing = std::thread::spawn(move || {
            *changer.lock() = Some(6);
            changer.notify();
        });
        let mut guard = board.sleep(guard);
        while guard.is_none() {
            guard = board.sleep(guard);
        }
        assert_eq!(*guard, Some(6), "the change is made while the lock is let go for the sleep");
        drop(guard);
        changing.join().unwrap();
    }

    #[test]
    fn loan_waits_for_return() {
        let board = Arc::new(Board::new(Loan::<u32>::default()).unwrap());
        let value = board.lock().take().unwrap();
        let reader = Arc::clone(&board);
        let reading = std::thread::spawn(move || reader.read_loan(|loan| loan, |value| *value));
        let changer = Arc::clone(&board);
        let changing = std::thread::spawn(move || changer.change_loan(|loan| loan, |value| *value += 1));
        board.lock().give(value + 7);
        board.notify();
        changing.join().unwrap();
        assert!(matches!(reading.join().unwrap(), 7 | 8), "read once it is back");
        assert_eq!(board.read_loan(|loan| loan, |value| *value), 8);
    }

    #[test]
    fn newest_value_once() {
        let board = Board::new(None).unwrap();
        let mut epoll = Epoll::with_fd(board.fd());
        let post = |value| {
            board.lock().replace(value);
            board.raise();
        };
        let take = || board.take(Option::take);
        assert_eq!(take(), None);
        post("first");
        post("second");
        assert_eq!(epoll.wait(None).unwrap(), vec![7]);
        assert_eq!((take(), take()), (Some("second"), None), "the newest, once");
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "taking clears the flag");
    }
}
