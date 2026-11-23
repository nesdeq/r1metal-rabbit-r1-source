use super::Account;
use crate::log;
use crate::sync::Board;
use r1_funcore::base::handoff::Step;
use r1_funcore::openai::key::{
    CheckResult, Order, REFUSED_LINE, Report, SCANNED_KEY, STORED_KEY, Source, State, Verdict, parse_verdict,
    to_checked_line,
};
use r1_funcore::openai::models;
use r1_funcore::openai::request::parse_target;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;

fn ask_verdict(account: &Account, key: &str, label: &str) -> Option<Verdict> {
    let hold = match account.0.online.hold() {
        Ok(hold) => hold,
        Err(reason) => {
            log!("openai: {label} is not checked: {reason}");
            return None;
        }
    };
    let target = parse_target(models::check::ADDRESS);
    let result = target.and_then(|target| Ok(account.check(&target, key, hold.down())?));
    drop(hold);
    Some(match result {
        Ok((status, body)) => {
            let verdict = parse_verdict(status, &body);
            log!("{}", to_checked_line(label, status, verdict));
            verdict
        }
        Err(e) => {
            log!("openai: checking {label}: {e}");
            Verdict::Unanswered
        }
    })
}

fn check_key(account: &Account, key: &str) -> CheckResult {
    let Some(verdict) = ask_verdict(account, key, SCANNED_KEY) else {
        return CheckResult::Offline;
    };
    let is_stored = verdict.should_store()
        && match account.write_key(key) {
            Ok(()) => {
                log!("{}", Source::Check.to_log_line());
                true
            }
            Err(e) => {
                log!("openai: the service knows the key, which is not kept: {e}");
                false
            }
        };
    verdict.to_check_result(is_stored)
}

fn recheck_key(account: &Account) -> bool {
    let key = match account.read_key() {
        Ok(Some(key)) => key,
        Ok(None) => return false,
        Err(e) => {
            log!("openai: reading the kept key to check it anew: {e}");
            return false;
        }
    };
    if !ask_verdict(account, &key, STORED_KEY).is_some_and(Verdict::should_remove) {
        return false;
    }
    match account.remove_key() {
        Ok(()) => log!("{REFUSED_LINE}"),
        Err(e) => log!("openai: the service refuses the kept key, which could not be taken away: {e}"),
    }
    true
}

fn work(account: &Account, board: &Board<State>) {
    while let Step::Serve(order) = board.wait(|state| state.orders.next_step()) {
        let report = match order {
            Order::Begin(flash_key) => {
                // The flash's stamp is kept only once its key is, which the main loop tells the settings' keeper.
                let is_flashed = flash_key.is_some_and(|key| match account.write_key(&key) {
                    Ok(()) => {
                        log!("{}", Source::Flash.to_log_line());
                        true
                    }
                    Err(e) => {
                        log!("openai: keeping the key the flash gave: {e}");
                        false
                    }
                });
                let has_key =
                    account.has_key().inspect_err(|e| log!("openai: reading whether a key is kept: {e}")).ok();
                Report::with_start(has_key, is_flashed)
            }
            Order::Check(number, key) => Report::from_check(number, check_key(account, &key)),
            Order::Keep(key) => {
                let result = account.write_key(&key).map_err(|e| e.to_string());
                match &result {
                    Ok(()) => log!("{}", Source::Console.to_log_line()),
                    Err(reason) => log!("openai: keeping a key the console handed over: {reason}"),
                }
                Report::from_handover(result)
            }
            Order::Recheck => Report::from_recheck(recheck_key(account)),
        };
        board.lock().add_report(report);
        board.raise();
    }
}

/// The main loop's handle of the key's thread.
pub struct Keys {
    board: Arc<Board<State>>,
}

impl Keys {
    /// Starts the thread, which stores the key of `account` and reaches the service through it.
    pub fn start(account: Account) -> io::Result<Keys> {
        let board = Arc::new(Board::new(State::default())?);
        let worker = Arc::clone(&board);
        thread::Builder::new().name("keys".into()).spawn(move || work(&account, &worker))?;
        Ok(Keys { board })
    }

    fn order(&self, order: Order) {
        self.board.lock().orders.add(order);
        self.board.notify();
    }

    /// Readable when a report waits.
    pub fn fd(&self) -> RawFd {
        self.board.fd()
    }

    /// Has the thread report whether a key is stored, storing `flash_key` first.
    pub fn begin(&self, flash_key: Option<String>) {
        self.order(Order::Begin(flash_key));
    }

    /// Asks for the check of a scanned key by number, once per check; a key that works is stored.
    pub fn check(&self, request: Option<(u32, &str)>) {
        if self.board.lock().ask_check(request) {
            self.board.notify();
        }
    }

    /// Stores a key the console handed over.
    pub fn keep(&self, key: String) {
        self.order(Order::Keep(key));
    }

    /// Checks the stored key again, which a service refused.
    pub fn recheck(&self) {
        self.order(Order::Recheck);
    }

    /// Takes the thread's report; an ended check is handed again while the window waits for it.
    pub fn latest(&self) -> Report {
        self.board.take(State::take_report)
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.board.lock().orders.close();
        self.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;

    const KEY: &str = "sk-proj-Fake0Key_-9";

    fn next_report(keys: &Keys, epoll: &mut Epoll) -> Report {
        assert_eq!(epoll.wait(None).unwrap(), vec![7], "the thread tells when it has done");
        keys.latest()
    }

    #[test]
    fn no_partition_no_network() {
        let radio = without_networks();
        let keys = Keys::start(Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(keys.fd());
        keys.begin(Some(KEY.into()));
        assert_eq!(
            next_report(&keys, &mut epoll),
            Report { has_key: Some(false), ..Report::default() },
            "the flash's key cannot be kept, and none is"
        );
        keys.check(Some((4, KEY)));
        assert_eq!(
            next_report(&keys, &mut epoll),
            Report { check: Some((4, CheckResult::Offline)), ..Report::default() }
        );
        keys.keep(KEY.into());
        let report = next_report(&keys, &mut epoll);
        assert!(
            report.has_key.is_none() && report.store.is_some_and(|store| store.is_err()),
            "the console is told why not"
        );
        assert_eq!(keys.latest(), Report::default(), "news comes once");
        keys.recheck();
        assert_eq!(
            next_report(&keys, &mut epoll),
            Report::default(),
            "without a partition there is no kept key to check anew"
        );
    }
}
