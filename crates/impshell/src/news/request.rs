use super::gather;
use crate::log;
use crate::openai::{Account, write};
use crate::storage::Data;
use crate::timekeeper::read_location;
use crate::voiced::{ChoreRunner, Part, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::domain::news::request::{self, Change, PROMPT, Reading, Step};
use r1_funcore::domain::voice::{Phase, Query};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use std::ffi::CStr;
use std::io;

/// The worker of the job that takes a request said or typed for the rundowns, with the partition that tells where the device is.
pub struct Taker {
    data: Data,
}

/// The main loop's handle of that job.
pub type Curator = Part<Taker>;

impl Curator {
    /// Starts the threads, idle until the rundowns' list shows; `data` holds the place the model's search starts from.
    pub fn start(account: Account, data: Data) -> io::Result<Curator> {
        Part::spawn(Taker { data }, false, account)
    }
}

impl Taker {
    // Writes a request said down, has the language model read it into the changes it asks, and reads every feed a change adds: the count of the changes asked, and those of them that stand.
    fn read(&self, shared: &Shared<Self>, query: Query, down: &NetworkDown) -> Result<(usize, Vec<Change>), Fault> {
        let subject = (Subject::RundownsRequest, PROMPT);
        let unwritten = "news: the request said for the rundowns is not written down";
        let said = shared.write_down(query, down, subject, unwritten, None)?;
        shared.set_phase(Phase::Thinking);
        // Two statements: the board's lock, which the main loop takes at this job's news, is let go before the file is read.
        let standing = shared.board.lock().standing().clone();
        let location = read_location(&self.data);
        let reply =
            write::write_rundown_lines(&shared.account, &mut None, &standing.to_input(&said), location.as_ref())
                .inspect_err(|fault| log!("news: the request for the rundowns is not read: {fault}"))?;
        shared.set_phase(Phase::Searching);
        let mut reading = Reading::new(standing, &reply);
        loop {
            match reading.step() {
                Step::Read(feeds) => {
                    let found = gather::check_feeds(feeds, down);
                    reading.take_found(&found);
                }
                Step::Done(asked_count, changes) => {
                    log!("{}", request::to_read_line(asked_count, changes.len()));
                    return Ok((asked_count, changes));
                }
            }
        }
    }
}

impl Worker for Taker {
    type State = request::Asker;
    type Stay = Hold;
    const NAME: &str = "rundowns";
    const THREADS: [&str; 2] = ["rundowns", "rundowns asked"];
    const FILE: &CStr = c"rundowns";
    const CHORE: Option<ChoreRunner<Self>> = Some(|shared, standing| shared.board.lock().set_standing(standing));
    const DOWN: fn(&Hold) -> &NetworkDown = Hold::down;

    fn stay(&self, shared: &Shared<Self>, _: bool) -> Result<Hold, Fault> {
        shared.account.hold()
    }

    fn turn(&self, shared: &Shared<Self>, hold: &mut Hold, queries: Vec<Query>) {
        for query in queries {
            let changes = self.read(shared, query, hold.down());
            shared.tell(|state| state.take_changes(changes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::news::request::Standing;
    use r1_funcore::domain::voice::Bid;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn typed_request_no_key_fails() {
        let radio = without_networks();
        let curator = Curator::start(Account::new(Data::unusable(), radio.online()), Data::unusable()).unwrap();
        let mut epoll = Epoll::with_fd(curator.fd());
        assert!(curator.is_off());
        curator.want(true);
        curator.ask(vec![Bid::Job(Standing::default()), Bid::Text("a rundown about Formula 1".into())]);
        let mut phase = Phase::Idle;
        while phase == Phase::Idle {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the screen is told");
            phase = curator.latest().phase;
        }
        while !curator.is_off() {
            thread::yield_now();
        }
        let report = curator.latest();
        assert_eq!(
            (report.phase, report.changes.len()),
            (Phase::Failed(Cause::NoAnswer), 0),
            "the key cannot be read: the network is not held, nothing is asked and no feed is read"
        );
        curator.want(false);
        assert!(curator.is_off());
    }
}
