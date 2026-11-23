use crate::log;
use crate::pool::ThreadPool;
use crate::sync::Board;
use crate::sys::{self, Wake};
use crate::wifi::radio::{Hold, Online};
use r1_funcore::domain::rundown::{
    self, JOINED_LINE, Pings, Report, Run, State, Status, Step, Switch, TARGET, round_wait,
};
use r1_funcore::net::MTU;
use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

static POOL: ThreadPool = ThreadPool::new("rundown");

struct Shared {
    board: Board<State>,
    stop: Wake,
}

fn post(shared: &Shared, status: Option<Status>) {
    if shared.board.lock().add_status(status) {
        shared.board.raise();
    }
}

fn rest(shared: &Shared, wait: Duration) {
    let mut stop = [sys::readable(shared.stop.fd())];
    match sys::poll(&mut stop, Some(Instant::now() + wait)) {
        Ok(_) if stop[0].revents != 0 => shared.stop.clear(),
        Ok(_) => {}
        Err(e) => log!("rundown: waiting: {e}"),
    }
}

fn ping(shared: &Shared, hold: &Hold, run: &mut Run) -> io::Result<()> {
    let socket = sys::socket(libc::AF_INET, libc::SOCK_RAW | libc::SOCK_NONBLOCK, libc::IPPROTO_ICMP)?;
    let (mut pings, mut packet) = (Pings::default(), [0u8; MTU]);
    while shared.board.lock().is_wanted {
        let echo = pings.echo();
        let send_time = Instant::now();
        let result = sys::send_to(&socket, &SocketAddr::from((TARGET, 0)), &echo);
        if let (true, Err(e)) = (pings.note_sent(result.is_ok(), run), &result) {
            log!("rundown: an echo was not sent: {e}");
        }
        while let Some(wait) = round_wait(send_time.elapsed()) {
            let mut fds =
                [sys::readable(socket.as_raw_fd()), sys::readable(hold.down().fd()), sys::readable(shared.stop.fd())];
            sys::poll(&mut fds, Some(Instant::now() + wait))?;
            if fds[1].revents != 0 {
                return Ok(());
            }
            if fds[2].revents != 0 {
                shared.stop.clear();
                if !shared.board.lock().is_wanted {
                    return Ok(());
                }
            }
            if fds[0].revents != 0 {
                loop {
                    let count = unsafe { libc::recv(socket.as_raw_fd(), packet.as_mut_ptr().cast(), packet.len(), 0) };
                    let Ok(length) = sys::check_count(count) else {
                        break;
                    };
                    pings.take_packet(&packet[..length], send_time.elapsed());
                }
            }
        }
        let end = pings.end_round(run);
        if let Some(line) = end.to_rundown_line() {
            log!("{line}");
        }
        post(shared, end.status);
    }
    Ok(())
}

fn join(shared: &Shared, online: &Online, run: &mut Run) {
    post(shared, run.begin_join());
    match online.hold() {
        Ok(hold) => {
            log!("{JOINED_LINE}");
            run.note_joined();
            if let Err(e) = ping(shared, &hold, run) {
                log!("rundown: pinging: {e}");
                post(shared, run.note_unanswered());
            }
        }
        Err(reason) => {
            let (status, is_new) = run.note_join_failed(&reason);
            if is_new {
                log!("rundown: {reason}");
            }
            post(shared, status);
        }
    }
}

fn work(shared: &Shared, online: &Online) {
    sys::set_background_priority();
    let (start, mut run) = (Instant::now(), Run::default());
    log!("{}", rundown::to_on_line());
    loop {
        let step = run.next_step(&mut shared.board.lock());
        match step {
            Step::Join => join(shared, online, &mut run),
            Step::Rest(wait) => rest(shared, wait),
            Step::End => break,
        }
    }
    log!("{}", run.to_off_line(start.elapsed().as_secs()));
}

/// The main loop's handle of the battery rundown.
pub struct Rundown {
    shared: Arc<Shared>,
    online: Online,
}

impl Rundown {
    /// A rundown that holds the network through `online` while it is wanted.
    pub fn new(online: Online) -> io::Result<Rundown> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, stop: Wake::new()? });
        Ok(Rundown { shared, online })
    }

    /// Readable when the rundown's status changed.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes the rundown's report, the status where it changed.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }

    fn start_thread(&self) {
        self.shared.stop.clear();
        let (shared, online) = (Arc::clone(&self.shared), self.online.clone());
        if let Err(e) = POOL.run(move || work(&shared, &online)) {
            self.shared.board.lock().note_no_thread();
            self.shared.board.raise();
            log!("rundown: no thread for it: {e}");
        }
    }

    /// Sets whether the rundown is on: on starts its thread unless it runs, off has that thread let the network go.
    pub fn want(&self, is_wanted: bool) {
        let switch = self.shared.board.lock().set_wanted(is_wanted);
        match switch {
            Switch::Nothing => {}
            Switch::Stop => self.shared.stop.raise(),
            Switch::Start => self.start_thread(),
        }
    }

    /// True while no thread of the rundown runs.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::rundown::PERIOD;
    use std::thread;

    #[test]
    fn no_network_then_switch_off() {
        let radio = without_networks();
        let rundown = Rundown::new(radio.online()).unwrap();
        let mut epoll = Epoll::with_fd(rundown.fd());
        assert!(rundown.is_off() && rundown.latest() == Report::default(), "nothing runs before it is switched on");
        rundown.want(true);
        assert!(!rundown.is_off(), "switched on, a thread runs");
        let mut statuses = Vec::new();
        while statuses.last() != Some(&Status::Offline) {
            assert_eq!(epoll.wait(None).unwrap(), vec![7]);
            statuses.extend(rundown.latest().status);
        }
        assert!(
            statuses.iter().all(|status| [Status::Joining, Status::Offline].contains(status)),
            "here without the driver's nodes the radio does not come on: {statuses:?}"
        );
        rundown.want(true);
        assert!(!rundown.is_off(), "a switch that is on already starts nothing more");
        let start = Instant::now();
        rundown.want(false);
        while !rundown.is_off() {
            assert!(start.elapsed() < 10 * PERIOD, "the thread never ends");
            thread::yield_now();
        }
        assert!(start.elapsed() < PERIOD / 2, "the switch turned off ends the wait for the next asking at once");
        rundown.want(true);
        assert!(!rundown.is_off(), "and on again, it runs again");
        rundown.want(false);
        while !rundown.is_off() {
            thread::yield_now();
        }
    }
}
