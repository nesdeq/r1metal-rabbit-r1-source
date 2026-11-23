//! A voice job: what its threads and the main loop share.

use super::cause::Cause;
use crate::base::time::NANOS_PER_SECOND;
use crate::openai::fault::Fault;
use crate::storage::SaveFault;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::time::Duration;

const NETWORK_HOLD_NS: u64 = 15 * NANOS_PER_SECOND;

const fn is_stay_usable(is_held_keyed: bool, is_keyed: bool) -> bool {
    is_held_keyed || !is_keyed
}

// True where a stay serves work that needs the key where `is_keyed`, and still stands.
const fn is_stay_kept(is_held_keyed: bool, is_keyed: bool, is_standing: bool) -> bool {
    is_stay_usable(is_held_keyed, is_keyed) && is_standing
}

/// The log line of a recording that ended, `bytes` its sound where enough was heard to write it down.
pub fn to_recorded_line(name: &str, bytes: Option<usize>) -> String {
    match bytes {
        Some(bytes) => format!("{name}: recorded, {bytes} bytes of sound"),
        None => format!("{name}: too little was heard to be written down"),
    }
}

/// How a job of the voice stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Connecting,
    Waking,
    Listening,
    Writing,
    Thinking,
    Searching,
    Speaking,
    Failed(Cause),
}

impl Phase {
    pub(crate) const fn is_working(self) -> bool {
        matches!(self, Phase::Waking | Phase::Writing | Phase::Thinking | Phase::Searching | Phase::Speaking)
    }

    pub(crate) const fn is_session_up(self) -> bool {
        matches!(self, Phase::Listening | Phase::Speaking)
    }

    /// The phase of a session that ended: letting go of it is no failure.
    pub const fn from_ending(cause: Option<Cause>, is_wanted: bool) -> Phase {
        match cause {
            Some(cause) if is_wanted => Phase::Failed(cause),
            _ => Phase::Idle,
        }
    }
}

/// What a job is asked that takes it said or typed: the recording with its seconds, or the words.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Query {
    Voice { sound: Vec<u8>, seconds: u64 },
    Text(String),
}

/// What a screen's button and keys ask, `C` what else the screen asks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Bid<C = Infallible> {
    Listen,
    // The button went down at home: the recording is the job's once the press is held.
    Overhear,
    Drop,
    Send,
    Text(String),
    Job(C),
}

/// What the listening thread does for a bid.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action<C> {
    Record,
    Overhear,
    Claim,
    Discard,
    Send,
    Text(String),
    Chore(C),
    Nothing,
}

/// The action for a bid, the job `is_wanted` and a recording of the thread's own running where `has_memo`.
pub fn to_action<C>(bid: Bid<C>, is_wanted: bool, has_memo: bool) -> Action<C> {
    match bid {
        Bid::Listen if is_wanted && !has_memo => Action::Record,
        Bid::Overhear if is_wanted && !has_memo => Action::Overhear,
        Bid::Listen if has_memo => Action::Claim,
        Bid::Listen | Bid::Overhear => Action::Nothing,
        Bid::Drop => Action::Discard,
        Bid::Send => Action::Send,
        Bid::Text(text) => Action::Text(text),
        Bid::Job(chore) => Action::Chore(chore),
    }
}

#[derive(PartialEq, Eq, Debug)]
pub(super) enum Pickup<I> {
    Work(Vec<I>, bool),
    End,
    Recording(Option<u64>),
}

/// What the turn thread has of the network as it asks for its next step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hold {
    Nothing,
    // A stay: whether it goes through the key, and whether it stands.
    Stay { is_keyed: bool, is_standing: bool },
    // The stay just asked for was refused, and nothing was waited for since.
    Refused,
}

/// The turn thread's next step.
#[derive(PartialEq, Eq, Debug)]
pub enum Step<I> {
    // A turn's inputs, whether the network is held through the key for it, and whether the stay held serves it: where not, a new one is taken first.
    Work { inputs: Vec<I>, needs_key: bool, is_stay_kept: bool },
    // The turn fails by the refusal of the stay just asked for: the network is not asked for again at once.
    Fail,
    End,
    // The stay serves the recording no more: true where the new one goes through the key.
    Renew(bool),
    Wait(Duration),
    Release,
    Sleep,
}

/// What a job keeps of its inputs: the newest alone while wanted, or all of them through a sleep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Keep {
    Newest,
    All,
}

/// What of a voice job is its own.
pub trait Job {
    type Input;
    type Chore;
    type Report;
    const KEEP: Keep;
    // Every turn needs the key, one of typed text too.
    const IS_KEYED: bool = false;
    // The input of a turn for what was said or typed; none where the job keeps a recording itself or takes no text.
    fn take_query(&mut self, query: Query) -> Option<Self::Input>;
    // True where the speech model is asked for a recording's words.
    fn is_written_down(&self) -> bool {
        true
    }
    // Stops what the job sounds; true where something sounded.
    fn hush(&mut self) -> bool {
        false
    }
    // True while nothing of the job's own blocks a sleep.
    fn is_idle(&self) -> bool {
        true
    }
    fn report(&mut self, phase: Phase) -> Self::Report;
}

/// What a job's threads and the main loop share.
pub struct Voice<J: Job> {
    bids: VecDeque<Bid<J::Chore>>,
    pub(super) is_wanted: bool,
    is_recording: bool,
    recording_since_ns: u64,
    is_claimed: bool,
    inputs: VecDeque<J::Input>,
    has_recording_waiting: bool,
    // The turn thread is at a turn.
    pub(super) is_working: bool,
    pub(super) phase: Phase,
    pub job: J,
}

impl<J: Job + Default> Voice<J> {
    pub fn new(is_wanted: bool) -> Voice<J> {
        Voice {
            bids: VecDeque::new(),
            is_wanted,
            is_recording: false,
            recording_since_ns: 0,
            is_claimed: false,
            inputs: VecDeque::new(),
            has_recording_waiting: false,
            is_working: false,
            phase: Phase::Idle,
            job: J::default(),
        }
    }
}

impl<J: Job> Voice<J> {
    /// Takes the screen's bids; true where there are any.
    pub fn ask(&mut self, bids: impl IntoIterator<Item = Bid<J::Chore>>) -> bool {
        let count = self.bids.len();
        self.bids.extend(bids);
        self.bids.len() > count
    }

    /// The bids for the listening thread, with whether the job is wanted; while `is_taking` an unwanted job ends.
    pub fn take_bids(&mut self, is_taking: bool) -> Option<(Vec<Bid<J::Chore>>, bool)> {
        let has_ended = !self.is_wanted && is_taking;
        if self.bids.is_empty() && !has_ended {
            return None;
        }
        Some((self.bids.drain(..).collect(), self.is_wanted))
    }

    /// The bid that ends a recording of a job no longer wanted: sent where the job keeps it through a sleep, else dropped.
    pub const fn to_ending(&self, is_wanted: bool) -> Option<Bid<J::Chore>> {
        if is_wanted {
            None
        } else if self.sends_on_end() {
            Some(Bid::Send)
        } else {
            Some(Bid::Drop)
        }
    }

    fn clear_inputs(&mut self) {
        self.inputs.clear();
        self.has_recording_waiting = false;
    }

    /// Takes whether the job is wanted; true where it changed. Unwanted, what it sounds stops.
    pub fn set_wanted(&mut self, is_wanted: bool) -> bool {
        let is_new = self.is_wanted != is_wanted;
        self.is_wanted = is_wanted;
        if is_new && !is_wanted {
            self.job.hush();
            if J::KEEP == Keep::Newest {
                self.clear_inputs();
            }
        }
        is_new
    }

    // True while a recording runs for the job.
    const fn is_listening(&self) -> bool {
        self.is_recording && self.is_claimed
    }

    /// True while a recording runs whose words the speech model will be asked for: the connection they go over is made ahead.
    pub fn should_connect_ahead(&self) -> bool {
        self.is_listening() && self.job.is_written_down()
    }

    /// A recording begins; for the job where `is_claimed`, else once a press at home is held.
    pub fn begin_recording(&mut self, is_claimed: bool, now_ns: u64) {
        self.is_recording = true;
        self.recording_since_ns = now_ns;
        self.is_claimed = is_claimed;
    }

    /// Stops what the job sounds, as a recording for it begins; true where something sounded.
    pub fn hush(&mut self) -> bool {
        self.job.hush()
    }

    /// The button went down for the job during a recording.
    pub fn claim_recording(&mut self) {
        self.job.hush();
        self.is_claimed |= self.is_recording;
    }

    const fn sends_on_end(&self) -> bool {
        self.is_claimed && matches!(J::KEEP, Keep::All)
    }

    pub fn end_recording(&mut self) {
        self.is_recording = false;
        self.is_claimed = false;
    }

    /// Takes a recording that ended, its file with its seconds where it holds enough to be written down.
    pub fn take_recording(&mut self, sound: Option<(Vec<u8>, u64)>) {
        self.end_recording();
        let Some((sound, seconds)) = sound else {
            self.phase = Phase::Failed(Cause::NotHeard);
            return;
        };
        let Some(input) = self.job.take_query(Query::Voice { sound, seconds }) else { return };
        self.put(input);
        self.has_recording_waiting = self.has_inputs();
    }

    pub fn take_text(&mut self, text: String) {
        self.job.hush();
        if let Some(input) = self.job.take_query(Query::Text(text)) {
            self.put(input);
        }
    }

    pub(super) fn put(&mut self, input: J::Input) {
        if J::KEEP == Keep::Newest {
            if !self.is_wanted {
                return;
            }
            self.clear_inputs();
        }
        self.inputs.push_back(input);
    }

    pub fn has_inputs(&self) -> bool {
        !self.inputs.is_empty()
    }

    fn has_work(&self) -> bool {
        self.is_wanted && (self.is_listening() || self.has_inputs())
    }

    pub(super) const fn is_keyed(&self) -> bool {
        J::IS_KEYED || self.is_listening() || self.has_recording_waiting
    }

    /// Whether the network is held through the key for the turn thread's work; none while it has none.
    pub fn work(&self) -> Option<bool> {
        self.has_work().then(|| self.is_keyed())
    }

    /// The turn thread's next step at `now_ns`, by what it has of the network.
    pub fn next_step(&mut self, now_ns: u64, hold: Hold) -> Step<J::Input> {
        let is_keyed = self.is_keyed();
        let has_stay = matches!(hold, Hold::Stay { .. });
        let serves = |needs_key: bool| match hold {
            Hold::Stay { is_keyed, is_standing } => is_stay_kept(is_keyed, needs_key, is_standing),
            Hold::Nothing | Hold::Refused => false,
        };
        match self.pick_up(now_ns) {
            Pickup::Work(..) if hold == Hold::Refused => Step::Fail,
            Pickup::Work(inputs, needs_key) => Step::Work { inputs, needs_key, is_stay_kept: serves(needs_key) },
            Pickup::End => Step::End,
            Pickup::Recording(Some(_)) if has_stay && !serves(is_keyed) => Step::Renew(is_keyed),
            Pickup::Recording(Some(remaining_ns)) => Step::Wait(Duration::from_nanos(remaining_ns)),
            Pickup::Recording(None) if has_stay => Step::Release,
            Pickup::Recording(None) => Step::Sleep,
        }
    }

    pub(super) fn pick_up(&mut self, now_ns: u64) -> Pickup<J::Input> {
        if self.is_wanted && self.has_inputs() {
            self.is_working = true;
            let is_keyed = core::mem::take(&mut self.has_recording_waiting) || J::IS_KEYED;
            return Pickup::Work(self.inputs.drain(..).collect(), is_keyed);
        }
        if !self.is_wanted || !self.is_recording {
            return Pickup::End;
        }
        let remaining_ns = (self.recording_since_ns + NETWORK_HOLD_NS).saturating_sub(now_ns);
        Pickup::Recording((remaining_ns > 0).then_some(remaining_ns))
    }

    /// The turn thread's turn is over.
    pub const fn end_turn(&mut self) {
        self.is_working = false;
    }

    /// Takes the turn thread's phase; true where it changed.
    pub fn set_phase(&mut self, phase: Phase) -> bool {
        core::mem::replace(&mut self.phase, phase) != phase
    }

    /// The turn failed; true where that is news.
    pub fn fail(&mut self, fault: &Fault) -> bool {
        self.set_phase(Phase::Failed(Cause::from_fault(fault, Cause::NoAnswer)))
    }

    /// Nothing could be saved or recorded; true where that is news.
    pub fn fail_saving(&mut self, fault: SaveFault) -> bool {
        self.set_phase(Phase::Failed(Cause::from_save_fault(fault)))
    }

    fn report_phase(&self) -> Phase {
        if self.is_listening() {
            Phase::Listening
        } else if self.has_recording_waiting {
            Phase::Writing
        } else {
            self.phase
        }
    }

    pub fn take_report(&mut self) -> J::Report {
        let phase = self.report_phase();
        self.job.report(phase)
    }

    /// True while nothing of the job blocks a sleep.
    pub fn is_off(&self) -> bool {
        let is_clear = J::KEEP == Keep::All || self.inputs.is_empty();
        !self.is_recording && !self.is_working && self.bids.is_empty() && is_clear && self.job.is_idle()
    }
}

/// What a job that reads requests into changes reports to the main loop.
pub struct Changes<C> {
    pub phase: Phase,
    /// What a request came to, which the interface takes.
    pub changes: Vec<C>,
}

/// A job that reads a request said or typed into changes of what the interface holds: `S` what a request is read against, as the screen hands it over, `C` a change, kept until the main loop takes it.
pub struct Request<S, C> {
    standing: S,
    changes: Vec<C>,
}

impl<S: Default, C> Default for Request<S, C> {
    fn default() -> Request<S, C> {
        Request { standing: S::default(), changes: Vec::new() }
    }
}

impl<S, C> Job for Request<S, C> {
    type Input = Query;
    type Chore = S;
    type Report = Changes<C>;
    const KEEP: Keep = Keep::Newest;
    const IS_KEYED: bool = true;

    fn take_query(&mut self, query: Query) -> Option<Query> {
        Some(query)
    }

    fn report(&mut self, phase: Phase) -> Changes<C> {
        Changes { phase, changes: core::mem::take(&mut self.changes) }
    }
}

impl<S, C> Voice<Request<S, C>> {
    /// Takes what requests are read against, as the interface holds it.
    pub fn set_standing(&mut self, standing: S) {
        self.job.standing = standing;
    }

    /// What a request is read against.
    pub const fn standing(&self) -> &S {
        &self.job.standing
    }

    // Keeps the changes a request came to for the main loop, or fails as `cause` where it came to none; true if the screen has news.
    pub(super) fn keep_changes(&mut self, changes: Vec<C>, cause: Cause) -> bool {
        if changes.is_empty() {
            return self.set_phase(Phase::Failed(cause));
        }
        self.job.changes.extend(changes);
        self.phase = Phase::Idle;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::refusal::Refusal;
    use alloc::vec;

    #[derive(Default)]
    struct Seconds;

    impl Job for Seconds {
        type Input = u64;
        type Chore = Infallible;
        type Report = Phase;
        const KEEP: Keep = Keep::Newest;

        fn take_query(&mut self, query: Query) -> Option<u64> {
            Some(match query {
                Query::Voice { seconds, .. } => seconds,
                Query::Text(text) => text.len() as u64,
            })
        }

        fn report(&mut self, phase: Phase) -> Phase {
            phase
        }
    }

    #[derive(Default)]
    struct Everything;

    impl Job for Everything {
        type Input = u64;
        type Chore = Infallible;
        type Report = ();
        const KEEP: Keep = Keep::All;
        const IS_KEYED: bool = true;

        fn take_query(&mut self, query: Query) -> Option<u64> {
            Some(match query {
                Query::Voice { seconds, .. } => seconds,
                Query::Text(_) => 0,
            })
        }

        fn report(&mut self, _: Phase) {}
    }

    #[test]
    fn newest_input_while_wanted() {
        let mut voice = Voice::<Seconds>::new(false);
        voice.put(1);
        assert!(!voice.has_work() && voice.pick_up(0) == Pickup::End);
        assert!(voice.set_wanted(true) && !voice.set_wanted(true));
        voice.put(1);
        voice.put(2);
        assert!(voice.has_work() && !voice.is_off());
        assert_eq!(voice.pick_up(0), Pickup::Work(vec![2], false));
        assert!(!voice.is_off() && voice.pick_up(0) == Pickup::End);
        voice.end_turn();
        assert!(voice.is_off());
        voice.put(3);
        assert!(voice.set_wanted(false) && !voice.has_work());
        assert!(voice.set_wanted(true) && voice.pick_up(0) == Pickup::End);
    }

    #[test]
    fn recording_ends_when_unwanted() {
        let mut voice = Voice::<Seconds>::new(true);
        voice.begin_recording(true, 0);
        assert!(voice.has_work() && matches!(voice.pick_up(0), Pickup::Recording(_)));
        assert_eq!((voice.take_report(), voice.is_off()), (Phase::Listening, false));
        assert!(voice.take_bids(true).is_none() && !voice.ask([]));
        assert!(voice.ask([Bid::Send]));
        assert_eq!(voice.take_bids(true), Some((vec![Bid::Send], true)));
        voice.set_wanted(false);
        assert_eq!(voice.take_bids(true), Some((Vec::new(), false)));
        assert!(voice.take_bids(false).is_none());
        assert!(!voice.sends_on_end());
        voice.end_recording();
        assert!(voice.is_off());
        assert!(voice.set_phase(Phase::Failed(Cause::NoNetwork)) && !voice.set_phase(Phase::Failed(Cause::NoNetwork)));
        assert_eq!(voice.take_report(), Phase::Failed(Cause::NoNetwork));
        assert!(voice.fail_saving(SaveFault::Full) && !voice.fail_saving(SaveFault::Full));
        assert_eq!(voice.take_report(), Phase::Failed(Cause::StorageFull));
    }

    #[test]
    fn recording_holds_network() {
        let mut voice = Voice::<Seconds>::new(true);
        assert!(!voice.should_connect_ahead(), "nothing is recorded yet");
        voice.begin_recording(false, 7);
        assert!(!voice.should_connect_ahead(), "a press at home that is no hold yet is no recording of the job's");
        voice.begin_recording(true, 7);
        assert!(voice.is_keyed() && voice.should_connect_ahead());
        assert_eq!(voice.pick_up(7), Pickup::Recording(Some(NETWORK_HOLD_NS)));
        assert_eq!(voice.pick_up(6 + NETWORK_HOLD_NS), Pickup::Recording(Some(1)));
        assert_eq!(voice.pick_up(7 + NETWORK_HOLD_NS), Pickup::Recording(None));
        voice.take_recording(Some((vec![1], 20)));
        assert_eq!(voice.pick_up(30 * NANOS_PER_SECOND), Pickup::Work(vec![20], true));
    }

    #[test]
    fn stay_without_key() {
        assert!(
            is_stay_usable(true, true)
                && is_stay_usable(true, false)
                && is_stay_usable(false, false)
                && !is_stay_usable(false, true)
        );
        let mut typed = Voice::<Everything>::new(true);
        typed.take_text("Hallo".into());
        assert!(typed.is_keyed() && matches!(typed.pick_up(0), Pickup::Work(_, true)));
        let mut name = Voice::<Seconds>::new(true);
        name.take_text("mew".into());
        assert!(!name.is_keyed());
    }

    #[test]
    fn sleep_sends_or_drops() {
        let mut all = Voice::<Everything>::new(true);
        all.begin_recording(false, 0);
        assert!(!all.has_work() && !all.sends_on_end());
        all.claim_recording();
        assert!(all.has_work() && matches!(all.pick_up(0), Pickup::Recording(_)));
        assert!(all.set_wanted(false) && all.sends_on_end());
        all.take_recording(Some((vec![1], 1)));
        assert!(all.has_inputs() && all.is_off());
        assert!(matches!(all.pick_up(0), Pickup::End) && all.set_wanted(true));
        assert!(matches!(all.pick_up(0), Pickup::Work(_, true)));
        let mut newest = Voice::<Seconds>::new(true);
        newest.begin_recording(true, 0);
        assert!(newest.set_wanted(false) && !newest.sends_on_end());
    }

    #[test]
    fn reported_phases() {
        let mut voice = Voice::<Seconds>::new(true);
        voice.begin_recording(true, 0);
        voice.take_recording(Some((vec![1], 1)));
        assert_eq!(voice.report_phase(), Phase::Writing);
        let _ = voice.pick_up(0);
        voice.begin_recording(true, 0);
        voice.take_recording(None);
        assert_eq!(voice.report_phase(), Phase::Failed(Cause::NotHeard));
        let refusal = Refusal { code: "x".into(), message: "No.".into() };
        voice.fail(&Fault::from_refusal(Some(503), &refusal));
        assert_eq!(voice.report_phase(), Phase::Failed(Cause::NoAnswer));
        voice.fail(&Fault::from_refusal(Some(400), &refusal));
        assert_eq!(voice.report_phase(), Phase::Failed(Cause::Refused));
    }

    #[test]
    fn text_replaces_recording() {
        let mut voice = Voice::<Seconds>::new(true);
        voice.begin_recording(true, 0);
        voice.take_recording(Some((vec![1], 1)));
        voice.take_text("mew".into());
        assert_eq!(voice.report_phase(), Phase::Idle);
        assert_eq!(voice.pick_up(0), Pickup::Work(vec![3], false));
        voice.begin_recording(true, 0);
        voice.take_recording(Some((vec![1], 1)));
        assert!(voice.set_wanted(false) && voice.report_phase() == Phase::Idle);
    }

    #[test]
    fn bid_actions() {
        let action = |bid: Bid, is_wanted: bool, has_memo: bool| to_action(bid, is_wanted, has_memo);
        assert_eq!(action(Bid::Listen, true, false), Action::Record);
        assert_eq!(action(Bid::Overhear, true, false), Action::Overhear);
        assert_eq!(action(Bid::Listen, true, true), Action::Claim);
        assert_eq!(action(Bid::Listen, false, true), Action::Claim);
        assert_eq!(action(Bid::Overhear, true, true), Action::Nothing);
        assert_eq!(action(Bid::Listen, false, false), Action::Nothing);
        assert_eq!(action(Bid::Overhear, false, false), Action::Nothing);
        assert_eq!(action(Bid::Drop, true, false), Action::Discard);
        assert_eq!(action(Bid::Send, false, true), Action::Send);
        assert_eq!(action(Bid::Text("a".into()), false, false), Action::Text("a".into()));
    }

    #[test]
    fn unwanted_job_ends_by_keep() {
        let mut all = Voice::<Everything>::new(true);
        all.begin_recording(true, 0);
        assert_eq!((all.to_ending(true), all.to_ending(false)), (None, Some(Bid::Send)));
        let mut newest = Voice::<Seconds>::new(true);
        newest.begin_recording(true, 0);
        assert_eq!(newest.to_ending(false), Some(Bid::Drop));
    }

    #[test]
    fn next_step_keeps_usable_stay() {
        let stay = |is_keyed, is_standing| Hold::Stay { is_keyed, is_standing };
        let mut voice = Voice::<Seconds>::new(true);
        voice.begin_recording(true, 0);
        let wait = Step::Wait(Duration::from_nanos(NETWORK_HOLD_NS));
        assert_eq!(voice.next_step(0, Hold::Nothing), wait);
        assert_eq!(voice.next_step(0, stay(true, true)), wait);
        assert_eq!(voice.next_step(0, Hold::Refused), wait, "a refused stay is not asked for again while it records");
        assert_eq!(voice.next_step(0, stay(false, true)), Step::Renew(true));
        assert_eq!(voice.next_step(0, stay(true, false)), Step::Renew(true));
        assert_eq!(voice.next_step(NETWORK_HOLD_NS, stay(false, false)), Step::Release);
        assert_eq!(voice.next_step(NETWORK_HOLD_NS, Hold::Nothing), Step::Sleep);
        assert_eq!(voice.next_step(NETWORK_HOLD_NS, Hold::Refused), Step::Sleep);
        assert_eq!(voice.work(), Some(true));
        voice.end_recording();
        assert_eq!((voice.work(), voice.next_step(0, Hold::Nothing)), (None, Step::End));
        assert_eq!(voice.next_step(0, Hold::Refused), Step::End);
        voice.put(1);
        assert_eq!(voice.work(), Some(false));
        let work = |is_stay_kept| Step::Work { inputs: vec![1], needs_key: false, is_stay_kept };
        assert_eq!(
            voice.next_step(0, stay(false, true)),
            work(true),
            "a stay that stands serves a turn without the key"
        );
        voice.end_turn();
        voice.put(1);
        assert_eq!(voice.next_step(0, stay(false, false)), work(false), "one that ended is taken anew");
        voice.end_turn();
        voice.put(1);
        assert_eq!(voice.next_step(0, Hold::Nothing), work(false), "and so is one for a turn that holds none");
        voice.end_turn();
        voice.put(1);
        assert_eq!(
            voice.next_step(0, Hold::Refused),
            Step::Fail,
            "a turn whose stay was just refused asks for no other"
        );
        assert!(!voice.has_inputs() && !voice.is_off(), "its inputs go with it, and its turn is to be ended");
        voice.end_turn();
        assert!(voice.is_off());
        let mut keyed = Voice::<Everything>::new(true);
        keyed.take_text("Hallo".into());
        assert_eq!(
            keyed.next_step(0, stay(false, true)),
            Step::Work { inputs: vec![0], needs_key: true, is_stay_kept: false },
            "a stay without the key serves no turn that needs it"
        );
        assert!(is_stay_kept(true, true, true) && !is_stay_kept(false, true, true) && !is_stay_kept(true, true, false));
    }

    #[test]
    fn request_changes() {
        let mut state = Voice::<Request<u8, &str>>::new(true);
        assert!(state.is_keyed(), "a request typed needs the key as one said does");
        state.set_standing(7);
        assert_eq!(*state.standing(), 7);
        state.take_text("a request".into());
        assert_eq!(state.pick_up(0), Pickup::Work(vec![Query::Text("a request".into())], true));
        assert!(state.keep_changes(vec!["one", "two"], Cause::NoAnswer));
        let report = state.take_report();
        assert_eq!((report.phase, report.changes), (Phase::Idle, vec!["one", "two"]));
        assert!(state.take_report().changes.is_empty(), "told once");
        assert!(state.keep_changes(Vec::new(), Cause::NoFeed) && !state.keep_changes(Vec::new(), Cause::NoFeed));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoFeed), "a request that came to no change");
    }

    #[test]
    fn recorded_line() {
        assert_eq!(to_recorded_line("chat", Some(9)), "chat: recorded, 9 bytes of sound");
        assert_eq!(to_recorded_line("chat", None), "chat: too little was heard to be written down");
    }

    #[test]
    fn phase_predicates() {
        assert!(Phase::Speaking.is_working() && !Phase::Listening.is_working() && Phase::Listening.is_session_up());
        assert_eq!(Phase::from_ending(Some(Cause::NoNetwork), true), Phase::Failed(Cause::NoNetwork));
        assert_eq!(Phase::from_ending(Some(Cause::NoNetwork), false), Phase::Idle);
        assert_eq!(Phase::from_ending(None, true), Phase::Idle);
    }
}
