use crate::log;
use crate::openai::{self, Account};
use crate::sound::Memo;
use crate::sync::Board;
use crate::sys;
use crate::wifi::radio::NetworkDown;
use r1_funcore::domain::voice::{self, Action, Bid, Hold, Phase, Query, Step, Voice};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::{self, Subject};
use r1_funcore::storage::SaveFault;
use std::ffi::CStr;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;

/// A job said or typed, as its two threads drive it.
pub trait Worker: Send + Sync + Sized + 'static {
    type State: voice::Job<Input: Send, Chore: Send> + Send;
    type Stay;
    const NAME: &str;
    const THREADS: [&str; 2];
    const FILE: &CStr;
    // Why nothing can be recorded (true) or typed now (false), for a job that may have a reason.
    const READY_CHECK: Option<ReadyCheck<Self>> = None;
    const CHORE: Option<ChoreRunner<Self>> = None;
    // The end of the stay in the network a stay of the job's stands in.
    const DOWN: fn(&Self::Stay) -> &NetworkDown;
    fn stay(&self, shared: &Shared<Self>, needs_key: bool) -> Result<Self::Stay, Fault>;
    fn turn(&self, shared: &Shared<Self>, stay: &mut Self::Stay, inputs: Vec<Input<Self>>);
}

type Chore<J> = <<J as Worker>::State as voice::Job>::Chore;
pub type ChoreRunner<J> = fn(&Shared<J>, Chore<J>);
pub type ReadyCheck<J> = fn(&Shared<J>, bool) -> Option<SaveFault>;
type Input<J> = <<J as Worker>::State as voice::Job>::Input;

/// What a job's threads and the main loop share.
pub struct Shared<J: Worker> {
    /// Notified for the threads, raised for the screen.
    pub board: Board<Voice<J::State>>,
    pub account: Account,
    pub job: J,
}

impl<J: Worker> Shared<J> {
    /// Wakes the threads and tells the main loop.
    pub fn wake_all(&self) {
        self.board.notify();
        self.board.raise();
    }

    /// Has the state take something under its lock, and tells the main loop where that is news.
    pub fn tell(&self, take: impl FnOnce(&mut Voice<J::State>) -> bool) {
        if take(&mut self.board.lock()) {
            self.board.raise();
        }
    }

    /// Tells the screen what the turn thread is at.
    pub fn set_phase(&self, phase: Phase) {
        self.tell(|state| state.set_phase(phase));
    }

    /// Tells the screen why a turn came to nothing.
    pub fn fail(&self, fault: &Fault) {
        self.tell(|state| state.fail(fault));
    }

    /// The words a job is asked in: those typed, or those the speech model writes down of what was said, as `subject` under its prompt, handed to `on_words` as they come where it is given; `unwritten` begins the log line of a recording that is not written down.
    pub fn write_down(
        &self,
        query: Query,
        down: &NetworkDown,
        subject: (Subject, &str),
        unwritten: &str,
        on_words: Option<&mut dyn FnMut(&str)>,
    ) -> Result<String, Fault> {
        match query {
            Query::Text(words) => Ok(words),
            Query::Voice { sound, seconds } => {
                self.set_phase(Phase::Writing);
                openai::transcript::transcribe(&self.account, down, (&sound, seconds), subject, on_words)
                    .inspect_err(|fault| log!("{unwritten}: {fault}"))
            }
        }
    }

    /// Tells the screen why nothing could be saved or recorded.
    pub fn fail_saving(&self, fault: SaveFault) {
        self.tell(|state| state.fail_saving(fault));
    }

    fn begin_recording(&self, is_claimed: bool) -> Result<Memo, SaveFault> {
        if let Some(fault) = J::READY_CHECK.and_then(|check| check(self, true)) {
            return Err(fault);
        }
        let to_fault = |e: io::Error| {
            log!("{}: no recording: {e}", J::NAME);
            SaveFault::NotRecorded
        };
        let files = Memo::files(J::FILE).map_err(to_fault)?;
        self.board.lock().begin_recording(is_claimed, sys::monotonic_ns());
        match Memo::record(files) {
            Ok(memo) => {
                self.wake_all();
                Ok(memo)
            }
            Err(e) => {
                self.board.lock().end_recording();
                Err(to_fault(e))
            }
        }
    }

    fn discard(&self, memo: Memo) {
        if let Err(e) = memo.discard() {
            log!("{}: a recording that was let go: {e}", J::NAME);
        }
        self.board.lock().end_recording();
        self.wake_all();
    }

    fn send(&self, memo: Memo) {
        let result = memo.finish(transcript::recording_seconds);
        let mut state = self.board.lock();
        match result {
            Ok(recording) => {
                log!("{}", voice::to_recorded_line(J::NAME, recording.as_ref().map(|(sound, _)| sound.len())));
                state.take_recording(recording);
            }
            Err(e) => {
                log!("{}: the recording: {e}", J::NAME);
                state.end_recording();
                state.fail_saving(SaveFault::NotRecorded);
            }
        }
        drop(state);
        self.wake_all();
    }

    fn take_bid(&self, bid: Bid<Chore<J>>, is_wanted: bool, memo: &mut Option<Memo>) {
        match voice::to_action(bid, is_wanted, memo.is_some()) {
            Action::Record => {
                self.tell(Voice::hush);
                match self.begin_recording(true) {
                    Ok(recording) => *memo = Some(recording),
                    Err(fault) => self.fail_saving(fault),
                }
            }
            // A press at home that fails to record says nothing: only a hold tells why, as it asks again.
            Action::Overhear => *memo = self.begin_recording(false).ok(),
            Action::Claim => {
                self.board.lock().claim_recording();
                self.wake_all();
            }
            Action::Nothing => {}
            Action::Discard => {
                if let Some(memo) = memo.take() {
                    self.discard(memo);
                }
            }
            Action::Send => {
                if let Some(memo) = memo.take() {
                    self.send(memo);
                }
            }
            Action::Text(text) => match J::READY_CHECK.and_then(|check| check(self, false)) {
                Some(fault) => self.fail_saving(fault),
                None => {
                    self.board.lock().take_text(text);
                    self.wake_all();
                }
            },
            Action::Chore(chore) => {
                if let Some(run_chore) = J::CHORE {
                    run_chore(self, chore);
                }
            }
        }
    }

    fn end_turn(&self) {
        self.board.lock().end_turn();
        self.wake_all();
    }
}

fn listen<J: Worker>(shared: &Shared<J>) -> ! {
    let mut memo: Option<Memo> = None;
    loop {
        let (bids, is_wanted) = shared.board.wait(|state| state.take_bids(memo.is_some()));
        let ending = shared.board.lock().to_ending(is_wanted);
        for bid in ending.into_iter().chain(bids) {
            shared.take_bid(bid, is_wanted, &mut memo);
        }
    }
}

fn work<J: Worker>(shared: &Shared<J>) -> ! {
    loop {
        let needs_key = shared.board.wait(|state| state.work());
        // From a recording's beginning the network is held, so that what was said goes as soon as it is there; one let go of meanwhile is held again for it.
        let (mut stay, mut refusal) = match take_stay(shared, needs_key) {
            Ok(stay) => (Some((stay, needs_key)), None),
            Err(fault) => (None, Some(fault)),
        };
        while let Some(turn) = take_turn(shared, &mut stay, refusal.take()) {
            let held = turn.and_then(|(inputs, needs_key, is_stay_kept)| {
                let held = match (is_stay_kept, stay.take()) {
                    (true, Some(held)) => Ok(held),
                    (_, ended) => renew_stay(shared, ended, needs_key),
                };
                held.map(|held| (held, inputs))
            });
            match held {
                Ok(((mut current, has_key), inputs)) => {
                    shared.job.turn(shared, &mut current, inputs);
                    stay = Some((current, has_key));
                }
                Err(fault) => shared.fail(&fault),
            }
            shared.end_turn();
        }
    }
}

// Takes a stay for the job, and in it makes the connection ahead that the words of a recording that runs will go over.
fn take_stay<J: Worker>(shared: &Shared<J>, needs_key: bool) -> Result<J::Stay, Fault> {
    let stay = shared.job.stay(shared, needs_key).inspect_err(|fault| log!("{}: {fault}", J::NAME))?;
    if shared.board.lock().should_connect_ahead() {
        openai::transcript::prepare(&shared.account, (J::DOWN)(&stay));
    }
    Ok(stay)
}

// Takes a stay anew; the one that serves no longer goes once the new one is there, so that the device stays in its network.
fn renew_stay<J: Worker>(
    shared: &Shared<J>,
    ended: Option<(J::Stay, bool)>,
    needs_key: bool,
) -> Result<(J::Stay, bool), Fault> {
    let result = take_stay(shared, needs_key);
    drop(ended);
    result.map(|stay| (stay, needs_key))
}

type Turn<J> = (Vec<Input<J>>, bool, bool);

// The next turn: its inputs, whether its stay goes through the key and whether the stay held serves it, or the refusal of the stay just asked for, which it fails by; none once the work is over.
fn take_turn<J: Worker>(
    shared: &Shared<J>,
    stay: &mut Option<(J::Stay, bool)>,
    mut refusal: Option<Fault>,
) -> Option<Result<Turn<J>, Fault>> {
    let mut state = shared.board.lock();
    loop {
        let hold = match (&refusal, stay.as_ref()) {
            (Some(_), _) => Hold::Refused,
            (None, Some((stay, has_key))) => {
                Hold::Stay { is_keyed: *has_key, is_standing: !(J::DOWN)(stay).is_raised() }
            }
            (None, None) => Hold::Nothing,
        };
        let step = state.next_step(sys::monotonic_ns(), hold);
        let refused = refusal.take();
        state = match step {
            Step::Work { inputs, needs_key, is_stay_kept } => return Some(Ok((inputs, needs_key, is_stay_kept))),
            Step::Fail => return refused.map(Err),
            Step::End => return None,
            Step::Renew(needs_key) => {
                drop(state);
                match renew_stay(shared, stay.take(), needs_key) {
                    Ok(renewed) => *stay = Some(renewed),
                    Err(fault) => refusal = Some(fault),
                }
                shared.board.lock()
            }
            Step::Wait(limit) => shared.board.sleep_within(state, limit),
            Step::Release => {
                drop(state);
                *stay = None;
                shared.board.lock()
            }
            Step::Sleep => shared.board.sleep(state),
        };
    }
}

/// The main loop's handle of a job said or typed.
pub struct Part<J: Worker> {
    pub shared: Arc<Shared<J>>,
}

impl<J: Worker> Part<J> {
    /// Starts its two threads, the job wanted from the start or not; `account` has the key, the ledger and the network.
    pub fn spawn(job: J, is_wanted: bool, account: Account) -> io::Result<Part<J>>
    where
        J::State: Default,
    {
        let board = Board::new(Voice::new(is_wanted))?;
        let shared = Arc::new(Shared { board, account, job });
        let (listener, worker) = (Arc::clone(&shared), Arc::clone(&shared));
        sys::spawn_background(J::THREADS[0], move || listen(&listener))?;
        sys::spawn_background(J::THREADS[1], move || work(&worker))?;
        Ok(Part { shared })
    }

    /// Readable when there is something new for the screen.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Hands on what the screen asked, waking the listening thread where it asked anything.
    pub fn ask(&self, bids: Vec<Bid<Chore<J>>>) {
        if self.shared.board.lock().ask(bids) {
            self.shared.board.notify();
        }
    }

    /// Sets whether the job is wanted: it begins only then, and unwanted it ends a recording.
    pub fn want(&self, is_wanted: bool) {
        if self.shared.board.lock().set_wanted(is_wanted) {
            self.shared.wake_all();
        }
    }

    /// Takes the report for the screen.
    pub fn latest(&self) -> <J::State as voice::Job>::Report {
        self.shared.board.take(|state| state.take_report())
    }

    /// True while nothing of the job stands in the way of a sleep.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::lock;
    use crate::wifi::radio::tests::up;
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::pokedex::state::Lookup;
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    type Events = Arc<Mutex<Vec<&'static str>>>;
    // The ends of the stays a job took, in the order it took them.
    type Downs = Arc<Mutex<Vec<NetworkDown>>>;

    struct Token {
        has_key: bool,
        events: Events,
        down: NetworkDown,
    }

    impl Drop for Token {
        fn drop(&mut self) {
            lock(&self.events).push(if self.has_key { "keyed let go" } else { "keyless let go" });
        }
    }

    struct Tokens {
        events: Events,
        is_keyless: bool,
        is_offline: bool,
        gate: Mutex<Receiver<()>>,
        downs: Downs,
    }

    impl Tokens {
        fn new(events: &Events, is_keyless: bool, gate: Receiver<()>) -> Tokens {
            let (events, gate) = (Arc::clone(events), Mutex::new(gate));
            Tokens { events, is_keyless, is_offline: false, gate, downs: Downs::default() }
        }
    }

    impl Worker for Tokens {
        type State = Lookup;
        type Stay = Token;
        const NAME: &str = "tokens";
        const THREADS: [&str; 2] = ["tokens", "tokens turns"];
        const FILE: &CStr = c"tokens";
        const DOWN: fn(&Token) -> &NetworkDown = |token| &token.down;

        fn stay(&self, _: &Shared<Self>, needs_key: bool) -> Result<Token, Fault> {
            if self.is_offline {
                lock(&self.events).push("refused");
                return Err(Fault::Offline("no network".into()));
            }
            if needs_key && self.is_keyless {
                return Err(Fault::NoKey);
            }
            let down = up();
            lock(&self.downs).push(down.clone());
            lock(&self.events).push(if needs_key { "keyed taken" } else { "keyless taken" });
            Ok(Token { has_key: needs_key, events: Arc::clone(&self.events), down })
        }

        fn turn(&self, _: &Shared<Self>, _: &mut Token, _: Vec<Query>) {
            let _ = lock(&self.gate).recv();
        }
    }

    fn wait_events(events: &Events, count: usize) -> Vec<&'static str> {
        let start = Instant::now();
        while lock(events).len() < count {
            assert!(start.elapsed() < Duration::from_secs(10), "the job did {:?} and no more", lock(events));
            std::thread::yield_now();
        }
        lock(events).clone()
    }

    fn text_then_recording(is_keyless: bool, count: usize) -> Vec<&'static str> {
        let (events, (gate, receiver)) = (Events::default(), mpsc::channel());
        let part =
            Part::spawn(Tokens::new(&events, is_keyless, receiver), true, crate::openai::tests::unusable()).unwrap();
        part.ask(vec![Bid::Text("mew".into())]);
        assert_eq!(wait_events(&events, 1), ["keyless taken"], "a name typed holds the network without the key");
        part.shared.board.lock().begin_recording(true, sys::monotonic_ns());
        part.shared.board.notify();
        gate.send(()).unwrap();
        wait_events(&events, count)
    }

    #[test]
    fn keyed_stay_replaces_keyless() {
        assert_eq!(
            text_then_recording(false, 3),
            ["keyless taken", "keyed taken", "keyless let go"],
            "the stay through the key is there before the one without it goes, so the device stays in its network"
        );
    }

    #[test]
    fn no_key_releases_stay_for_recording() {
        assert_eq!(
            text_then_recording(true, 2),
            ["keyless taken", "keyless let go"],
            "nothing is held for a recording that cannot be written down"
        );
    }

    #[test]
    fn refused_stay_asked_once() {
        let (events, (_gate, receiver)) = (Events::default(), mpsc::channel());
        let job = Tokens { is_offline: true, ..Tokens::new(&events, false, receiver) };
        let part = Part::spawn(job, true, crate::openai::tests::unusable()).unwrap();
        part.ask(vec![Bid::Text("mew".into())]);
        wait_events(&events, 1);
        while !part.is_off() {
            std::thread::yield_now();
        }
        assert_eq!(part.latest().phase, Phase::Failed(Cause::NoNetwork), "the name typed fails by the refusal");
        assert_eq!(*lock(&events), ["refused"], "a network just refused is not asked for again for the same name");
    }

    #[test]
    fn ended_stay_renewed_for_turn() {
        let (events, (gate, receiver)) = (Events::default(), mpsc::channel());
        let job = Tokens::new(&events, false, receiver);
        let downs = Arc::clone(&job.downs);
        let part = Part::spawn(job, true, crate::openai::tests::unusable()).unwrap();
        part.ask(vec![Bid::Text("mew".into())]);
        assert_eq!(wait_events(&events, 1), ["keyless taken"]);
        part.ask(vec![Bid::Text("mewtwo".into())]);
        while !part.shared.board.lock().has_inputs() {
            std::thread::yield_now();
        }
        lock(&downs)[0].raise();
        gate.send(()).unwrap();
        assert_eq!(
            wait_events(&events, 3),
            ["keyless taken", "keyless taken", "keyless let go"],
            "the next name's turn is held anew, and the stay that ended goes once the new one is there"
        );
        gate.send(()).unwrap();
    }

    #[test]
    fn ended_stay_renewed_for_recording() {
        let (events, (_gate, receiver)) = (Events::default(), mpsc::channel());
        let job = Tokens::new(&events, false, receiver);
        let downs = Arc::clone(&job.downs);
        let part = Part::spawn(job, true, crate::openai::tests::unusable()).unwrap();
        part.shared.board.lock().begin_recording(true, sys::monotonic_ns());
        part.shared.board.notify();
        assert_eq!(
            wait_events(&events, 1),
            ["keyed taken"],
            "a recording holds the network through the key from its beginning"
        );
        // Under the lock: the turn thread reads this before it sleeps, and a notify between the two would be lost.
        let state = part.shared.board.lock();
        lock(&downs)[0].raise();
        drop(state);
        part.shared.board.notify();
        assert_eq!(
            wait_events(&events, 3),
            ["keyed taken", "keyed taken", "keyed let go"],
            "the stay that ended is taken anew while the recording runs, before it goes"
        );
    }
}
