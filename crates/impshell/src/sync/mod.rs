mod board;
mod fetcher;

pub use board::Board;
pub use fetcher::Fetcher;

use crate::sys::lock;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Sleeps on a condition and retakes the lock.
pub fn wait<'a, T>(condition: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condition.wait(guard).unwrap_or_else(|e| e.into_inner())
}

/// Sleeps on a condition while `should_wait` holds, and retakes the lock.
pub fn wait_while<'a, T>(
    condition: &Condvar,
    mut guard: MutexGuard<'a, T>,
    mut should_wait: impl FnMut(&mut T) -> bool,
) -> MutexGuard<'a, T> {
    while should_wait(&mut guard) {
        guard = wait(condition, guard);
    }
    guard
}

// Sleeps on a condition while `should_wait` holds, for `limit` at most, and retakes the lock; false if it still holds.
fn wait_while_within<'a, T>(
    condition: &Condvar,
    guard: MutexGuard<'a, T>,
    limit: Duration,
    should_wait: impl FnMut(&mut T) -> bool,
) -> (MutexGuard<'a, T>, bool) {
    let (guard, result) = condition.wait_timeout_while(guard, limit, should_wait).unwrap_or_else(|e| e.into_inner());
    (guard, !result.timed_out())
}

/// Sleeps on a condition until `find` returns a value.
pub fn wait_for<T, R>(condition: &Condvar, mutex: &Mutex<T>, mut find: impl FnMut(&mut T) -> Option<R>) -> R {
    let mut guard = lock(mutex);
    loop {
        if let Some(value) = find(&mut guard) {
            return value;
        }
        guard = wait(condition, guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn lock_after_panic() {
        let shared = Arc::new((Mutex::new(1), Condvar::new()));
        let other = Arc::clone(&shared);
        let panicking = std::thread::spawn(move || {
            let _guard = lock(&other.0);
            panic!("under the lock");
        });
        assert!(panicking.join().is_err());
        *lock(&shared.0) += 1;
        let waiter = Arc::clone(&shared);
        let waiting = std::thread::spawn(move || {
            let mut value = lock(&waiter.0);
            while *value < 3 {
                value = wait(&waiter.1, value);
            }
            *value
        });
        *lock(&shared.0) += 1;
        shared.1.notify_all();
        assert_eq!(waiting.join().unwrap(), 3);
    }

    #[test]
    fn wait_ends_on_condition() {
        let shared = Arc::new((Mutex::new((0, None::<&str>)), Condvar::new()));
        let (first, second) = (Arc::clone(&shared), Arc::clone(&shared));
        let waiting = std::thread::spawn(move || wait_while(&first.1, lock(&first.0), |value| value.0 < 2).0);
        let taking = std::thread::spawn(move || wait_for(&second.1, &second.0, |value| value.1.take()));
        for _ in 0..2 {
            lock(&shared.0).0 += 1;
            shared.1.notify_all();
        }
        assert_eq!(waiting.join().unwrap(), 2, "it returns at the count it waits for, never before");
        lock(&shared.0).1 = Some("found");
        shared.1.notify_all();
        assert_eq!(taking.join().unwrap(), "found", "and what is waited for is taken");
        assert_eq!(lock(&shared.0).1, None, "once");
        assert_eq!(wait_for(&shared.1, &shared.0, |value| Some(value.0)), 2, "what is there already is not waited for");
        let (guard, is_done) =
            wait_while_within(&shared.1, lock(&shared.0), Duration::from_millis(20), |value| value.0 < 3);
        assert!(!is_done && guard.0 == 2, "a wait with a bound gives up at it");
        drop(guard);
        assert!(
            wait_while_within(&shared.1, lock(&shared.0), Duration::ZERO, |value| value.0 < 2).1,
            "and what is there already is no wait at all"
        );
    }
}
