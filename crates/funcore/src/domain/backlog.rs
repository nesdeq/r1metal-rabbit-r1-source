//! Work a thread has a service do over the network, and which piece goes next.

use alloc::vec::Vec;
use core::borrow::Borrow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Status<B> {
    New,
    Waiting,
    Busy(B),
    Failed,
}

/// The pieces of work that wait.
pub struct Backlog<K, B> {
    pub(super) pieces: Vec<(K, Status<B>)>,
    is_wanted: bool,
    is_scan_due: bool,
    pub(super) is_stalled: bool,
}

impl<K, B> Default for Backlog<K, B> {
    fn default() -> Self {
        Backlog { pieces: Vec::new(), is_wanted: false, is_scan_due: false, is_stalled: false }
    }
}

/// The next step of the thread that works through a backlog.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step<K> {
    Scan,
    // True where the piece is new.
    Piece(K, bool),
    Release,
    Sleep,
}

impl<K: PartialEq + Clone, B: Copy> Backlog<K, B> {
    /// The thread's next step: a piece it takes counts as `busy`, and `is_holding` tells that it holds the network.
    pub fn next_step(&mut self, busy: B, is_holding: bool) -> Step<K> {
        if self.take_scan() {
            return Step::Scan;
        }
        match self.take_next(busy) {
            Some((key, is_new)) => Step::Piece(key, is_new),
            None if is_holding => Step::Release,
            None => Step::Sleep,
        }
    }

    /// True where no other piece is due, so the network goes with a piece's last requests.
    pub fn is_release_due(&self) -> bool {
        self.due().is_none()
    }

    fn due(&self) -> Option<usize> {
        if self.is_stalled {
            return None;
        }
        let find = |is_match: fn(&Status<B>) -> bool| self.pieces.iter().position(|piece| is_match(&piece.1));
        let new = find(|status| matches!(status, Status::New));
        new.or_else(|| find(|status| matches!(status, Status::Waiting)).filter(|_| self.is_wanted))
    }

    pub(super) fn take_next(&mut self, busy: B) -> Option<(K, bool)> {
        let index = self.due()?;
        let (key, status) = &mut self.pieces[index];
        let was_new = matches!(status, Status::New);
        *status = Status::Busy(busy);
        Some((key.clone(), was_new))
    }

    fn take_scan(&mut self) -> bool {
        core::mem::take(&mut self.is_scan_due)
    }

    fn position<Q: PartialEq + ?Sized>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        self.pieces.iter().position(|piece| piece.0.borrow() == key)
    }

    /// Adds the pieces listed on the partition that are not known yet.
    pub fn add_waiting(&mut self, keys: Vec<K>) {
        for key in keys {
            if self.position(&key).is_none() {
                self.pieces.push((key, Status::Waiting));
            }
        }
    }

    /// Takes a piece just handed over; one that waits moves up, one that is busy stays.
    pub fn add_new(&mut self, key: K) {
        match self.position(&key) {
            None => self.pieces.push((key, Status::New)),
            Some(index) if matches!(self.pieces[index].1, Status::Waiting | Status::Failed) => {
                self.pieces[index].1 = Status::New;
            }
            Some(_) => {}
        }
    }

    /// Drops a deleted piece; one that is busy stays.
    pub fn forget<Q: PartialEq + ?Sized>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
    {
        self.pieces.retain(|(piece, status)| matches!(status, Status::Busy(_)) || piece.borrow() != key);
    }

    /// Counts a piece as `busy` from now.
    pub fn set_busy<Q: PartialEq + ?Sized>(&mut self, key: &Q, busy: B)
    where
        K: Borrow<Q>,
    {
        if let Some(index) = self.position(key) {
            self.pieces[index].1 = Status::Busy(busy);
        }
    }

    pub(super) fn end<Q: PartialEq + ?Sized>(&mut self, key: &Q, is_done: bool, is_stalled: bool)
    where
        K: Borrow<Q>,
    {
        self.is_stalled |= is_stalled;
        let Some(index) = self.position(key) else {
            return;
        };
        if is_done {
            self.pieces.remove(index);
        } else {
            self.pieces[index].1 = Status::Failed;
        }
    }

    /// Notes whether a screen of such work shows; true where one opened, which retries everything.
    pub fn set_wanted(&mut self, is_wanted: bool) -> bool {
        let has_opened = is_wanted && !self.is_wanted;
        self.is_wanted = is_wanted;
        if has_opened {
            self.is_scan_due = true;
            self.is_stalled = false;
            for piece in &mut self.pieces {
                if matches!(piece.1, Status::Failed) {
                    piece.1 = Status::Waiting;
                }
            }
        }
        has_opened
    }

    /// The pieces being worked on, with how far each is.
    pub fn busy(&self) -> impl Iterator<Item = (&K, B)> {
        self.pieces.iter().filter_map(|(key, status)| match status {
            Status::Busy(busy) => Some((key, *busy)),
            _ => None,
        })
    }
}

/// The state that holds a backlog.
pub trait Holder {
    type Key: PartialEq + Clone;
    type Busy: Copy;

    fn backlog(&mut self) -> &mut Backlog<Self::Key, Self::Busy>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn new_goes_before_waiting() {
        let mut backlog = Backlog::<&str, u64>::default();
        assert_eq!(backlog.take_next(1), None);
        backlog.add_waiting(vec!["old", "older"]);
        assert_eq!(backlog.take_next(1), None);
        backlog.add_new("new");
        assert_eq!(backlog.due(), Some(2));
        assert_eq!(backlog.take_next(1), Some(("new", true)));
        assert_eq!((backlog.take_next(1), backlog.busy().collect::<Vec<_>>()), (None, vec![(&"new", 1)]));
        assert!(backlog.set_wanted(true) && !backlog.set_wanted(true) && backlog.take_scan() && !backlog.take_scan());
        backlog.add_waiting(vec!["old", "oldest"]);
        assert_eq!(
            backlog.pieces,
            [
                ("old", Status::Waiting),
                ("older", Status::Waiting),
                ("new", Status::Busy(1)),
                ("oldest", Status::Waiting)
            ]
        );
        backlog.add_new("newer");
        assert_eq!([backlog.take_next(2), backlog.take_next(2)], [Some(("newer", true)), Some(("old", false))]);
        assert!(!backlog.set_wanted(false));
        assert_eq!(backlog.take_next(3), None);
    }

    #[test]
    fn done_and_failed_pieces() {
        let mut backlog = Backlog::<&str, u64>::default();
        backlog.add_waiting(vec!["new"]);
        backlog.add_new("new");
        assert_eq!(backlog.pieces, [("new", Status::New)]);
        assert_eq!(backlog.take_next(1), Some(("new", true)));
        backlog.add_new("new");
        assert_eq!(backlog.pieces, [("new", Status::Busy(1))]);
        assert_eq!(backlog.take_next(1), None);
        backlog.add_new("newer");
        assert_eq!(backlog.take_next(1), Some(("newer", true)));
        backlog.end(&"newer", false, false);
        assert_eq!(backlog.take_next(1), None);
        backlog.add_new("newer");
        assert_eq!(backlog.take_next(1), Some(("newer", true)));
        backlog.end(&"newer", true, false);
        assert_eq!(backlog.pieces, [("new", Status::Busy(1))]);
    }

    #[test]
    fn forget_keeps_busy() {
        let mut backlog = Backlog::<&str, u64>::default();
        backlog.add_waiting(vec!["a", "b"]);
        backlog.add_new("c");
        assert_eq!(backlog.take_next(1), Some(("c", true)));
        backlog.forget(&"a");
        backlog.forget(&"c");
        assert_eq!(backlog.pieces, [("b", Status::Waiting), ("c", Status::Busy(1))]);
        backlog.set_busy(&"c", 2);
        assert_eq!(backlog.busy().collect::<Vec<_>>(), [(&"c", 2)]);
    }

    #[test]
    fn stall_until_screen_opens() {
        let mut backlog = Backlog::<&str, u64>::default();
        backlog.set_wanted(true);
        backlog.add_waiting(vec!["a", "b"]);
        assert_eq!(backlog.take_next(1), Some(("a", false)));
        backlog.end(&"a", false, true);
        assert!(backlog.is_stalled);
        backlog.add_new("c");
        assert_eq!(backlog.take_next(1), None);
        assert!(!backlog.set_wanted(true));
        backlog.set_wanted(false);
        assert!(backlog.set_wanted(true) && !backlog.is_stalled);
        assert_eq!(backlog.pieces, [("a", Status::Waiting), ("b", Status::Waiting), ("c", Status::New)]);
    }

    #[test]
    fn next_step_order() {
        let mut backlog = Backlog::<&str, u64>::default();
        assert_eq!(backlog.next_step(1, false), Step::Sleep);
        assert_eq!(backlog.next_step(1, true), Step::Release);
        assert!(backlog.is_release_due());
        backlog.set_wanted(true);
        assert_eq!(backlog.next_step(1, true), Step::Scan);
        backlog.add_waiting(vec!["a"]);
        assert!(!backlog.is_release_due());
        assert_eq!(backlog.next_step(1, true), Step::Piece("a", false));
    }
}
