use crate::log;
use crate::openai::{Account, write};
use crate::sys;
use crate::voiced::{ChoreRunner, Part, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::domain::bell::state::{self, PROMPT};
use r1_funcore::domain::voice::{Phase, Query};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use std::ffi::CStr;
use std::io;

/// The worker of the job that takes a request said or typed to the bell.
pub struct Taker;

/// The main loop's handle of that job.
pub type Setter = Part<Taker>;

impl Setter {
    /// Starts the threads, idle until the bell's list shows.
    pub fn start(account: Account) -> io::Result<Setter> {
        Part::spawn(Taker, false, account)
    }
}

// Writes a request said down, and has the language model read it into the calls that change the bell.
fn read(shared: &Shared<Taker>, query: Query, down: &NetworkDown) -> Result<String, Fault> {
    let subject = (Subject::BellRequest, PROMPT);
    let request = shared.write_down(query, down, subject, "bell: the request said is not written down", None)?;
    shared.set_phase(Phase::Thinking);
    let input = shared.board.lock().to_input(&request, sys::realtime_seconds());
    write::write_bell_calls(&shared.account, &mut None, &input)
        .inspect_err(|fault| log!("bell: the request is not read: {fault}"))
}

impl Worker for Taker {
    type State = state::Setter;
    type Stay = Hold;
    const NAME: &str = "bell";
    const THREADS: [&str; 2] = ["bell", "bell request"];
    const FILE: &CStr = c"bell";
    const CHORE: Option<ChoreRunner<Self>> = Some(|shared, standing| shared.board.lock().set_standing(standing));
    const DOWN: fn(&Hold) -> &NetworkDown = Hold::down;

    fn stay(&self, shared: &Shared<Self>, _: bool) -> Result<Hold, Fault> {
        shared.account.hold()
    }

    fn turn(&self, shared: &Shared<Self>, hold: &mut Hold, queries: Vec<Query>) {
        for query in queries {
            let reply = read(shared, query, hold.down());
            shared.tell(|state| state.take_reply(reply));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::bell::Bell;
    use r1_funcore::domain::bell::state::Standing;
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::voice::Bid;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn typed_request_no_key_fails() {
        let radio = without_networks();
        let setter = Setter::start(Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(setter.fd());
        assert!(setter.is_off());
        setter.want(true);
        setter.ask(vec![Bid::Job(Standing(Bell::default(), 0)), Bid::Text("at seven".into())]);
        let mut phase = Phase::Idle;
        while phase == Phase::Idle {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the screen is told");
            phase = setter.latest().phase;
        }
        while !setter.is_off() {
            thread::yield_now();
        }
        assert_eq!(
            setter.latest().phase,
            Phase::Failed(Cause::NoAnswer),
            "the key cannot be read: the network is not held, and nothing is asked"
        );
        setter.want(false);
        assert!(setter.is_off());
    }
}
