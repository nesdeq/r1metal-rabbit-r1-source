use super::{Account, Key};
use crate::sys::{self, lock};
use crate::wifi::radio::{Hold, NetworkDown, Online};
use crate::{crypto, log};
use r1_funcore::codec::http::{GET, POST, Target};
use r1_funcore::domain::bell;
use r1_funcore::domain::chat::recollect;
use r1_funcore::domain::news::request;
use r1_funcore::domain::news::stories::to_input;
use r1_funcore::domain::paper::sheet::summary_task;
use r1_funcore::domain::web;
use r1_funcore::openai::chat::request::Location;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::ledger::{Entry, SummaryKind};
use r1_funcore::openai::models::writer;
use r1_funcore::openai::request::{JSON, parse_target};
use r1_funcore::openai::summary::{
    self, Cost, Job, Poll, RESUMED_LINE, RUNDOWN, Summary, UNFINISHED, Watch, Work, WorkStatus, cancel_target,
    parse_poll, parse_status, poll_target, status_cost, to_asked_line,
};
use std::time::{Duration, Instant};

fn pause(down: Option<&NetworkDown>, duration: Duration) -> bool {
    let mut fds: Vec<_> = down.into_iter().map(|down| sys::readable(down.fd())).collect();
    let ready = sys::poll(&mut fds, Some(Instant::now() + duration));
    ready.inspect_err(|e| log!("openai: waiting for background work: {e}")).is_ok_and(|ready| ready == 0)
}

fn digest(request: &str) -> String {
    let mut sum = crypto::Sum::new();
    sum.add(request.as_bytes());
    sum.hex()
}

// Books what an answer cost, where it cost anything, as `entry` tells.
fn book(account: &Account, cost: Option<Cost>, entry: Entry) {
    if let Some(cost) = cost {
        account.add(|ledger, year, month| cost.book(ledger, year, month, entry));
    }
}

fn ask_work(account: &Account, slot: &mut Option<Hold>, request: &str, entry: Entry) -> Result<(Summary, u32), Fault> {
    let (key, target, request_digest) = (account.key()?, parse_target(writer::ADDRESS)?, digest(request));
    let poller = WorkPoller { key, account: account.clone(), entry };
    let online = &account.0.online;
    let down = online.keep(slot)?;
    let pending = lock(&account.0.state).jobs.take(&request_digest, sys::boottime_ns());
    let (name, start_ns) = match pending {
        Some(Job { id, start_ns, .. }) => {
            log!("{RESUMED_LINE}");
            (id, start_ns)
        }
        None => {
            account.check_limit()?;
            let start_ns = sys::boottime_ns();
            let (status, answer) = poller.key.ask((&target, &down), POST, Some(JSON), request.as_bytes(), |_| {})?;
            let work_status = parse_status(status, &answer);
            book(account, status_cost(&work_status, &answer), entry);
            match work_status? {
                WorkStatus::Done(summary) => return Ok((summary, 0)),
                WorkStatus::Pending(name, _) => (name, start_ns),
            }
        }
    };
    let Some((summary, polls)) = wait_work((online, slot), Watch::new(start_ns), |down| poller.poll(&name, down))?
    else {
        let job = Job { digest: request_digest, id: name, start_ns };
        lock(&account.0.state).jobs.add(job, sys::boottime_ns());
        return Err(Fault::Temporary(UNFINISHED.into()));
    };
    Ok((summary, polls))
}

// Has the work a request asks written, waits for it, and logs and books it as funcore says of its kind; `input` is what the request hands the model.
fn write_text(
    account: &Account,
    slot: &mut Option<Hold>,
    work: Work,
    request: &str,
    input: &str,
) -> Result<Summary, Fault> {
    let start = Instant::now();
    let (summary, polls) = ask_work(account, slot, request, work.entry())?;
    log!("{}", summary.to_written_line(&work.to_name(input), start.elapsed().as_millis(), polls));
    Ok(summary)
}

/// Has the model of the owner's choice summarize `words` and books it as a summary of `kind`, over the network `slot` keeps.
pub fn write_summary(
    account: &Account,
    slot: &mut Option<Hold>,
    kind: SummaryKind,
    words: &str,
) -> Result<Summary, Fault> {
    let work = Work::Summary(kind);
    let request = summary::request(account.thinking(work.task()), summary_task(kind), words);
    write_text(account, slot, work, &request, words)
}

/// Has the model of the owner's choice settle a day of the conversation into its memory and books it as the conversation's, over the network `slot` keeps.
pub fn write_recollection(account: &Account, slot: &mut Option<Hold>, input: &str) -> Result<String, Fault> {
    let work = Work::Recollection;
    let request = summary::request(account.thinking(work.task()), recollect::PROMPT, input);
    Ok(write_text(account, slot, work, &request, input)?.text)
}

/// Has the language model write the owner's next message to a Signal conversation in auto mode and books it as an auto reply, over the network `slot` keeps.
pub fn write_auto_reply(account: &Account, slot: &mut Option<Hold>, request: &str) -> Result<String, Fault> {
    Ok(write_text(account, slot, Work::AutoReply, request, request)?.text)
}

/// Has the model of the owner's choice take a step for the browser on `input`, to resolve what was asked or to write a page, free to search the web from `location`, and books it as a page, over the network `slot` keeps.
pub fn write_page(
    account: &Account,
    slot: &mut Option<Hold>,
    input: &str,
    location: Option<&Location>,
) -> Result<String, Fault> {
    let work = Work::Page;
    let request = summary::search_request(account.thinking(work.task()), web::TASK, input, location);
    Ok(write_text(account, slot, work, &request, input)?.text)
}

/// Has the model of the owner's choice read a request to the bell into the calls that set and cancel its rings, and books it as a request to the bell, over the network `slot` keeps.
pub fn write_bell_calls(account: &Account, slot: &mut Option<Hold>, input: &str) -> Result<String, Fault> {
    let work = Work::BellCalls;
    let request = summary::request(account.thinking(work.task()), &bell::task(), input);
    Ok(write_text(account, slot, work, &request, input)?.text)
}

/// Has the model of the owner's choice read a request for the rundowns into the lines that change them, free to search the web from `location` for a new one's feeds, and books it as work for the rundowns, over the network `slot` keeps.
pub fn write_rundown_lines(
    account: &Account,
    slot: &mut Option<Hold>,
    input: &str,
    location: Option<&Location>,
) -> Result<String, Fault> {
    let work = Work::RundownLines;
    let request = summary::search_request(account.thinking(work.task()), &request::task(), input, location);
    Ok(write_text(account, slot, work, &request, input)?.text)
}

/// Puts a day's rundown of that name to the language model with the `task` that tells how it is written, `day` written `yyyy-mm-dd`; returns the work's status, as a rule pending under the name to poll by. A rundown there at once is booked.
pub fn ask_rundown(
    account: &Account,
    down: &NetworkDown,
    (topic, task): (&str, &str),
    day: &str,
    report: &str,
) -> Result<WorkStatus, Fault> {
    let (key, target, start) = (account.paying()?, parse_target(writer::ADDRESS)?, Instant::now());
    let (model_use, entry) = RUNDOWN;
    let request = summary::request(account.thinking(model_use), task, &to_input(day, report));
    let (status, answer) = key.ask((&target, down), POST, Some(JSON), request.as_bytes(), |_| {})?;
    let work_status = parse_status(status, &answer);
    book(account, status_cost(&work_status, &answer), entry);
    let work_status = work_status?;
    log!("{}", to_asked_line(topic, report, start.elapsed().as_millis(), &work_status));
    Ok(work_status)
}

/// Polls the language model's work over the connection a stay keeps open, booking what it came to.
pub struct WorkPoller {
    key: Key,
    account: Account,
    entry: Entry,
}

impl WorkPoller {
    /// Polls rundowns with the account's key, read before any network is held for it.
    pub fn new(account: &Account) -> Result<WorkPoller, Fault> {
        Ok(WorkPoller { key: account.key()?, account: account.clone(), entry: RUNDOWN.1 })
    }

    /// Polls once for the work of that name in the stay `down`, over a connection the stay left open where there is one; books what the poll found.
    pub fn poll(&self, name: &str, down: &NetworkDown) -> Poll {
        self.ask(poll_target(name), GET, down)
    }

    /// Has the service cancel the work of that name in the stay `down`; books what its answer tells as a poll does.
    pub fn cancel(&self, name: &str, down: &NetworkDown) -> Poll {
        self.ask(cancel_target(name), POST, down)
    }

    fn ask(&self, target: Result<Target, Fault>, method: &str, down: &NetworkDown) -> Poll {
        match target.and_then(|target| Ok(self.key.ask((&target, down), method, None, &[], |_| {})?)) {
            Ok((status, answer)) => {
                let poll = parse_poll(status, &answer);
                book(&self.account, poll.cost(&answer), self.entry);
                poll
            }
            Err(fault) => Poll::from_fault(&fault),
        }
    }
}

fn wait_work(
    (online, slot): (&Online, &mut Option<Hold>),
    mut watch: Watch,
    mut poll: impl FnMut(&NetworkDown) -> Poll,
) -> Result<Option<(Summary, u32)>, Fault> {
    loop {
        let (due_ns, should_hold) = watch.next();
        if !should_hold {
            *slot = None;
        }
        let wait = Duration::from_nanos(due_ns.saturating_sub(sys::boottime_ns()));
        if !pause(slot.as_ref().map(Hold::down), wait) {
            return Ok(None);
        }
        let Ok(down) = online.keep(slot) else { return Ok(None) };
        if let Some(result) = watch.note_poll(poll(&down), sys::boottime_ns()) {
            return result.map(Some);
        }
    }
}

/// Logs a day's rundown the model wrote, booked as its answer was read; `elapsed` is the time since it was asked for.
pub fn log_rundown(topic: &str, summary: &Summary, elapsed: Duration) {
    log!("{}", summary.to_rundown_line(topic, elapsed.as_secs()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock;
    use crate::wifi::radio::tests::{up, without_networks};
    use r1_funcore::base::time::Moment;
    use r1_funcore::openai::ledger::Ledger;

    #[test]
    fn pause_ends_on_network_down() {
        let down = up();
        let start = Instant::now();
        assert!(pause(Some(&down), Duration::from_millis(30)) && start.elapsed() >= Duration::from_millis(30));
        down.raise();
        let start = Instant::now();
        assert!(!pause(Some(&down), Duration::from_secs(60)) && start.elapsed() < Duration::from_secs(1));
        assert!(pause(None, Duration::from_millis(30)) && start.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn work_polled_in_stay() {
        let radio = without_networks();
        assert!(
            matches!(wait_work((&radio.online(), &mut None), Watch::new(0), |_| unreachable!()), Ok(None)),
            "here no network is known: nothing is asked"
        );
    }

    #[test]
    fn cost_booked_in_held_ledger() {
        let account = crate::openai::tests::unusable();
        lock(&account.0.state).ledger.give(Some(Ledger::default()));
        let (now, entry) = (Moment::from_seconds(clock::local()), RUNDOWN.1);
        book(&account, None, entry);
        assert_eq!(account.ledger().unwrap(), Ledger::default(), "an answer that cost nothing leaves the book alone");
        book(&account, Some(Cost::Done(7_000)), entry);
        let mut expected = Ledger::default();
        expected.add(now.year, now.month, entry, 7_000);
        assert_eq!(account.ledger().unwrap(), expected, "what an answer cost is booked in the month the clock tells");
    }

    #[test]
    fn digest_of_request() {
        assert_eq!(digest("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "FIPS 180");
        assert_ne!(digest("summarize a"), digest("summarize b"));
    }

    #[test]
    fn no_key_no_request() {
        let (account, down, mut slot) = (crate::openai::tests::unusable(), up(), None);
        assert!(
            matches!(write_summary(&account, &mut slot, SummaryKind::Note, "words"), Err(Fault::Temporary(_))),
            "a partition that cannot be read"
        );
        assert!(matches!(
            ask_rundown(&account, &down, ("news", "Write it."), "2026-10-13", "stories"),
            Err(Fault::Temporary(_))
        ));
        assert!(matches!(write_rundown_lines(&account, &mut slot, "words", None), Err(Fault::Temporary(_))));
        assert!(matches!(WorkPoller::new(&account), Err(Fault::Temporary(_))), "nor is anything asked after");
        assert!(matches!(write_recollection(&account, &mut slot, "words"), Err(Fault::Temporary(_))));
        assert!(matches!(write_auto_reply(&account, &mut slot, "words"), Err(Fault::Temporary(_))));
        assert!(matches!(write_page(&account, &mut slot, "words", None), Err(Fault::Temporary(_))));
        assert!(matches!(write_bell_calls(&account, &mut slot, "words"), Err(Fault::Temporary(_))));
    }
}
