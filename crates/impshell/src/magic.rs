use crate::backlog::Worker;
use crate::log;
use crate::openai::{self, Account};
use crate::storage::Data;
use crate::sync::Board;
use crate::sys;
use r1_funcore::domain::magic::{
    self, DIRECTORY, MAX_PARALLEL, Order, Outcome, Progress, Report, Stage, State, Timings,
};
use r1_funcore::openai::fault::Fault;
use r1_funcore::storage::paths::Listed;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Instant;

struct Shared {
    board: Board<State>,
    data: Data,
    account: Account,
}

impl Shared {
    fn take_photo(&self, worker: &mut Worker<'_, State>) -> String {
        let busy = || Progress { stage: Stage::Waiting, since_ns: sys::monotonic_ns() };
        let scan = || {
            let names = self.data.names(DIRECTORY, Listed::Files);
            names
                .map(|names| magic::to_waiting_photos(&names))
                .inspect_err(|e| log!("magic: looking for photos that wait: {e}"))
                .unwrap_or_default()
        };
        worker.take(scan, busy).0
    }

    fn set_stage(&self, photo: &str, stage: Stage) {
        self.board.lock().backlog.set_busy(photo, Progress { stage, since_ns: sys::monotonic_ns() });
        self.board.raise();
    }

    fn redraw(&self, worker: &mut Worker<'_, State>, photo: &str) -> Result<Outcome, Fault> {
        let (data, start) = (&self.data, Instant::now());
        let is_redraw = self.board.lock().is_redraw(photo);
        let small_picture = magic::take_small(data, photo, is_redraw)
            .inspect_err(|e| log!("magic: {photo}: taking its small picture: {e}"))?;
        let Some(picture) = small_picture else {
            log!("{}", magic::to_needless_line(photo));
            return Ok(Outcome::Needless);
        };
        let (result, mut timings) = worker.ask_last(|slot| {
            let hold = self.account.keep_hold(slot);
            let down = hold.inspect_err(|fault| log!("{}", magic::to_unheld_line(photo, fault)))?;
            let mut timings = Timings { joined: start.elapsed(), bytes: picture.len(), ..Timings::default() };
            self.set_stage(photo, Stage::Joined);
            let result = openai::image::redraw(&self.account, &down, &picture, |step| {
                timings.note(step, start.elapsed());
                self.set_stage(photo, Stage::from(step));
            });
            Ok::<_, Fault>((result, timings))
        })?;
        timings.total = start.elapsed();
        let times = timings.to_log_text();
        match result {
            Ok((picture, cost)) => match magic::keep(data, photo, is_redraw, &picture) {
                Ok(stored) => {
                    log!("{}", stored.to_log_line(photo, picture.len(), &cost, &times, start.elapsed().as_millis()));
                    Ok(stored.outcome())
                }
                Err(e) => {
                    log!("{}", magic::to_unkept_line(photo, &cost, &e, &times));
                    Err(e.into())
                }
            },
            Err(fault) => {
                log!("{}", magic::to_failure_line(photo, &fault, &timings));
                if let Err(e) = magic::end_failed(data, photo, &fault) {
                    log!("magic: {photo}: removing its small picture: {e}");
                }
                Err(fault)
            }
        }
    }
}

fn work(shared: &Shared) -> ! {
    let mut worker = Worker::new(&shared.board);
    loop {
        let photo = shared.take_photo(&mut worker);
        let result = shared.redraw(&mut worker, &photo);
        shared.board.lock().take_outcome(&photo, &result);
        shared.board.raise();
        shared.board.notify();
    }
}

/// The main loop's handle of the pixel cam's threads.
pub struct Magic {
    shared: Arc<Shared>,
}

impl Magic {
    /// Starts the threads, idle until a photo is to be sent.
    pub fn start(data: Data, account: Account) -> io::Result<Magic> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, data, account });
        for _ in 0..MAX_PARALLEL {
            let worker = Arc::clone(&shared);
            sys::spawn_background("magic", move || work(&worker))?;
        }
        Ok(Magic { shared })
    }

    /// Readable when a redraw has got on or ended.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes what the camera and the gallery ask.
    pub fn take_orders(&self, orders: Vec<Order>) {
        if self.shared.board.lock().take_orders(orders) {
            self.shared.board.notify();
        }
    }

    /// Sets whether a photo viewfinder or the roll shows; when one opens, every waiting photo is tried again.
    pub fn want(&self, is_wanted: bool) {
        if self.shared.board.lock().backlog.set_wanted(is_wanted) {
            self.shared.board.notify();
        }
    }

    /// Takes the report for the screen.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use std::time::Duration;

    #[test]
    fn no_partition_takes_nothing() {
        let radio = without_networks();
        let magic = Magic::start(Data::unusable(), Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(magic.fd());
        magic.want(true);
        magic.want(false);
        assert_eq!(magic.latest(), Report::default());
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "nothing to tell the main loop of");
    }
}
