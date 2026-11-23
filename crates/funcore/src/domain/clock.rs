//! When the clocks are asked of a server, and what setting them did.

use crate::base::handoff::{Orders, Turns};
use crate::domain::cause::Cause;
use crate::net::race::TIMEOUT;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const TOLERANCE_S: u64 = 1;
const CHECK_PERIOD_S: u64 = 3_600;
// The wait after a check that failed, doubling from what its silence cost the stay up to the period.
const FIRST_RETRY_S: u64 = TIMEOUT.as_secs();

/// What every thread that tells the time shares: the offset of the clock on screen, and the hardware clock's turn.
#[derive(Debug, Default)]
pub struct Wall {
    /// How far the clock on screen is ahead of UTC, in minutes.
    pub offset: i16,
    is_rtc_due: bool,
    is_rtc_writing: bool,
}

impl Wall {
    /// A clock at UTC.
    pub const fn new() -> Wall {
        Wall { offset: 0, is_rtc_due: false, is_rtc_writing: false }
    }

    /// Asks for the hardware clock to be set; true where a job has to start for it.
    pub const fn ask_rtc(&mut self) -> bool {
        let is_new = !self.is_rtc_due;
        self.is_rtc_due = true;
        is_new && !self.is_rtc_writing
    }

    /// No job could start: nothing is asked any more.
    pub const fn drop_rtc(&mut self) {
        self.is_rtc_due = false;
    }

    /// True where the job that sets the hardware clock has to write once more.
    pub const fn next_rtc_write(&mut self) -> bool {
        self.is_rtc_writing = self.is_rtc_due;
        self.is_rtc_due = false;
        self.is_rtc_writing
    }
}

/// What came of asking the network for the time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeAnswer {
    Answered { is_moved: bool },
    NoTime,
    Failed(Cause),
}

impl TimeAnswer {
    /// The answer of a check: the service's, none where it gave none, or why no network could be had.
    pub fn from_result(result: Result<Option<bool>, &Fault>) -> TimeAnswer {
        match result {
            Ok(Some(is_moved)) => TimeAnswer::Answered { is_moved },
            Ok(None) | Err(Fault::Temporary(_)) => TimeAnswer::NoTime,
            Err(fault) => TimeAnswer::Failed(Cause::from_fault(fault, Cause::NoAnswer)),
        }
    }
}

/// By how many seconds a clock is ahead of a server's time, behind for a negative number; none where they agree.
pub fn ahead_by(clock: u64, server: u64) -> Option<i64> {
    let gap = clock.abs_diff(server);
    if gap <= TOLERANCE_S {
        return None;
    }
    let magnitude = i64::try_from(gap).unwrap_or(i64::MAX);
    Some(if clock > server { magnitude } else { -magnitude })
}

fn to_gap_text(ahead_s: i64) -> String {
    format!("{} s {}", ahead_s.unsigned_abs(), if ahead_s > 0 { "ahead" } else { "behind" })
}

fn to_outcome_text(ahead_s: Option<i64>) -> String {
    match ahead_s {
        Some(ahead_s) => format!("the clocks were {} and are set", to_gap_text(ahead_s)),
        None => String::from("the clocks agree with it"),
    }
}

/// The log line of a time check, after that many milliseconds.
pub fn to_time_line(ahead_s: Option<i64>, ms: u128) -> String {
    format!("net: time: {}, in {ms} ms", to_outcome_text(ahead_s))
}

fn to_set_by_text(server: &str, ahead_s: i64) -> String {
    format!("set by {server}, it was {}", to_gap_text(ahead_s))
}

/// The log line of clocks a server set.
pub fn to_set_by_line(server: &str, ahead_s: i64) -> String {
    format!("clock: {}", to_set_by_text(server, ahead_s))
}

/// The last check's answer and the place service's last offset, where new.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Report {
    pub answer: Option<TimeAnswer>,
    pub offset: Option<i16>,
}

/// An order to the clock thread: `H` holds the stay a check is made in, `O` is the way to the network, and the flag has the clock's screen told.
pub enum Order<H, O> {
    Locate(H),
    Check(H),
    Ask(O, bool),
}

/// What the clock thread, the holders of the network and the main loop share; `W` wakes a holder that waits for the first check.
pub struct State<H, O, W> {
    pub orders: Orders<Order<H, O>>,
    check_due_s: u64,
    // The wait the last failed check set; 0 while none has failed since one answered.
    retry_s: u64,
    // The first check since the start, which TLS needs the clocks of: holds wait for it, and whoever waited through one that failed makes none of its own.
    first: Turns<(), W>,
    pub report: Report,
}

impl<H, O, W> Default for State<H, O, W> {
    fn default() -> State<H, O, W> {
        State { orders: Orders::default(), check_due_s: 0, retry_s: 0, first: Turns::new(), report: Report::default() }
    }
}

impl<H, O, W> State<H, O, W> {
    // At `now_s`: moves a check that is due on; true where one was due.
    const fn take_due(&mut self, now_s: u64) -> bool {
        match next_check_s(self.check_due_s, now_s) {
            Some(next) => {
                self.check_due_s = next;
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    const fn check_due_s(&self) -> u64 {
        self.check_due_s
    }

    /// What a hold does next for the clocks at `now_s`: a quiet one counts as the check that was due; another waits for the first check since the start or makes it; then the place after a first check it made, or a check that is due, goes to the clock thread beside the stay.
    pub fn next_hold_step(&mut self, hold: &mut HoldCheck, now_s: u64) -> HoldStep {
        if hold.is_quiet {
            self.take_due(now_s);
            return HoldStep::Done;
        }
        let count = match hold.stage {
            HoldStage::Checked(true) => return HoldStep::Order(StayCheck::Locate),
            HoldStage::Checked(false) => return HoldStep::Done,
            HoldStage::Begun => self.first.end_count(),
            HoldStage::Waiting(count) => count,
        };
        if self.first.end_count() == 0 {
            if self.first.take() {
                return HoldStep::Check;
            }
            hold.stage = HoldStage::Waiting(count);
            return HoldStep::Await;
        }
        if self.first.failure_since(count).is_some() {
            return HoldStep::Done;
        }
        if self.take_due(now_s) { HoldStep::Order(StayCheck::Check) } else { HoldStep::Done }
    }

    /// Adds a waiter for the first check, which its end hands back.
    pub fn add_waiter(&mut self, waiter: W) {
        self.first.add_waiter(waiter);
    }

    /// Takes how the first check a hold made at `now_s` ended; returns the waiters to wake.
    pub fn end_hold_check(&mut self, hold: &mut HoldCheck, is_set: bool, now_s: u64) -> Vec<W> {
        hold.stage = HoldStage::Checked(is_set);
        self.end_check(true, is_set, now_s)
    }

    /// The clock thread begins a check: true where it is the first since the start, which none made or makes, so that `end_check` ends that.
    pub const fn begin_check(&mut self) -> bool {
        self.first.end_count() == 0 && self.first.take()
    }

    /// A check ended at `now_s`, the first or not, the clocks set or not: the next is due a period later, or, where this one failed, after a wait that doubles from one failure to the next; returns the waiters of a first to wake.
    pub fn end_check(&mut self, is_first: bool, is_set: bool, now_s: u64) -> Vec<W> {
        self.retry_s = if is_set { 0 } else { (2 * self.retry_s).clamp(FIRST_RETRY_S, CHECK_PERIOD_S) };
        self.check_due_s = now_s + if is_set { CHECK_PERIOD_S } else { self.retry_s };
        if is_first { self.first.end((!is_set).then_some(())) } else { Vec::new() }
    }

    pub fn take_report(&mut self) -> Report {
        core::mem::take(&mut self.report)
    }
}

const fn next_check_s(due_s: u64, now_s: u64) -> Option<u64> {
    if now_s >= due_s { Some(now_s + CHECK_PERIOD_S) } else { None }
}

/// A hold's look at the clocks: whether it is quiet, its holder asking for the time itself or having had it asked in an earlier stay, and how far it came.
pub struct HoldCheck {
    is_quiet: bool,
    stage: HoldStage,
}

impl HoldCheck {
    pub const fn new(is_quiet: bool) -> HoldCheck {
        HoldCheck { is_quiet, stage: HoldStage::Begun }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HoldStage {
    Begun,
    // Waits for the first check, an end count noted.
    Waiting(u64),
    // Made the first check, the clocks set or not.
    Checked(bool),
}

/// What a hold does next for the clocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldStep {
    // Waits for the first check, which another makes; `State::add_waiter` takes what wakes it.
    Await,
    // Makes the first check on the holder's thread; `State::end_hold_check` takes the outcome.
    Check,
    // Has the clock thread do this beside the stay, under the hold.
    Order(StayCheck),
    Done,
}

/// What a stay has the clock thread do beside the stay's own work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StayCheck {
    // The stay's holder made the first time check itself, so only the place is asked.
    Locate,
    Check,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_write_repeats() {
        let mut wall = Wall::new();
        assert!(wall.ask_rtc() && !wall.ask_rtc());
        assert!(wall.next_rtc_write());
        assert!(!wall.ask_rtc(), "the job at work takes it");
        assert!(wall.next_rtc_write() && !wall.next_rtc_write());
        assert!(wall.ask_rtc());
    }

    #[test]
    fn clock_tolerance() {
        let table = [
            ((100, 100), None),
            ((101, 100), None),
            ((99, 100), None),
            ((102, 100), Some(2)),
            ((98, 100), Some(-2)),
            ((1_791_697_759, 1_747_392_631), Some(44_305_128)),
            ((0, 1_791_697_759), Some(-1_791_697_759)),
            ((u64::MAX, 0), Some(i64::MAX)),
        ];
        for ((clock, server), ahead) in table {
            assert_eq!(ahead_by(clock, server), ahead, "{clock} against {server}");
        }
    }

    #[test]
    fn set_lines() {
        assert_eq!(
            [to_outcome_text(Some(44_305_128)), to_outcome_text(Some(-3)), to_outcome_text(None)],
            [
                "the clocks were 44305128 s ahead and are set",
                "the clocks were 3 s behind and are set",
                "the clocks agree with it"
            ]
        );
        assert_eq!(
            [to_set_by_text("time.cloudflare.com", 2), to_set_by_text("time.cloudflare.com", -44_305_128)],
            ["set by time.cloudflare.com, it was 2 s ahead", "set by time.cloudflare.com, it was 44305128 s behind"]
        );
        assert_eq!(to_time_line(None, 40), "net: time: the clocks agree with it, in 40 ms");
    }

    type Shared = State<(), (), char>;

    #[test]
    fn first_check_waited_for() {
        let mut state = Shared::default();
        let (mut first, mut second, mut third) = (HoldCheck::new(false), HoldCheck::new(false), HoldCheck::new(false));
        assert_eq!(state.next_hold_step(&mut first, 10), HoldStep::Check, "the first hold makes the first check");
        assert_eq!(state.next_hold_step(&mut second, 10), HoldStep::Await, "the next waits for it");
        state.add_waiter('w');
        assert!(!state.begin_check(), "a check of the clock thread meanwhile is no first");
        assert!(state.end_check(false, true, 10).is_empty(), "and its end wakes nobody");
        assert_eq!(state.end_hold_check(&mut first, true, 11), ['w'], "the first's end wakes whoever waits");
        assert_eq!(
            state.next_hold_step(&mut first, 11),
            HoldStep::Order(StayCheck::Locate),
            "the clocks set, only the place is asked beside the stay"
        );
        assert_eq!(state.next_hold_step(&mut second, 11), HoldStep::Done, "and nothing is due for the waiter");
        assert_eq!(state.next_hold_step(&mut third, 11 + CHECK_PERIOD_S), HoldStep::Order(StayCheck::Check));
        assert_eq!(state.next_hold_step(&mut HoldCheck::new(false), 12 + CHECK_PERIOD_S), HoldStep::Done, "hourly");
    }

    #[test]
    fn failed_first_check() {
        let mut state = Shared::default();
        let (mut first, mut second) = (HoldCheck::new(false), HoldCheck::new(false));
        assert_eq!(state.next_hold_step(&mut first, 10), HoldStep::Check);
        assert_eq!(state.next_hold_step(&mut second, 10), HoldStep::Await);
        state.end_hold_check(&mut first, false, 11);
        assert_eq!(state.next_hold_step(&mut first, 11), HoldStep::Done, "a check that failed is not ordered again");
        assert_eq!(state.next_hold_step(&mut second, 11), HoldStep::Done, "nor by whoever waited through it");
        let step = |state: &mut Shared, now_s: u64| state.next_hold_step(&mut HoldCheck::new(false), now_s);
        assert_eq!(step(&mut state, 12), HoldStep::Done, "nor at once by the next hold: its silence cost a stay");
        assert_eq!(
            step(&mut state, 11 + FIRST_RETRY_S),
            HoldStep::Order(StayCheck::Check),
            "the first hold after the wait has it made again, beside its stay"
        );
        let failed_at = 100;
        state.end_check(false, false, failed_at);
        assert_eq!(step(&mut state, failed_at + 2 * FIRST_RETRY_S - 1), HoldStep::Done, "a second failure waits twice");
        assert_eq!(step(&mut state, failed_at + 2 * FIRST_RETRY_S), HoldStep::Order(StayCheck::Check));
        (0..20).for_each(|_| drop(state.end_check(false, false, failed_at)));
        assert_eq!(state.check_due_s(), failed_at + CHECK_PERIOD_S, "and no wait outlasts the period");
        state.end_check(false, true, failed_at);
        state.end_check(false, false, failed_at);
        assert_eq!(state.check_due_s(), failed_at + FIRST_RETRY_S, "a check that answered begins the waits anew");
    }

    #[test]
    fn quiet_hold_counts_as_check() {
        let mut state = Shared::default();
        assert_eq!(state.next_hold_step(&mut HoldCheck::new(true), 10), HoldStep::Done, "it makes no first check");
        assert_eq!(state.check_due_s(), 10 + CHECK_PERIOD_S, "and counts as the check that was due");
        assert!(state.begin_check(), "the clock thread's check, nobody's yet, is the first");
        state.end_check(true, true, 20);
        assert_eq!(state.next_hold_step(&mut HoldCheck::new(false), 21), HoldStep::Done, "after it nobody waits");
        assert_eq!(
            [next_check_s(0, 5), next_check_s(3_605, 3_604), next_check_s(3_605, 3_605)],
            [Some(3_605), None, Some(7_205)]
        );
    }
}
