//! The battery rundown's pings, counts and status, without its socket and thread.

use crate::net::icmp;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use core::net::Ipv4Addr;
use core::time::Duration;

/// The address that is pinged, and how often.
pub const TARGET: [u8; 4] = [1, 1, 1, 1];
pub const PERIOD: Duration = Duration::from_secs(1);
// A join or a stay that failed is tried again at the pace of the echoes.
const REST: Duration = PERIOD;
// What every echo of the rundown carries to tell its replies by: the device's name in ASCII.
const IDENTIFIER: u16 = u16::from_be_bytes(*b"r1");
/// The log line of a stay in the network.
pub const JOINED_LINE: &str = "rundown: in the network";

/// What remains of a round `elapsed` old, none once the next echo is due.
pub fn round_wait(elapsed: Duration) -> Option<Duration> {
    PERIOD.checked_sub(elapsed).filter(|remaining| !remaining.is_zero())
}

/// The log line of a rundown switched on.
pub fn to_on_line() -> String {
    format!("rundown: on, pinging {} every second", Ipv4Addr::from(TARGET))
}

/// How the battery rundown stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Off,
    Joining,
    Answered,
    Unanswered,
    Offline,
}

/// How the rundown stands, where that changed since it was last taken.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Report {
    pub status: Option<Status>,
}

/// What a change of the switch asks of the main loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Switch {
    Nothing,
    Stop,
    Start,
}

/// What the rundown's thread and the main loop share.
#[derive(Default)]
pub struct State {
    pub is_wanted: bool,
    is_running: bool,
    report: Report,
}

impl State {
    /// Takes whether the rundown is switched on: on, a thread starts unless one runs; off, the one that runs lets the network go.
    pub const fn set_wanted(&mut self, is_on: bool) -> Switch {
        if core::mem::replace(&mut self.is_wanted, is_on) == is_on {
            Switch::Nothing
        } else if !is_on {
            Switch::Stop
        } else if core::mem::replace(&mut self.is_running, true) {
            Switch::Nothing
        } else {
            Switch::Start
        }
    }

    /// The thread could not be started.
    pub const fn note_no_thread(&mut self) {
        self.is_running = false;
        self.report.status = Some(Status::Offline);
    }

    // True where the thread ends, false where the switch is on.
    const fn should_end(&mut self) -> bool {
        self.is_running = self.is_wanted;
        !self.is_running
    }

    /// True while no thread of the rundown runs.
    pub const fn is_off(&self) -> bool {
        !self.is_running
    }

    /// Takes how the rundown stands, where that is new; true where the main loop is to look.
    pub const fn add_status(&mut self, status: Option<Status>) -> bool {
        if status.is_some() {
            self.report.status = status;
        }
        status.is_some()
    }

    pub fn take_report(&mut self) -> Report {
        core::mem::take(&mut self.report)
    }
}

/// What the rundown's thread does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    // Holds the network and pings while the stay lasts.
    Join,
    // Waits that long, or until the switch turns.
    Rest(Duration),
    End,
}

/// What the rundown has done since it was switched on.
#[derive(Default)]
pub struct Run {
    sent_count: u64,
    answered_count: u64,
    status: Option<Status>,
    fault: Option<Fault>,
    is_rest_due: bool,
}

impl Run {
    fn update(&mut self, status: Status) -> Option<Status> {
        (self.status.replace(status) != Some(status)).then_some(status)
    }

    /// The thread's next step: it ends once the switch is off, waits out a join or a stay that failed, and joins otherwise.
    pub const fn next_step(&mut self, state: &mut State) -> Step {
        if state.should_end() {
            Step::End
        } else if core::mem::replace(&mut self.is_rest_due, false) {
            Step::Rest(REST)
        } else {
            Step::Join
        }
    }

    /// A join begins: the status changes, unless the last join failed, which stands until one works.
    pub fn begin_join(&mut self) -> Option<Status> {
        if self.fault.is_some() { None } else { self.update(Status::Joining) }
    }

    pub fn note_joined(&mut self) {
        self.fault = None;
    }

    /// A join failed: the status, and true where the reason differs from the last and the log tells it.
    pub fn note_join_failed(&mut self, fault: &Fault) -> (Option<Status>, bool) {
        let is_new = self.fault.as_ref() != Some(fault);
        if is_new {
            self.fault = Some(fault.clone());
        }
        self.is_rest_due = true;
        (self.update(Status::Offline), is_new)
    }

    /// The pinging of a stay failed.
    pub fn note_unanswered(&mut self) -> Option<Status> {
        self.is_rest_due = true;
        self.update(Status::Unanswered)
    }

    /// The log line of a rundown switched off after that many seconds.
    pub fn to_off_line(&self, seconds: u64) -> String {
        format!("rundown: off after {seconds} s, {} echoes sent and {} answered", self.sent_count, self.answered_count)
    }
}

/// The end of a round of pinging: the status where it is new, and the reply's delay where answering changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoundEnd {
    pub status: Option<Status>,
    change: Option<Option<Duration>>,
}

impl RoundEnd {
    fn to_log_line(self) -> Option<String> {
        let target = Ipv4Addr::from(TARGET);
        self.change.map(|reply| match reply {
            Some(delay) => format!("{target} answers, after {} ms", delay.as_millis()),
            None => format!("{target} does not answer"),
        })
    }

    /// The log line of whether the address answers, where that changed.
    pub fn to_rundown_line(self) -> Option<String> {
        self.to_log_line().map(|line| format!("rundown: {line}"))
    }
}

/// The echoes of one stay in the network.
#[derive(Default)]
pub struct Pings {
    sequence: u16,
    reply_delay: Option<Duration>,
    was_answered: Option<bool>,
    has_unsent: bool,
}

impl Pings {
    /// Begins the next round: its echo, under a sequence of its own.
    pub fn echo(&mut self) -> [u8; icmp::HEADER] {
        (self.sequence, self.reply_delay) = (self.sequence.wrapping_add(1), None);
        icmp::request(IDENTIFIER, self.sequence)
    }

    /// Takes whether the round's echo went out; true where the log tells that it did not, as it tells the first of a stay.
    pub const fn note_sent(&mut self, is_sent: bool, run: &mut Run) -> bool {
        if is_sent {
            run.sent_count += 1;
            return false;
        }
        !core::mem::replace(&mut self.has_unsent, true)
    }

    /// Takes a packet `delay` after the round's echo: the first reply to that echo counts.
    pub fn take_packet(&mut self, packet: &[u8], delay: Duration) {
        if icmp::parse_reply(packet, IDENTIFIER, TARGET) == Some(self.sequence) {
            self.reply_delay = self.reply_delay.or(Some(delay));
        }
    }

    /// Ends the round once the next echo is due.
    pub fn end_round(&mut self, run: &mut Run) -> RoundEnd {
        let is_answered = self.reply_delay.is_some();
        run.answered_count += u64::from(is_answered);
        let has_changed = self.was_answered.replace(is_answered) != Some(is_answered);
        RoundEnd {
            status: run.update(if is_answered { Status::Answered } else { Status::Unanswered }),
            change: has_changed.then_some(self.reply_delay),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const DELAY: Duration = Duration::from_millis(12);

    fn reply_to(sequence: u16, source: [u8; 4]) -> Vec<u8> {
        let mut head = alloc::vec![0u8; 20];
        (head[0], head[9]) = (0x45, 1);
        head[12..16].copy_from_slice(&source);
        let mut echo = icmp::request(IDENTIFIER, sequence);
        (echo[0], echo[2], echo[3]) = (0, 0, 0);
        let sum = icmp::checksum(&echo);
        echo[2..4].copy_from_slice(&sum.to_be_bytes());
        head.extend(echo);
        head
    }

    #[test]
    fn first_reply_counts() {
        let (mut run, mut pings) = (Run::default(), Pings::default());
        let echo = pings.echo();
        assert_eq!(echo, icmp::request(0x7231, 1), "the first echo of a stay carries the sequence 1 under `r1`");
        assert!(!pings.note_sent(true, &mut run));
        pings.take_packet(&reply_to(1, [9, 9, 9, 9]), DELAY);
        pings.take_packet(&reply_to(2, TARGET), DELAY);
        pings.take_packet(&reply_to(1, TARGET), DELAY);
        pings.take_packet(&reply_to(1, TARGET), 2 * DELAY);
        assert_eq!(
            pings.end_round(&mut run),
            RoundEnd { status: Some(Status::Answered), change: Some(Some(DELAY)) },
            "of another address, of another round, and the same reply again count for nothing"
        );
        pings.echo();
        pings.note_sent(true, &mut run);
        pings.take_packet(&reply_to(2, TARGET), DELAY);
        assert_eq!(pings.end_round(&mut run), RoundEnd { status: None, change: None }, "answered again: nothing new");
        pings.echo();
        pings.note_sent(true, &mut run);
        assert_eq!(pings.end_round(&mut run), RoundEnd { status: Some(Status::Unanswered), change: Some(None) });
        assert_eq!((run.sent_count, run.answered_count), (3, 2));
        assert_eq!(
            [Some(Some(DELAY)), Some(None), None].map(|change| RoundEnd { status: None, change }.to_log_line()),
            [Some("1.1.1.1 answers, after 12 ms".into()), Some("1.1.1.1 does not answer".into()), None],
            "the log hears whether the address answers where that changed, and nothing where it did not"
        );
    }

    #[test]
    fn round_waits_period() {
        assert_eq!(round_wait(Duration::ZERO), Some(PERIOD));
        assert_eq!(round_wait(PERIOD - DELAY), Some(DELAY));
        assert_eq!((round_wait(PERIOD), round_wait(2 * PERIOD)), (None, None));
    }

    #[test]
    fn rundown_lines() {
        assert_eq!(to_on_line(), "rundown: on, pinging 1.1.1.1 every second");
        let run = Run { sent_count: 3, answered_count: 2, ..Run::default() };
        assert_eq!(run.to_off_line(9), "rundown: off after 9 s, 3 echoes sent and 2 answered");
        let end = RoundEnd { status: None, change: Some(None) };
        assert_eq!(end.to_rundown_line().as_deref(), Some("rundown: 1.1.1.1 does not answer"));
        assert_eq!(RoundEnd { status: None, change: None }.to_rundown_line(), None);
    }

    #[test]
    fn unsent_echo_and_sequence_wrap() {
        let (mut run, mut pings) = (Run::default(), Pings::default());
        pings.echo();
        assert!(pings.note_sent(false, &mut run) && !pings.note_sent(false, &mut run), "the first of a stay");
        assert!(Pings::default().note_sent(false, &mut run), "and the first of the next");
        assert_eq!(run.sent_count, 0);
        let mut long = Pings::default();
        for _ in 0..u16::MAX {
            long.echo();
        }
        assert_eq!(long.echo(), icmp::request(IDENTIFIER, 0), "65536 rounds on the sequence begins again");
    }

    #[test]
    fn join_status_and_reasons() {
        let mut run = Run::default();
        assert_eq!(run.begin_join(), Some(Status::Joining));
        assert_eq!(run.begin_join(), None, "told once");
        let offline = |reason: &str| Fault::Offline(reason.into());
        assert_eq!(run.note_join_failed(&offline("no radio")), (Some(Status::Offline), true));
        assert_eq!(run.begin_join(), None, "the screen says no network until a join works");
        assert_eq!(run.note_join_failed(&offline("no radio")), (None, false), "the same reason again is no news");
        assert_eq!(run.note_join_failed(&offline("asleep")), (None, true), "another reason is told in the log");
        run.note_joined();
        assert_eq!(run.begin_join(), Some(Status::Joining));
        assert_eq!(run.note_unanswered(), Some(Status::Unanswered));
    }

    #[test]
    fn thread_steps() {
        let (mut run, mut state) = (Run::default(), State::default());
        assert_eq!(state.set_wanted(true), Switch::Start);
        assert_eq!(run.next_step(&mut state), Step::Join, "switched on, it joins");
        assert_eq!(run.next_step(&mut state), Step::Join, "and joins anew at once where a stay ended");
        run.note_join_failed(&Fault::Offline("no radio".into()));
        assert_eq!(
            (run.next_step(&mut state), run.next_step(&mut state)),
            (Step::Rest(PERIOD), Step::Join),
            "a join that failed is waited out once before the next"
        );
        run.note_unanswered();
        assert_eq!(run.next_step(&mut state), Step::Rest(PERIOD), "and so is a stay whose pinging failed");
        run.note_unanswered();
        assert_eq!(state.set_wanted(false), Switch::Stop);
        assert!(!state.is_off(), "the thread runs until it has taken the switch");
        assert_eq!(run.next_step(&mut state), Step::End, "switched off, it ends without a wait");
        assert!(state.is_off());
        assert_eq!(state.set_wanted(true), Switch::Start, "and the switch starts a thread anew");
    }
}
