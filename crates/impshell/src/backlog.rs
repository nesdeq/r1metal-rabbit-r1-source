use crate::sync::Board;
use crate::wifi::radio::Hold;
use r1_funcore::domain::backlog::{Holder, Step};

/// A thread that works through a board's backlog, and the hold it keeps across pieces while another is due.
pub struct Worker<'a, S> {
    board: &'a Board<S>,
    hold: Option<Hold>,
}

impl<'a, S: Holder> Worker<'a, S> {
    pub const fn new(board: &'a Board<S>) -> Worker<'a, S> {
        Worker { board, hold: None }
    }

    /// Sleeps until a piece is due and returns it with whether it is new, counted as `busy` says and told to the main loop; lets the hold go before the sleep, and `scan` lists what waits on the partition once a screen has opened.
    pub fn take(&mut self, scan: impl Fn() -> Vec<S::Key>, busy: impl Fn() -> S::Busy) -> (S::Key, bool) {
        let board = self.board;
        let mut state = board.lock();
        loop {
            match state.backlog().next_step(busy(), self.hold.is_some()) {
                Step::Scan => {
                    drop(state);
                    let pieces = scan();
                    state = board.lock();
                    state.backlog().add_waiting(pieces);
                }
                Step::Piece(key, is_new) => {
                    drop(state);
                    board.raise();
                    return (key, is_new);
                }
                Step::Release => {
                    drop(state);
                    self.hold = None;
                    state = board.lock();
                }
                Step::Sleep => state = board.sleep(state),
            }
        }
    }

    /// Runs requests of a piece that more of its own follow, over the hold `ask` keeps.
    pub fn ask<T>(&mut self, ask: impl FnOnce(&mut Option<Hold>) -> T) -> T {
        ask(&mut self.hold)
    }

    /// Runs a piece's last requests over the hold `ask` keeps, and lets the hold go unless another piece is due: the radio is on for the requests alone.
    pub fn ask_last<T>(&mut self, ask: impl FnOnce(&mut Option<Hold>) -> T) -> T {
        let answer = ask(&mut self.hold);
        if self.board.lock().backlog().is_release_due() {
            self.hold = None;
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::{hold, without_networks};
    use r1_funcore::domain::backlog::Backlog;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[derive(Default)]
    struct Pieces(Backlog<&'static str, u32>);

    impl Holder for Pieces {
        type Key = &'static str;
        type Busy = u32;

        fn backlog(&mut self) -> &mut Backlog<&'static str, u32> {
            &mut self.0
        }
    }

    #[test]
    fn piece_taken_when_due() {
        let board = Arc::new(Board::new(Pieces::default()).unwrap());
        let mut epoll = Epoll::with_fd(board.fd());
        board.lock().0.set_wanted(true);
        let scan_count = Cell::new(0);
        let scan = || {
            scan_count.set(scan_count.get() + 1);
            vec!["old"]
        };
        assert_eq!(Worker::new(&board).take(scan, || 7), ("old", false), "what waits on the partition goes");
        assert_eq!(scan_count.get(), 1, "looked for once");
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), vec![7], "and the main loop is told");
        let clone = Arc::clone(&board);
        let thread = thread::spawn(move || Worker::new(&clone).take(Vec::new, || 8));
        board.lock().0.add_new("new");
        board.notify();
        assert_eq!(thread.join().unwrap(), ("new", true), "one handed over wakes the thread that sleeps");
        assert_eq!(board.lock().0.busy().collect::<Vec<_>>(), [(&"old", 7), (&"new", 8)], "each as it was counted");
    }

    #[test]
    fn hold_released_after_last_requests() {
        let radio = without_networks();
        let board = Board::new(Pieces::default()).unwrap();
        let mut worker = Worker::new(&board);
        worker.ask(|slot| *slot = Some(hold(&radio.online())));
        board.lock().0.add_new("next");
        assert!(
            worker.ask_last(|slot| slot.is_some()) && worker.hold.is_some(),
            "asked over the network held, which a piece due keeps"
        );
        assert_eq!(worker.take(Vec::new, || 1), ("next", true));
        assert!(
            worker.ask_last(|slot| slot.is_some()) && worker.hold.is_none(),
            "and let go of once the last askings are answered with none due"
        );
    }
}
