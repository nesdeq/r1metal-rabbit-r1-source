use super::Board;
use r1_funcore::base::handoff::{Fetch, Step};
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;

/// A thread that answers the main loop's requests in order; the newest answer waits for the main loop.
pub struct Fetcher<R, A> {
    board: Arc<Board<Fetch<R, A>>>,
}

impl<R: Send + 'static, A: Send + 'static> Fetcher<R, A> {
    /// Starts the thread, which answers each request by `answer` until the fetcher is dropped.
    pub fn start(name: &str, mut answer: impl FnMut(R) -> A + Send + 'static) -> io::Result<Fetcher<R, A>> {
        let board = Arc::new(Board::new(Fetch::default())?);
        let worker = Arc::clone(&board);
        thread::Builder::new().name(name.into()).spawn(move || {
            while let Step::Serve(request) = worker.wait(|fetch| fetch.orders.next_step()) {
                let result = answer(request);
                let previous = worker.lock().post(result);
                worker.raise();
                drop(previous);
            }
        })?;
        Ok(Fetcher { board })
    }

    /// Readable while an answer waits.
    pub fn fd(&self) -> RawFd {
        self.board.fd()
    }

    /// Asks once for each wanted request.
    pub fn want(&self, request: Option<R>)
    where
        R: Clone + PartialEq,
    {
        if self.board.lock().want(request) {
            self.board.notify();
        }
    }

    /// Takes the newest answer, if any.
    pub fn latest(&self) -> Option<A> {
        self.board.take(Fetch::take_answer)
    }
}

impl<R, A> Drop for Fetcher<R, A> {
    fn drop(&mut self) {
        self.board.lock().orders.close();
        self.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;

    #[test]
    fn answers_in_order() {
        let fetcher = Fetcher::start("test", |request: u32| request * 2).unwrap();
        let mut epoll = Epoll::with_fd(fetcher.fd());
        fetcher.board.lock().orders.add(4);
        fetcher.board.lock().orders.add(5);
        fetcher.board.notify();
        while fetcher.latest().is_none_or(|answer| answer != 10) {
            assert_eq!(epoll.wait(None).unwrap(), vec![7], "the thread tells when it has answered");
        }
        assert_eq!(fetcher.latest(), None, "the newest is answered last");
    }
}
