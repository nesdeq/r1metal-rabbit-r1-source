mod service;

use crate::net::dial::await_turn;
use crate::storage::Data;
use crate::sync::Board;
use crate::sys::{self, Wake};
use crate::wifi::radio::{self, Hold, NetworkDown, Online};
use crate::{clock, log};
use r1_funcore::base::handoff::Step;
use r1_funcore::domain::clock::{HoldCheck, HoldStep, Report, StayCheck, TimeAnswer, to_time_line};
use r1_funcore::domain::place::{self, LOCATION_FILE, OFFSET_FILE};
use r1_funcore::net::sntp;
use r1_funcore::openai::chat::request::Location;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

/// The device's last stored location; none while the place service never told one.
pub fn read_location(data: &Data) -> Option<Location> {
    data.text(place::DIRECTORY, LOCATION_FILE).ok().flatten().and_then(|text| place::parse(&text))
}

// Asks the time service and sets the clocks by its answer; the log tells the outcome.
fn ask_clocks(down: &NetworkDown) -> io::Result<Option<i64>> {
    let start = Instant::now();
    let result = service::ask_time(down).and_then(|time| clock::set_from_server(time, sntp::SERVICE));
    match &result {
        Ok(change) => log!("{}", to_time_line(*change, start.elapsed().as_millis())),
        Err(e) => log!("net: time failed after {} ms: {e}", start.elapsed().as_millis()),
    }
    result
}

type Order = r1_funcore::domain::clock::Order<Hold, Online>;
type State = r1_funcore::domain::clock::State<Hold, Online, Arc<Wake>>;

/// The main loop's handle of the thread that keeps the clocks and the offset from UTC.
pub struct Timing {
    board: Arc<Board<State>>,
    data: Data,
}

impl Timing {
    /// Starts the thread, which lives as long as this handle.
    pub fn new(data: Data) -> io::Result<Arc<Timing>> {
        let board = Arc::new(Board::new(State::default())?);
        let timing = Arc::new(Timing { board: Arc::clone(&board), data });
        let handle = Arc::downgrade(&timing);
        thread::Builder::new().name("time".into()).spawn(move || {
            while let Step::Serve(order) = board.wait(|state| state.orders.next_step()) {
                if let Some(timing) = handle.upgrade() {
                    timing.serve(order);
                }
            }
        })?;
        Ok(timing)
    }

    fn order(&self, order: Order) {
        self.board.lock().orders.add(order);
        self.board.notify();
    }

    /// What every hold of the network runs beside it, for the radio to be started with.
    pub fn hold_hook(self: &Arc<Timing>) -> radio::HoldHook {
        let timing = Arc::clone(self);
        Arc::new(move |hold, is_quiet| timing.on_hold(hold, is_quiet).unwrap_or_else(|e| log!("net: time: {e}")))
    }

    // Beside a hold: waits for the first check since the start or makes it, as TLS needs the clocks, then has the clock thread ask for the place or the time beside the stay, under the hold, where that is due; a wait whose stay ends first fails.
    fn on_hold(&self, hold: &Hold, is_quiet: bool) -> io::Result<()> {
        let mut check = HoldCheck::new(is_quiet);
        let order = loop {
            let mut state = self.board.lock();
            let step = state.next_hold_step(&mut check, sys::boottime_seconds());
            match step {
                HoldStep::Await => await_turn(state, State::add_waiter, hold.down())?,
                HoldStep::Check => {
                    drop(state);
                    let is_set = ask_clocks(hold.down()).is_ok();
                    let waiters = self.board.lock().end_hold_check(&mut check, is_set, sys::boottime_seconds());
                    waiters.iter().for_each(|waiter| waiter.raise());
                }
                HoldStep::Order(StayCheck::Locate) => break Order::Locate(hold.share()),
                HoldStep::Order(StayCheck::Check) => break Order::Check(hold.share()),
                HoldStep::Done => return Ok(()),
            }
        };
        self.order(order);
        Ok(())
    }

    fn serve(&self, order: Order) {
        match order {
            Order::Locate(hold) => self.locate(hold.down()),
            Order::Check(hold) => drop(self.check(hold.down())),
            Order::Ask(online, is_for_screen) => self.answer(&online, is_for_screen),
        }
    }

    /// Readable when an asking has ended or the place service told a new offset.
    pub fn fd(&self) -> RawFd {
        self.board.fd()
    }

    // A check of the clock thread's own, the first since the start where no hold made one or makes one.
    fn set_clocks(&self, down: &NetworkDown) -> io::Result<Option<i64>> {
        let is_first = self.board.lock().begin_check();
        let result = ask_clocks(down);
        let waiters = self.board.lock().end_check(is_first, result.is_ok(), sys::boottime_seconds());
        waiters.iter().for_each(|waiter| waiter.raise());
        result
    }

    fn locate(&self, down: &NetworkDown) {
        let start = Instant::now();
        let (offset, place) = match service::ask_place(down) {
            Ok(answer) => answer,
            Err(e) => return log!("net: place failed after {} ms: {e}", start.elapsed().as_millis()),
        };
        // The place itself is kept for the conversation's web search, and never written into the log.
        if let Some(place) = place
            && place::is_new(read_location(&self.data).as_ref(), &place)
            && let Err(e) = self.data.put(place::DIRECTORY, LOCATION_FILE, place::to_text(&place).as_bytes())
        {
            log!("net: place: keeping where the device is: {e}");
        }
        let stored = self.data.text(place::DIRECTORY, OFFSET_FILE).ok().flatten();
        if !place::has_offset_changed(stored.as_deref(), offset) {
            return log!("{}", place::to_same_line(offset, start.elapsed().as_millis()));
        }
        if let Err(e) = self.data.put(place::DIRECTORY, OFFSET_FILE, place::to_offset_file(offset).as_bytes()) {
            log!("net: place: keeping it: {e}");
        }
        log!("{}", place::to_moved_line(offset, start.elapsed().as_millis()));
        self.board.lock().report.offset = Some(offset);
        self.board.raise();
    }

    fn check(&self, down: &NetworkDown) -> io::Result<Option<i64>> {
        let change = self.set_clocks(down)?;
        self.locate(down);
        Ok(change)
    }

    fn post(&self, answer: TimeAnswer) {
        self.board.lock().report.answer = Some(answer);
        self.board.raise();
    }

    /// Asks for the time on the thread, which holds the network for it; the main loop hears the outcome where `is_for_screen`, the log alone otherwise.
    pub fn ask(&self, online: Online, is_for_screen: bool) {
        self.order(Order::Ask(online, is_for_screen));
    }

    fn answer(&self, online: &Online, is_for_screen: bool) {
        let hold = online.hold_quietly().inspect_err(|reason| log!("net: time: {reason}"));
        let result = hold.map(|hold| self.check(hold.down()).ok().map(|change| change.is_some()));
        let answer = TimeAnswer::from_result(result.as_ref().map(|result| *result));
        if is_for_screen {
            self.post(answer);
        }
    }

    /// Takes the report: the last ended asking's outcome and a new offset, each where one came.
    pub fn latest(&self) -> Report {
        self.board.take(State::take_report)
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        self.board.lock().orders.close();
        self.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::{hold, without_networks};
    use r1_funcore::domain::cause::Cause;

    #[test]
    fn no_network_told_once() {
        let (timing, radio) = (Timing::new(Data::unusable()).unwrap(), without_networks());
        let mut epoll = Epoll::with_fd(timing.fd());
        assert_eq!(timing.latest(), Report::default());
        // The thread finds no network here, without the driver's nodes.
        timing.ask(radio.online(), true);
        assert_eq!(epoll.wait(None).unwrap(), vec![7], "the main loop is woken for it");
        assert_eq!(
            (timing.latest(), timing.latest()),
            (Report { answer: Some(TimeAnswer::Failed(Cause::NoNetwork)), offset: None }, Report::default())
        );
    }

    #[test]
    fn holds_wait_for_first_check() {
        let (timing, radio) = (Timing::new(Data::unusable()).unwrap(), without_networks());
        let mut first = HoldCheck::new(false);
        let step = timing.board.lock().next_hold_step(&mut first, sys::boottime_seconds());
        assert_eq!(step, HoldStep::Check, "the first hold makes the first check");
        let wait = |hold: Hold| {
            let timing = Arc::clone(&timing);
            thread::spawn(move || timing.on_hold(&hold, false).map_err(|e| e.kind()))
        };
        let (waiting, gone) = (hold(&radio.online()), hold(&radio.online()));
        gone.down().raise();
        assert_eq!(
            wait(gone).join().unwrap(),
            Err(io::ErrorKind::NetworkDown),
            "a hold whose stay ended waits no more"
        );
        let second = wait(waiting);
        for _ in 0..100 {
            thread::yield_now();
        }
        assert!(!second.is_finished(), "a hold does not go on to TLS while the first check is made");
        let waiters = timing.board.lock().end_hold_check(&mut first, true, sys::boottime_seconds());
        waiters.iter().for_each(|waiter| waiter.raise());
        assert_eq!(second.join().unwrap(), Ok(()), "it goes on once the check has ended, and makes none of its own");
    }
}
