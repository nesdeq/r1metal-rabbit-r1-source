//! The modem's life and its AT conversation: start and stop, the SIM and its PIN, a data connection for the radio's stay, and the state the modem's thread, the radio and the main loop share.

use crate::cell::at::{self, Card, Final, Urc};
use crate::cell::attaches::Attaches;
use crate::cell::ccci::{self, BOOT_WORDS, Event, SBP_FILES};
use crate::cell::link::Interface;
use crate::cell::sim::{Pin, Sim};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem;
use core::net::IpAddr;
use core::time::Duration;

// How long a command's answer is waited for: as long as the firmware gives a scan, a join or an address request (`wifi::TIMEOUT`).
const ANSWER_LIMIT: Duration = Duration::from_secs(10);
// How long the modem's `+EIND: 128` is waited for before the first command, as MediaTek's `gsm0710muxd` waits for it on a user build.
const AT_READY_LIMIT: Duration = Duration::from_secs(10);
// How long a data connection's activation is waited for, as ModemManager's MediaTek CCCI plugin waits for it.
const ACTIVATE_LIMIT: Duration = Duration::from_secs(60);
// How long the modem's interface is waited for after its activation, as the same plugin waits.
const INTERFACE_LIMIT: Duration = Duration::from_secs(10);
// How long a registration and the APN its attach names are waited for: an attach's five attempts of T3410, 15 s each (3GPP TS 24.301).
const ATTACH_LIMIT: Duration = Duration::from_secs(75);
// How long a busy SIM is given before it is asked again, and how often, as MediaTek's RIL retries its radio.
const BUSY_WAIT: Duration = Duration::from_millis(500);
const BUSY_TRIES: u8 = 20;
// The wait after a failed connection, doubling from T3411 to T3402 (3GPP TS 24.301); a connection the network refuses waits the whole of it, the back-off the standard gives where the network names none.
const FIRST_BACKOFF: Duration = Duration::from_secs(10);
const LAST_BACKOFF: Duration = Duration::from_secs(12 * 60);
// How long an attach is kept after a stay that began awake: an attach costs 0.18 mAh and being attached 5.2 mA, so an unused keep costs under eight attaches, and a question, a pause and another question share one.
pub(crate) const KEEP: Duration = Duration::from_secs(15 * 60);
// A SIM's PIN tries before any is spent (3GPP TS 31.102).
const FULL_TRIES: u8 = 3;
// `+CME ERROR` numbers (3GPP TS 27.007).
const SIM_MISSING: u32 = 10;
const SIM_FAILURE: u32 = 13;
const SIM_BUSY: u32 = 14;
const WRONG_PIN: u32 = 16;
// `+CEREG` states (3GPP TS 27.007).
const NOT_SEARCHING: u8 = 0;
const HOME: u8 = 1;
const DENIED: u8 = 3;
const ROAMING: u8 = 5;

/// The radio's log line of a stay that ends with the modem's connection.
pub const LOST_LINE: &str = "cell: the stay ends with the modem's connection";
/// Why a way down went on without the modem stopped.
pub const NOT_STOPPED: &str = "the modem is not stopped in the time a way down waits";
/// Why nothing of the modem can be asked of the kernel.
pub const NO_MODEM: &str = "the kernel has no modem this boot";
/// Why a command was written to no AT channel.
pub const NO_CHANNEL: &str = "the AT channel is not open";
const NOT_WANTED: &str = "mobile data is off";
pub(crate) const NOT_READY: &str = "the modem is not ready";
const NO_ANSWER: &str = "the modem does not answer";
const NOT_REGISTERED: &str = "no network took the device in";
const REGISTRATION_DENIED: &str = "the network refused the device";
const NO_APN: &str = "the network named no APN";
const NO_INTERFACE: &str = "the modem named no interface for the connection";
const NO_ADDRESS: &str = "the connection has no address";
const GAVE_WAY: &str = "a later ask came first";
const STOPPED: &str = "the modem stops";
const THREAD_GONE: &str = "the modem's thread has ended";

/// What the screens show of the modem.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Status {
    #[default]
    Off,
    // The kernel has no modem this boot.
    Absent,
    Starting,
    NoSim,
    // The SIM waits for its PIN, with the tries left.
    Pin(u8),
    Puk,
    Locked,
    Ready,
    Attaching,
    Online,
    // Attached and idle between stays.
    Attached,
    Failed,
    // The modem's thread has ended: nothing serves the modem until the app starts again.
    Ended,
}

/// The modem thread's report.
#[derive(Default)]
pub struct Report {
    pub status: Option<Status>,
}

/// The state the modem's thread, the radio's thread and the main loop share; `G` is a link granted.
pub struct State<G> {
    is_wanted: bool,
    keeps_pin: bool,
    holds_attach: bool,
    is_awake: bool,
    // A level above changed since the thread last took them.
    has_wishes: bool,
    commands: Vec<at::Command>,
    pin: Option<Pin>,
    asks: u64,
    // The ask the radio waits on, and the one the thread has not taken yet.
    awaited: Option<u64>,
    ask: Option<u64>,
    give_up: bool,
    release: bool,
    answer: Option<(u64, Result<G, String>)>,
    // From when on boot time a link may be asked for, none while the modem or its SIM is not ready.
    usable_from: Option<Duration>,
    is_running: bool,
    is_off: bool,
    // The thread has noted its state machine once: before, it waits for the partition and works on no wish.
    has_noted: bool,
    is_ended: bool,
    report: Report,
}

impl<G> Default for State<G> {
    fn default() -> State<G> {
        State {
            is_wanted: false,
            keeps_pin: true,
            holds_attach: false,
            is_awake: false,
            has_wishes: false,
            commands: Vec::new(),
            pin: None,
            asks: 0,
            awaited: None,
            ask: None,
            give_up: false,
            release: false,
            answer: None,
            usable_from: None,
            is_running: false,
            is_off: true,
            has_noted: false,
            is_ended: false,
            report: Report::default(),
        }
    }
}

/// What the main loop and the radio asked of the modem's thread since it last looked.
#[derive(Default, PartialEq, Eq, Debug)]
pub struct Wishes {
    is_wanted: bool,
    keeps_pin: bool,
    // The console's word that the attach outlasts the stays.
    holds_attach: bool,
    // Whether the device is awake: a stay begun awake keeps its attach after it.
    is_awake: bool,
    // AT commands the console sent, each answered in the log.
    commands: Vec<at::Command>,
    pin: Option<Pin>,
    // The radio's ask for a link, by its number.
    ask: Option<u64>,
    // The radio took back an ask the thread works on.
    give_up: bool,
    release: bool,
}

impl<G> State<G> {
    // Takes a level the main loop set; true if it is new, which the thread then takes.
    fn set_level(&mut self, is_new: bool) -> bool {
        self.has_wishes |= is_new;
        self.is_off &= !is_new;
        is_new
    }

    /// Sets whether the modem is wanted; true if that is new.
    pub fn want(&mut self, is_wanted: bool) -> bool {
        let is_new = mem::replace(&mut self.is_wanted, is_wanted) != is_wanted;
        self.set_level(is_new)
    }

    /// Sets whether a PIN the SIM takes is kept; true if that is new.
    pub fn keep_pin(&mut self, keeps_pin: bool) -> bool {
        let is_new = mem::replace(&mut self.keeps_pin, keeps_pin) != keeps_pin;
        self.set_level(is_new)
    }

    /// Sets whether the attach outlasts the stays; true if that is new.
    pub fn hold_attach(&mut self, holds_attach: bool) -> bool {
        let is_new = mem::replace(&mut self.holds_attach, holds_attach) != holds_attach;
        self.set_level(is_new)
    }

    /// Sets whether the device is awake; true if that is new.
    pub fn set_awake(&mut self, is_awake: bool) -> bool {
        let is_new = mem::replace(&mut self.is_awake, is_awake) != is_awake;
        self.set_level(is_new)
    }

    /// Hands the modem an AT command to send in its turn, its answer told in the log.
    pub fn send(&mut self, command: String) {
        self.commands.push(at::Command::new(command));
        self.is_off = false;
    }

    /// Hands the SIM a PIN typed for it.
    pub fn enter_pin(&mut self, pin: Pin) {
        self.pin = Some(pin);
        self.is_off = false;
    }

    /// Asks for a link; the number is the answer's.
    pub fn ask_link(&mut self) -> u64 {
        self.asks += 1;
        (self.awaited, self.ask, self.answer) = (Some(self.asks), Some(self.asks), None);
        self.is_off = false;
        self.asks
    }

    /// Takes the answer to the ask of that number, once there is one; once the thread has ended, nobody waits for one.
    pub fn take_answer(&mut self, number: u64) -> Option<Result<G, String>> {
        if self.answer.as_ref().is_none_or(|(answered, _)| *answered != number) {
            let is_gone = self.is_ended && self.awaited == Some(number);
            return is_gone.then(|| {
                self.awaited = None;
                Err(String::from(THREAD_GONE))
            });
        }
        self.awaited = None;
        self.answer.take().map(|(_, answer)| answer)
    }

    /// Notes the thread's end: no link can be asked for any more, and the report tells it.
    pub const fn end_thread(&mut self) {
        (self.is_ended, self.usable_from, self.is_running, self.is_off) = (true, None, false, true);
        self.report.status = Some(Status::Ended);
    }

    /// Takes the ask of that number back: its answer if there is one, which the caller then holds; else the thread drops it, and a link it grants later is let go; and whether the thread is to hear of that.
    pub fn take_back(&mut self, number: u64) -> (Option<Result<G, String>>, bool) {
        let answer = self.take_answer(number);
        let mut is_news = false;
        if answer.is_none() && self.awaited == Some(number) {
            self.awaited = None;
            if self.ask == Some(number) {
                self.ask = None;
            } else {
                (self.give_up, self.is_off, is_news) = (true, false, true);
            }
        }
        (answer, is_news)
    }

    pub fn release_link(&mut self) {
        (self.release, self.is_off) = (true, false);
    }

    /// True while a link can be asked for at `now`, in boot time: the modem runs, its SIM is ready and no failure is being waited out.
    pub fn is_usable(&self, now: Duration) -> bool {
        self.is_wanted && self.usable_from.is_some_and(|from| now >= from)
    }

    /// Takes what was asked since the thread last looked.
    pub fn take_wishes(&mut self) -> Wishes {
        self.has_wishes = false;
        Wishes {
            is_wanted: self.is_wanted,
            keeps_pin: self.keeps_pin,
            holds_attach: self.holds_attach,
            is_awake: self.is_awake,
            commands: mem::take(&mut self.commands),
            pin: self.pin.take(),
            ask: self.ask.take(),
            give_up: mem::take(&mut self.give_up),
            release: mem::take(&mut self.release),
        }
    }

    /// Answers the ask of that number; a link granted for an ask taken back is let go.
    pub fn answer(&mut self, number: u64, answer: Result<G, String>) {
        if self.awaited == Some(number) {
            self.answer = Some((number, answer));
        } else {
            self.release |= answer.is_ok();
        }
    }

    /// Takes the thread's state machine as it stands after its steps; true where the report has news, a link may be asked for from another time on, as at the end of a failure waited out, or the thread came to rest, which a suspend waits for.
    pub fn note(&mut self, modem: &mut Modem) -> bool {
        let was_off = self.is_off();
        let usable_from = modem.usable_from();
        let is_usable_news = mem::replace(&mut self.usable_from, usable_from) != usable_from;
        self.is_running = modem.is_running();
        let is_asked = self.has_wishes
            || self.pin.is_some()
            || !self.commands.is_empty()
            || self.ask.is_some()
            || self.give_up
            || self.release;
        (self.is_off, self.has_noted) = (!modem.is_busy() && !is_asked, true);
        let status = modem.take_status();
        if status.is_some() {
            self.report.status = status;
        }
        status.is_some() || is_usable_news || self.is_off() && !was_off
    }

    /// True while the thread works on nothing and nothing waits for it: it has not come to its work yet, or has ended and never works again.
    pub const fn is_off(&self) -> bool {
        !self.has_noted || self.is_off || self.is_ended
    }

    /// True from the modem's start until the kernel tells it stopped.
    pub const fn is_running(&self) -> bool {
        self.is_running
    }

    pub fn take_report(&mut self) -> Report {
        mem::take(&mut self.report)
    }
}

/// What the modem's thread does next.
#[derive(PartialEq, Eq, Debug)]
pub enum Step {
    // Fills the modem's folders from the factory's partitions where they are empty; `take_seed` takes the outcome.
    Seed,
    // Reads the SBP code's files in `nvdata` (`ccci::SBP_FILES`); `take_sbp` takes what they hold.
    ReadSbp,
    // Writes the CCB descriptors, hands the boot data over and starts the modem; `take_start` takes the outcome.
    Start([u32; BOOT_WORDS]),
    // Stops the modem, as after `AT+EPOF` where the flag is set; `take_stop` takes the outcome, the kernel's `Stop` event its end.
    Stop(bool),
    // Writes the files the modem left open and closes its file service's port, once it is stopped or as the thread ends.
    Unload,
    // Opens the AT channel; `take_open` takes the outcome.
    Open,
    Close,
    // Writes the command to the AT channel; `take_sent` takes the outcome.
    Send(at::Command),
    // Puts the addresses and the MTU on the modem's interface; `take_configured` takes the outcome.
    Configure(Interface),
    Unconfigure(Interface),
    // Answers the radio's ask of that number with a link on the `ccmni` device of that index, with the network's DNS servers where the connection is IPv6 alone, which tell its NAT64; `take_grant` takes the outcome. Or why there is none.
    Grant(u64, u32, Option<Vec<IpAddr>>),
    Refuse(u64, String),
    // Tells the radio its link is lost.
    Lose,
    KeepSim(Sim),
    KeepAttaches(Attaches),
    // Sets the alarm that ends the kept attach, at that second of UTC, or clears it; one the clocks were set under is set again (`arm_again`).
    Arm(Option<u64>),
    Log(String),
    Wait(Option<Duration>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Purpose {
    Echo,
    Setup,
    Card,
    Tries,
    // The PIN handed over, where it was typed rather than kept.
    Pin(Option<Pin>),
    RadioOn,
    Registration,
    Contexts,
    Activate,
    Confirm,
    Describe,
    Context,
    Down,
    RadioOff,
    PowerOff,
    // A command the console sent, its answer told in the log.
    Raw,
}

impl Purpose {
    // A step of a connection under way, which matters only while the radio asks for it.
    const fn is_join(&self) -> bool {
        matches!(
            self,
            Purpose::RadioOn
                | Purpose::Registration
                | Purpose::Contexts
                | Purpose::Activate
                | Purpose::Confirm
                | Purpose::Describe
                | Purpose::Context
        )
    }

    const fn is_sim(&self) -> bool {
        matches!(self, Purpose::Card | Purpose::Tries | Purpose::Pin(_))
    }

    // A command the way down still sends: the connection's end, the radio's, the stop's own and the console's.
    const fn outlasts_stop(&self) -> bool {
        matches!(self, Purpose::Down | Purpose::RadioOff | Purpose::PowerOff | Purpose::Raw)
    }
}

struct Command {
    text: at::Command,
    purpose: Purpose,
    limit: Duration,
}

impl Command {
    fn new(text: impl Into<at::Command>, purpose: Purpose) -> Command {
        Command { text: text.into(), purpose, limit: ANSWER_LIMIT }
    }
}

struct Pending {
    command: Command,
    sent_at: Duration,
    answers: Vec<String>,
    // What it was sent for is gone, a join left or the modem stopping: its answer is taken in and passed over.
    is_stale: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Off,
    Starting,
    // From the kernel's ready until the AT channel takes commands.
    Opening,
    Ready,
    Stopping,
    // Given up until the modem is no longer wanted; true while the modem may still run.
    Failed(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Awaited {
    Registration,
    // The APN the attach was made with, which `+EIAREG` names.
    Apn,
    Interface,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum End {
    // Let go of: the device stays attached where the console holds the attach or a keep runs.
    Release,
    Drop,
    // Waited out before the next connection.
    Failure,
}

// How a join began: from the radio off, with the radio on already, or on the attach kept since the last stay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Begin {
    RadioOff,
    RadioOn,
    Kept,
}

// The log line of a connection granted `ms` after its ask.
fn to_online_line(begin: Begin, ms: u128) -> String {
    let how = match begin {
        Begin::RadioOff => "attaching from the radio off",
        Begin::RadioOn => "with the radio on already",
        Begin::Kept => "on the attach kept since the last stay",
    };
    format!("cell: online {ms} ms after the ask, {how}")
}

// The second of UTC an alarm is set to for an end `left` from now, `utc` being now cut to its whole second: never before the end.
fn alarm_second(utc: u64, left: Duration) -> u64 {
    utc + left.as_secs() + u64::from(left.subsec_nanos() > 0) + 1
}

// The log line of the answer to a command the console sent; the command's arguments, which may hold a PIN, are left out.
fn to_answer_line(command: &Command, answers: &[String], result: Option<Final>) -> String {
    let end = match result {
        Some(Final::Ok) => String::from("OK"),
        Some(Final::Error(Some(code))) => format!("ERROR {code}"),
        Some(Final::Error(None)) => String::from("ERROR"),
        None => String::from("no answer"),
    };
    let lines: Vec<&str> = answers.iter().map(String::as_str).chain([end.as_str()]).collect();
    format!("cell: {} answered {}", command.text.name(), lines.join(" | "))
}

// A data connection under way or up: its id, its interface, the network's DNS servers, and how far it came.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Connection {
    aid: Option<u32>,
    interface: Option<Interface>,
    servers: Vec<IpAddr>,
    is_confirmed: bool,
    is_configured: bool,
    is_granted: bool,
}

impl Connection {
    // The index of its interface's `ccmni` device, with the network's DNS servers, which tell its NAT64's prefix, where the connection has no IPv4 address.
    fn link(&self) -> Option<(u32, Option<Vec<IpAddr>>)> {
        let interface = self.interface.as_ref()?;
        Some((interface.index(), interface.ipv4.is_none().then(|| self.servers.clone())))
    }
}

/// The modem thread's state machine; `now` is boot time and `utc` seconds of UTC, both as of the last call that took them.
pub struct Modem {
    is_enabled: bool,
    is_wanted: bool,
    is_seeded: bool,
    sim: Sim,
    attaches: Attaches,
    typed: Option<Pin>,
    keeps_pin: bool,
    holds_attach: bool,
    is_awake: bool,
    // Whether the stay being served began awake, and until when a kept attach stands.
    stay_awake: bool,
    kept_until: Option<Duration>,
    stage: Stage,
    status: Status,
    is_status_new: bool,
    card: Option<Card>,
    queue: VecDeque<Command>,
    pending: Option<Pending>,
    steps: VecDeque<Step>,
    // A busy SIM's asks so far, and until when the next waits.
    busy_tries: u8,
    busy_until: Option<Duration>,
    // The radio's ask being served, and how and when its join began.
    ask: Option<u64>,
    begin: Option<(Begin, Duration)>,
    // When `ttyC0` opened, while the first command waits for `+EIND: 128`.
    channel_opened_at: Option<Duration>,
    // When the modem was started and the radio came on, for the log.
    started_at: Option<Duration>,
    radio_on_at: Option<Duration>,
    is_radio_on: bool,
    waiting: Option<(Awaited, Duration)>,
    attach_apn: Option<String>,
    connection: Option<Connection>,
    backoff: Option<Duration>,
    next_backoff: Duration,
    // The SBP code read for the start to come.
    sbp: Option<u32>,
    // The alarm that ends the kept attach is to be set again from the next step's clocks.
    should_arm: bool,
    now: Duration,
    utc: u64,
}

impl Modem {
    /// A modem the kernel has or has not this boot, with the SIM's file and the record of attaches.
    pub fn new(is_enabled: bool, sim: Sim, attaches: Attaches) -> Modem {
        Modem {
            is_enabled,
            is_wanted: false,
            is_seeded: false,
            sim,
            attaches,
            typed: None,
            keeps_pin: true,
            holds_attach: false,
            is_awake: false,
            stay_awake: false,
            kept_until: None,
            stage: Stage::Off,
            status: if is_enabled { Status::Off } else { Status::Absent },
            is_status_new: true,
            card: None,
            queue: VecDeque::new(),
            pending: None,
            steps: VecDeque::new(),
            busy_tries: 0,
            busy_until: None,
            ask: None,
            begin: None,
            channel_opened_at: None,
            started_at: None,
            radio_on_at: None,
            is_radio_on: false,
            waiting: None,
            attach_apn: None,
            connection: None,
            backoff: None,
            next_backoff: FIRST_BACKOFF,
            sbp: None,
            should_arm: false,
            now: Duration::ZERO,
            utc: 0,
        }
    }

    fn at(&mut self, now: Duration, utc: u64) {
        (self.now, self.utc) = (now, utc);
    }

    fn set_status(&mut self, status: Status) {
        if self.status != status {
            (self.status, self.is_status_new) = (status, true);
        }
    }

    // The status, once each time it changes.
    fn take_status(&mut self) -> Option<Status> {
        mem::take(&mut self.is_status_new).then_some(self.status)
    }

    // From when the radio may ask for a link, the end of a failure being waited out; none while the modem or its SIM is not ready.
    fn usable_from(&self) -> Option<Duration> {
        (self.stage == Stage::Ready && self.card == Some(Card::Ready)).then(|| self.backoff.unwrap_or_default())
    }

    fn is_usable(&self) -> bool {
        self.usable_from().is_some_and(|from| self.now >= from)
    }

    // True while the thread works on something the device stays awake for; the kernel holds it awake itself while it starts or stops the modem (`fsm_routine_start`, `fsm_routine_stop` in `ccci_fsm.c`).
    fn is_busy(&self) -> bool {
        self.pending.is_some()
            || !self.queue.is_empty()
            || !self.steps.is_empty()
            || self.stage == Stage::Opening
            || self.ask.is_some()
            || self.waiting.is_some()
    }

    // True while the modem may run: from its start until the kernel tells it stopped.
    const fn is_running(&self) -> bool {
        !matches!(self.stage, Stage::Off | Stage::Failed(false))
    }

    // Commands go to a ready modem alone: from the stop on, nothing queues behind `AT+EPOF`.
    fn push(&mut self, command: Command) {
        if self.stage == Stage::Ready {
            self.queue.push_back(command);
        }
    }

    // The command out serves nothing once what it was sent for is gone.
    fn forget_pending(&mut self, is_gone: impl Fn(&Purpose) -> bool) {
        if let Some(pending) = self.pending.as_mut().filter(|pending| is_gone(&pending.command.purpose)) {
            pending.is_stale = true;
        }
    }

    // Closes the AT channel, with an open of it not made yet.
    fn close_channel(&mut self) {
        self.steps.retain(|step| *step != Step::Open);
        self.channel_opened_at = None;
        self.steps.push_back(Step::Close);
    }

    fn log(&mut self, line: String) {
        self.steps.push_back(Step::Log(line));
    }

    fn is_granted(&self) -> bool {
        self.connection.as_ref().is_some_and(|connection| connection.is_granted)
    }

    // True where an attach let go of stays: the console holds it, or its keep has not run out.
    fn keeps_attach(&self) -> bool {
        self.holds_attach || self.kept_until.is_some_and(|until| self.now < until)
    }

    // True while a connection stands between stays, attached and idle, with no ask on it.
    fn is_kept(&self) -> bool {
        self.ask.is_none() && !self.is_granted() && self.connection.is_some()
    }

    // Keeps the attach `KEEP` from now, never sooner than it was to be kept; the alarm follows.
    fn extend_keep(&mut self) {
        let until = self.now + KEEP;
        if self.kept_until.is_none_or(|kept| kept < until) {
            self.kept_until = Some(until);
            self.steps.push_back(Step::Arm(Some(alarm_second(self.utc, KEEP))));
        }
    }

    fn end_keep(&mut self) {
        if self.kept_until.take().is_some() {
            self.steps.push_back(Step::Arm(None));
        }
    }

    /// Has the alarm that ends the kept attach set again from the clocks of the next step: they were set, or its time came.
    pub const fn arm_again(&mut self) {
        self.should_arm = true;
    }

    /// Takes what was asked of the thread.
    pub fn take_wishes(&mut self, wishes: Wishes, now: Duration, utc: u64) {
        self.at(now, utc);
        self.is_awake = wishes.is_awake;
        if wishes.is_wanted != self.is_wanted {
            self.is_wanted = wishes.is_wanted;
            if !self.is_wanted {
                self.stop();
            }
        }
        self.keeps_pin = wishes.keeps_pin;
        if !self.keeps_pin && self.sim.pin.take().is_some() {
            self.steps.push_back(Step::KeepSim(self.sim.clone()));
        }
        let was_holding = mem::replace(&mut self.holds_attach, wishes.holds_attach);
        if was_holding && !self.keeps_attach() && self.is_kept() {
            self.leave(End::Drop);
        }
        for text in wishes.commands {
            if self.stage == Stage::Ready {
                self.push(Command::new(text, Purpose::Raw));
            } else {
                self.log(format!("cell: {} is not sent: {NOT_READY}", text.name()));
            }
        }
        // A PIN typed for a SIM that is ready already is for no start to come.
        if let Some(pin) = wishes.pin.filter(|_| self.card != Some(Card::Ready)) {
            self.typed = Some(pin);
            let mut purposes = self
                .queue
                .iter()
                .map(|command| &command.purpose)
                .chain(self.pending.iter().map(|pending| &pending.command.purpose));
            if self.stage == Stage::Ready && self.card == Some(Card::Pin) && !purposes.any(Purpose::is_sim) {
                self.push(Command::new(at::PIN_TRIES, Purpose::Tries));
            }
        }
        if wishes.release && self.is_granted() {
            self.leave(End::Release);
        }
        if wishes.give_up && self.ask.take().is_some() {
            self.leave(End::Release);
        }
        if let Some(number) = wishes.ask {
            self.join(number);
        }
    }

    /// Takes the outcome of `Seed`.
    pub fn take_seed(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.is_seeded = true,
            Err(reason) => self.fail(&format!("seeding the modem's files: {reason}")),
        }
    }

    /// Takes the outcome of `ReadSbp`: each of `ccci::SBP_FILES`, none where it is not there, or why it was not read.
    pub fn take_sbp(&mut self, files: [Result<Option<Vec<u8>>, String>; 2]) {
        let mut contents = [None, None];
        for ((content, file), path) in contents.iter_mut().zip(files).zip(SBP_FILES) {
            match file {
                Ok(file) => *content = file,
                Err(reason) => self.log(format!("cell: the SBP code's {path}: {reason}")),
            }
        }
        let [cxp, md] = contents;
        self.sbp = Some(ccci::to_sbp(cxp.as_deref(), md.as_deref()));
    }

    /// Takes the outcome of `Start`.
    pub fn take_start(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => (self.stage, self.started_at) = (Stage::Starting, Some(self.now)),
            Err(reason) => self.fail(&format!("starting: {reason}")),
        }
    }

    /// Takes the outcome of `Grant`: a link the radio could not be handed is let go.
    pub fn take_grant(&mut self, result: Result<(), String>) {
        if let Err(reason) = result {
            self.log(format!("cell: handing the radio its link: {reason}"));
            if let Some(connection) = &mut self.connection {
                connection.is_granted = false;
            }
            self.leave(End::Drop);
        }
    }

    /// Ends the thread's work, as it can wait no more: a link up is lost, its interface let go of, the modem stopped where it may run (without `AT+EPOF`, whose answer the thread cannot wait for) and its open files written.
    pub fn end(&mut self, reason: &str) {
        self.fail(&format!("waiting: {reason}"));
        if self.stage == Stage::Failed(true) {
            self.steps.push_back(Step::Stop(false));
        }
        self.steps.push_back(Step::Unload);
    }

    /// Takes the failed read of the AT channel or of the file service's port, or a reply the file service could not write, after which the thread watches the port no more: the modem is given up, but in a stop under way, which goes on.
    pub fn take_port_error(&mut self, reason: &str) {
        if self.stage == Stage::Stopping {
            return self.log(format!("cell: {reason}"));
        }
        self.fail(reason);
    }

    /// Takes the outcome of `Stop`, which the kernel carries out after it returns: its `Stop` event ends it.
    pub fn take_stop(&mut self, result: Result<(), String>) {
        if let Err(reason) = result {
            self.log(format!("cell: stopping: {reason}"));
            self.stage = Stage::Failed(true);
            self.set_status(Status::Failed);
        }
    }

    /// Takes an event of the modem's state.
    pub fn take_event(&mut self, event: Event, reason: &str) {
        self.log(format!("cell: the modem's state: {event:?} {reason}"));
        match (event, self.stage) {
            (Event::Ready, Stage::Starting) => {
                self.stage = Stage::Opening;
                self.steps.push_back(Step::Open);
            }
            (Event::Stop, Stage::Stopping) => {
                self.stage = Stage::Off;
                self.set_status(Status::Off);
                self.steps.push_back(Step::Unload);
            }
            (Event::Exception | Event::Stop, Stage::Off | Stage::Stopping | Stage::Failed(_)) => {}
            (Event::Exception, _) => self.fail("the modem met an exception"),
            (Event::Stop, _) => self.fail("the modem stopped by itself"),
            _ => {}
        }
    }

    /// Takes the outcome of `Open`.
    pub fn take_open(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.channel_opened_at = Some(self.now),
            Err(reason) => self.fail(&format!("opening the AT channel: {reason}")),
        }
    }

    /// Takes the outcome of `Send`: a command that did not leave is waited for no longer, and ends as one unanswered.
    pub fn take_sent(&mut self, result: Result<(), String>) {
        let Err(reason) = result else {
            return;
        };
        self.log(format!("cell: {reason}"));
        if let Some(pending) = self.pending.take() {
            self.finish(pending, None);
        }
    }

    // The first commands, once the AT channel takes them or its word is waited out.
    fn greet(&mut self) {
        (self.stage, self.channel_opened_at) = (Stage::Ready, None);
        self.push(Command::new(at::ECHO_OFF, Purpose::Echo));
        // The radio off at the start is setup: whatever it answers, the radio counts off until a join turns it on.
        for text in at::SETUP.into_iter().chain([at::RADIO_OFF]) {
            self.push(Command::new(text, Purpose::Setup));
        }
        self.push(Command::new(at::SIM_STATE, Purpose::Card));
    }

    /// Takes the outcome of `Configure`.
    pub fn take_configured(&mut self, result: Result<(), String>, now: Duration, utc: u64) {
        self.at(now, utc);
        if let Some(connection) = &mut self.connection {
            connection.is_configured = true;
        }
        if let Err(reason) = result {
            return self.fail_join(&format!("the interface: {reason}"));
        }
        let link = self.connection.as_ref().and_then(Connection::link);
        match (self.ask.take(), link) {
            (Some(number), Some((index, nat64))) => {
                self.connection.iter_mut().for_each(|connection| connection.is_granted = true);
                self.next_backoff = FIRST_BACKOFF;
                self.set_status(Status::Online);
                if let Some((begin, at)) = self.begin.take() {
                    self.log(to_online_line(begin, now.saturating_sub(at).as_millis()));
                }
                self.steps.push_back(Step::Grant(number, index, nat64));
            }
            (Some(number), None) => self.steps.push_back(Step::Refuse(number, String::from(NO_INTERFACE))),
            (None, _) => self.leave(End::Release),
        }
    }

    /// Takes a line of the AT channel.
    pub fn take_line(&mut self, line: &str, now: Duration, utc: u64) {
        self.at(now, utc);
        if let Some(urc) = at::parse_urc(line) {
            self.take_urc(urc);
        }
        let Some(pending) = &mut self.pending else {
            return;
        };
        if let Some(result) = at::to_final(line) {
            let pending = self.pending.take().unwrap_or_else(|| unreachable!("a command is pending"));
            self.finish(pending, Some(result));
        } else {
            pending.answers.push(String::from(line));
        }
    }

    fn take_urc(&mut self, urc: Urc) {
        let interface_id =
            self.connection.as_ref().and_then(|connection| connection.interface.as_ref()).map(|interface| interface.id);
        let aid = self.connection.as_ref().and_then(|connection| connection.aid);
        match urc {
            Urc::AtReady => {
                if let Some(at) = self.channel_opened_at {
                    let ms = self.now.saturating_sub(at).as_millis();
                    self.log(format!("cell: the AT channel takes commands {ms} ms after it opened"));
                    self.greet();
                }
            }
            Urc::Registration(state) => self.take_registration(state),
            Urc::Attached(apn) => {
                self.attach_apn = Some(apn);
                if self.waiting.is_some_and(|(awaited, _)| awaited == Awaited::Apn) {
                    self.waiting = None;
                    self.activate();
                }
            }
            Urc::PdnUp(id) => {
                if let Some(connection) = self.connection.as_mut().filter(|connection| connection.aid.is_none()) {
                    connection.aid = Some(id);
                }
            }
            Urc::InterfaceUp(interface) | Urc::Pdn(_, interface) => self.take_interface(interface),
            Urc::Mtu(id, mtu) => {
                if let Some(interface) = self
                    .connection
                    .as_mut()
                    .and_then(|connection| connection.interface.as_mut())
                    .filter(|interface| interface.id == id)
                {
                    interface.mtu = Some(mtu);
                }
            }
            Urc::PdnDown(id) if aid == Some(id) => self.lose("the network ended the connection"),
            Urc::InterfaceDown(id) if interface_id == Some(id) => self.lose("the modem took the interface down"),
            Urc::Detached if self.connection.is_some() => self.lose("the network detached the device"),
            Urc::PdnDown(_) | Urc::InterfaceDown(_) | Urc::Detached => {}
        }
    }

    fn take_interface(&mut self, interface: Interface) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        match &mut connection.interface {
            Some(known) if known.id == interface.id => {
                known.ipv4 = known.ipv4.or(interface.ipv4);
                known.ipv6 = known.ipv6.or(interface.ipv6);
                known.mtu = known.mtu.or(interface.mtu);
            }
            Some(_) => {}
            None => connection.interface = Some(interface),
        }
        if self.waiting.is_some_and(|(awaited, _)| awaited == Awaited::Interface) {
            self.waiting = None;
            self.confirm();
        }
    }

    // A connection past its registration (a link up, an attach kept between stays, a join registered or on a kept attach) is lost only where the device is out of the network for good; a search keeps it, as the network keeps its connections through one.
    fn take_registration(&mut self, state: u8) {
        let is_awaited = self.waiting.is_some_and(|(awaited, _)| awaited == Awaited::Registration);
        if !is_awaited && self.is_radio_on && self.connection.is_some() && matches!(state, NOT_SEARCHING | DENIED) {
            return self.lose("the network let the device go");
        }
        if !is_awaited {
            return;
        }
        if matches!(state, HOME | ROAMING) {
            self.waiting = None;
            let line = match self.radio_on_at.take() {
                Some(at) => {
                    let ms = self.now.saturating_sub(at).as_millis();
                    format!("cell: registered ({state}), {ms} ms after the radio came on")
                }
                None => format!("cell: registered ({state})"),
            };
            self.log(line);
            self.activate();
        } else if state == DENIED {
            self.fail_join(REGISTRATION_DENIED);
        }
    }

    // The connection of the APN the network attached with, else of the first `+CGCONTRDP` names.
    fn activate(&mut self) {
        match self.attach_apn.clone() {
            Some(apn) => {
                self.push(Command { text: at::activate(&apn), purpose: Purpose::Activate, limit: ACTIVATE_LIMIT })
            }
            None => self.push(Command::new(at::context(None), Purpose::Contexts)),
        }
    }

    fn confirm(&mut self) {
        match self.connection.as_ref().and_then(|connection| connection.interface.as_ref()) {
            Some(interface) => {
                let text = at::confirm(interface, true);
                self.push(Command::new(text, Purpose::Confirm));
            }
            None => self.fail_join(NO_INTERFACE),
        }
    }

    // Asks for the registration and waits for it as long as an attach takes.
    fn await_registration(&mut self) {
        self.waiting = Some((Awaited::Registration, self.now + ATTACH_LIMIT));
        self.push(Command::new(at::REGISTRATION, Purpose::Registration));
    }

    fn finish(&mut self, pending: Pending, result: Option<Final>) {
        let Pending { command, answers, is_stale, .. } = pending;
        let is_ok = result == Some(Final::Ok);
        let code = match result {
            Some(Final::Error(code)) => code,
            _ => None,
        };
        if !is_ok && !command.purpose.is_sim() && command.purpose != Purpose::Raw {
            let how = result.map_or_else(|| String::from("no answer"), |result| format!("{result:?}"));
            self.log(format!("cell: {} ended in {how}", command.text.name()));
        }
        if is_stale {
            // A radio a left join turned on goes off again, unless a join under way, which asks for it anew, wants it.
            if command.purpose == Purpose::RadioOn && is_ok && self.ask.is_none() {
                self.push(Command::new(at::RADIO_OFF, Purpose::RadioOff));
            }
            return;
        }
        let answers = answers.as_slice();
        match command.purpose {
            Purpose::Echo if result.is_none() => self.fail(NO_ANSWER),
            Purpose::Echo | Purpose::Setup | Purpose::Down => {}
            // A radio the modem keeps on counts on, and the next leave turns it off.
            Purpose::RadioOff => self.is_radio_on |= !is_ok && self.stage == Stage::Ready,
            Purpose::Card => {
                self.take_card(answers.iter().find_map(|line| at::parse_card(line)), code, result.is_none())
            }
            Purpose::Tries => self.take_tries(answers.iter().find_map(|line| at::parse_pin_tries(line))),
            Purpose::Pin(typed) => self.take_pin(is_ok, code, typed),
            Purpose::RadioOn if is_ok => {
                (self.is_radio_on, self.radio_on_at) = (true, Some(self.now));
                self.attaches.note(self.utc);
                self.steps.push_back(Step::KeepAttaches(self.attaches.clone()));
                self.await_registration();
            }
            Purpose::RadioOn => self.fail_join("the radio does not come on"),
            Purpose::Registration => {
                if let Some(state) = answers.iter().find_map(|line| at::parse_registration(line)) {
                    self.take_registration(state);
                }
            }
            Purpose::Contexts => match answers.iter().find_map(|line| at::parse_context(line)) {
                Some(context) => {
                    self.attach_apn = Some(context.apn);
                    self.activate();
                }
                // `+EIAREG` names the first attach's APN after the registration, while this question is out or after it.
                None if self.attach_apn.is_some() => self.activate(),
                None => self.waiting = Some((Awaited::Apn, self.now + ATTACH_LIMIT)),
            },
            Purpose::Activate if is_ok => {
                if self.connection.as_ref().is_some_and(|connection| connection.interface.is_some()) {
                    self.confirm();
                } else {
                    self.waiting = Some((Awaited::Interface, self.now + INTERFACE_LIMIT));
                }
            }
            // Refused: the network backs the device off for the APN, and its modem refuses every try meanwhile (3GPP TS 24.301, 6.5.1.4): the radio stays off for the standard's wait.
            Purpose::Activate if result.is_some() => {
                let (count, minutes) = (self.attaches.count(self.utc), LAST_BACKOFF.as_secs() / 60);
                self.next_backoff = LAST_BACKOFF;
                self.log(format!("cell: refused after {count} attaches in the last day: none for {minutes} min"));
                self.fail_join(&format!("the network refused the connection ({code:?})"));
            }
            Purpose::Activate => self.fail_join(NO_ANSWER),
            Purpose::Confirm => {
                if let Some(connection) = &mut self.connection {
                    connection.is_confirmed = is_ok;
                }
                match self.connection.as_ref().and_then(|connection| connection.aid) {
                    Some(aid) => self.push(Command::new(at::describe(aid), Purpose::Describe)),
                    None => self.push(Command::new(at::context(None), Purpose::Context)),
                }
            }
            Purpose::Describe => {
                let aid = self.connection.as_ref().and_then(|connection| connection.aid);
                self.push(Command::new(at::context(aid), Purpose::Context));
            }
            Purpose::Context => self.take_context(answers),
            Purpose::Raw => self.log(to_answer_line(&command, answers, result)),
            Purpose::PowerOff => {
                self.close_channel();
                self.steps.push_back(Step::Stop(true));
            }
        }
    }

    fn take_card(&mut self, card: Option<Card>, code: Option<u32>, is_silent: bool) {
        if code == Some(SIM_BUSY) && self.busy_tries < BUSY_TRIES {
            (self.busy_tries, self.busy_until) = (self.busy_tries + 1, Some(self.now + BUSY_WAIT));
            return self.push(Command::new(at::SIM_STATE, Purpose::Card));
        }
        self.busy_tries = 0;
        self.card = card;
        match card {
            Some(Card::Ready) => {
                self.typed = None;
                self.set_status(Status::Ready);
                if let Some(at) = self.started_at.take() {
                    let ms = self.now.saturating_sub(at).as_millis();
                    self.log(format!("cell: the SIM is ready {ms} ms after the modem's start"));
                }
                for text in at::DATA_SETUP {
                    self.push(Command::new(text, Purpose::Setup));
                }
            }
            Some(Card::Pin) => self.push(Command::new(at::PIN_TRIES, Purpose::Tries)),
            Some(Card::Puk) => self.set_status(Status::Puk),
            Some(Card::Locked) => self.set_status(Status::Locked),
            None if is_silent => self.fail(NO_ANSWER),
            None if matches!(code, Some(SIM_MISSING | SIM_FAILURE | SIM_BUSY) | None) => self.set_status(Status::NoSim),
            None => self.set_status(Status::Locked),
        }
    }

    // A kept PIN is handed over only with every try left, so that a PIN no longer right never spends one; a typed one always.
    fn take_tries(&mut self, tries: Option<u8>) {
        let tries = tries.unwrap_or(0);
        if let Some(pin) = self.typed.take() {
            let text = at::enter_pin(pin.digits());
            self.push(Command::new(text, Purpose::Pin(Some(pin))));
        } else if let (Some(pin), FULL_TRIES) = (&self.sim.pin, tries) {
            let text = at::enter_pin(pin.digits());
            self.push(Command::new(text, Purpose::Pin(None)));
        } else {
            self.set_status(Status::Pin(tries));
        }
    }

    fn take_pin(&mut self, is_ok: bool, code: Option<u32>, typed: Option<Pin>) {
        if is_ok {
            self.log(String::from("cell: the SIM took its PIN"));
            if let Some(pin) = typed.filter(|_| self.keeps_pin) {
                self.sim.pin = Some(pin);
                self.steps.push_back(Step::KeepSim(self.sim.clone()));
            }
        } else {
            self.log(format!("cell: the SIM refused its PIN ({code:?})"));
            if typed.is_none() && code == Some(WRONG_PIN) && self.sim.pin.take().is_some() {
                self.steps.push_back(Step::KeepSim(self.sim.clone()));
            }
        }
        self.push(Command::new(at::SIM_STATE, Purpose::Card));
    }

    fn take_context(&mut self, answers: &[String]) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        let contexts: Vec<at::Context> = answers.iter().filter_map(|line| at::parse_context(line)).collect();
        let context = contexts.iter().find(|context| Some(context.id) == connection.aid).or(contexts.first());
        if let (Some(interface), Some(mtu)) = (&mut connection.interface, context.and_then(|context| context.mtu)) {
            interface.mtu = Some(mtu);
        }
        // A connection of both families has a line of the answer for each, each with its servers.
        let lines = contexts.iter().filter(|line| context.is_some_and(|context| line.id == context.id));
        connection.servers = lines.flat_map(|line| line.servers.iter().copied()).collect();
        match connection.interface.clone() {
            Some(interface) if interface.ipv4.is_some() || interface.ipv6.is_some() => {
                self.steps.push_back(Step::Configure(interface))
            }
            _ => self.fail_join(NO_ADDRESS),
        }
    }

    fn join(&mut self, number: u64) {
        let earlier = self.ask.replace(number);
        if let Some(earlier) = earlier {
            self.steps.push_back(Step::Refuse(earlier, String::from(GAVE_WAY)));
        }
        if !self.is_enabled {
            return self.refuse(NO_MODEM);
        }
        if !self.is_wanted {
            return self.refuse(NOT_WANTED);
        }
        if !self.is_usable() {
            return self.refuse(NOT_READY);
        }
        self.stay_awake = self.is_awake;
        let is_radio_on = self.is_radio_on;
        match &mut self.connection {
            Some(connection) if connection.is_granted => {
                self.ask = None;
                self.steps.push_back(match connection.link() {
                    Some((index, nat64)) => Step::Grant(number, index, nat64),
                    None => Step::Refuse(number, String::from(NO_INTERFACE)),
                });
            }
            Some(_) if earlier.is_some() => {}
            Some(connection) if is_radio_on && connection.interface.is_some() => {
                connection.is_confirmed = false;
                self.begin = Some((Begin::Kept, self.now));
                self.set_status(Status::Attaching);
                self.confirm();
            }
            _ => {
                self.connection = Some(Connection::default());
                self.begin = Some((if is_radio_on { Begin::RadioOn } else { Begin::RadioOff }, self.now));
                self.set_status(Status::Attaching);
                if is_radio_on {
                    self.await_registration();
                } else {
                    self.push(Command::new(at::RADIO_ON, Purpose::RadioOn));
                }
            }
        }
    }

    fn refuse(&mut self, reason: &str) {
        if let Some(number) = self.ask.take() {
            self.steps.push_back(Step::Refuse(number, String::from(reason)));
        }
    }

    fn fail_join(&mut self, reason: &str) {
        self.log(format!("cell: no data connection: {reason}"));
        self.refuse(reason);
        self.leave(End::Failure);
    }

    // A connection is gone: a join under way fails, a link up is lost and waited out.
    fn lose(&mut self, reason: &str) {
        if self.ask.is_some() {
            return self.fail_join(reason);
        }
        self.log(format!("cell: out of the network: {reason}"));
        self.leave(if self.is_granted() { End::Failure } else { End::Drop });
    }

    // Leaves the connection and turns the radio off, unless it is let go of while the attach is kept: then it stays, idle, for the next ask. A stay begun awake keeps it `KEEP` from its end; one begun asleep keeps nothing of its own. A link the radio holds and did not let go of is lost to it.
    fn leave(&mut self, end: End) {
        (self.waiting, self.begin, self.radio_on_at) = (None, None, None);
        self.queue.retain(|command| !command.purpose.is_join());
        self.forget_pending(Purpose::is_join);
        self.steps.retain(|step| !matches!(step, Step::Configure(_)));
        if end == End::Release && mem::take(&mut self.stay_awake) && self.is_radio_on {
            self.extend_keep();
        }
        let is_kept = end == End::Release && self.is_radio_on && self.keeps_attach();
        if let Some(connection) = self.connection.take() {
            if connection.is_granted && end != End::Release {
                self.steps.push_back(Step::Lose);
            }
            if let Some(interface) = connection.interface.as_ref() {
                if connection.is_configured {
                    self.steps.push_back(Step::Unconfigure(interface.clone()));
                }
                if connection.is_confirmed {
                    self.push(Command::new(at::confirm(interface, false), Purpose::Down));
                }
            }
            if is_kept && connection.interface.is_some() {
                self.connection =
                    Some(Connection { aid: connection.aid, interface: connection.interface, ..Connection::default() });
            }
        }
        if self.connection.is_none() {
            self.end_keep();
        }
        if end == End::Failure {
            self.backoff = Some(self.now + self.next_backoff);
            self.next_backoff = (self.next_backoff * 2).min(LAST_BACKOFF);
        }
        if self.is_radio_on && self.connection.is_none() && self.stage == Stage::Ready {
            self.is_radio_on = false;
            self.push(Command::new(at::RADIO_OFF, Purpose::RadioOff));
        }
        if self.stage == Stage::Ready && self.card == Some(Card::Ready) {
            self.set_status(if self.is_kept() { Status::Attached } else { Status::Ready });
        }
    }

    fn fail(&mut self, reason: &str) {
        self.log(format!("cell: {reason}"));
        self.refuse(reason);
        self.leave(End::Drop);
        self.queue.clear();
        (self.pending, self.card, self.is_radio_on, self.stay_awake) = (None, None, false, false);
        (self.typed, self.attach_apn, self.busy_tries, self.busy_until) = (None, None, 0, None);
        if matches!(self.stage, Stage::Opening | Stage::Ready | Stage::Stopping) {
            self.close_channel();
        }
        self.stage = Stage::Failed(self.is_running());
        self.set_status(Status::Failed);
    }

    // Stops the modem: a ready one after what the way down still sends and `AT+EPOF`, the SIM's and the setup's commands dropped.
    fn stop(&mut self) {
        self.refuse(STOPPED);
        match self.stage {
            Stage::Ready => {
                self.leave(End::Drop);
                self.queue.retain(|command| command.purpose.outlasts_stop());
                self.forget_pending(|purpose| !purpose.outlasts_stop());
                self.push(Command::new(at::POWER_OFF, Purpose::PowerOff));
                self.stage = Stage::Stopping;
            }
            Stage::Starting | Stage::Opening | Stage::Failed(true) => {
                if self.stage == Stage::Opening {
                    self.close_channel();
                }
                self.steps.push_back(Step::Stop(false));
                self.stage = Stage::Stopping;
            }
            Stage::Failed(false) => {
                self.stage = Stage::Off;
                self.set_status(Status::Off);
            }
            Stage::Off | Stage::Stopping => {}
        }
        (self.card, self.busy_tries, self.busy_until) = (None, 0, None);
        (self.typed, self.attach_apn, self.sbp) = (None, None, None);
    }

    /// What the thread does next.
    pub fn next(&mut self, now: Duration, utc: u64) -> Step {
        self.at(now, utc);
        loop {
            if let Some(step) = self.steps.pop_front() {
                return step;
            }
            if self.backoff.is_some_and(|until| until <= now) {
                self.backoff = None;
            }
            if self.busy_until.is_some_and(|until| until <= now) {
                self.busy_until = None;
            }
            if self.kept_until.is_some_and(|until| until <= now) {
                self.end_keep();
                if self.is_kept() && !self.holds_attach {
                    let minutes = KEEP.as_secs() / 60;
                    self.log(format!("cell: the attach kept {minutes} min after its last stay goes"));
                    self.leave(End::Drop);
                }
                continue;
            }
            if mem::take(&mut self.should_arm)
                && let Some(until) = self.kept_until
            {
                return Step::Arm(Some(alarm_second(utc, until.saturating_sub(now))));
            }
            if let Some((awaited, _)) = self.waiting.filter(|&(_, deadline)| now >= deadline) {
                self.waiting = None;
                self.fail_join(match awaited {
                    Awaited::Registration => NOT_REGISTERED,
                    Awaited::Apn => NO_APN,
                    Awaited::Interface => NO_INTERFACE,
                });
                continue;
            }
            if self.channel_opened_at.is_some_and(|at| now >= at + AT_READY_LIMIT) {
                let seconds = AT_READY_LIMIT.as_secs();
                self.log(format!(
                    "cell: no +EIND: 128 in {seconds} s after the AT channel opened; its commands go regardless"
                ));
                self.greet();
                continue;
            }
            if let Some(pending) = &self.pending {
                let deadline = pending.sent_at + pending.command.limit;
                if now < deadline {
                    return self.wait(Some(deadline));
                }
                let pending = self.pending.take().unwrap_or_else(|| unreachable!("a command is pending"));
                self.finish(pending, None);
                continue;
            }
            if self.busy_until.is_none()
                && let Some(command) = self.queue.pop_front()
            {
                let text = command.text.clone();
                self.pending = Some(Pending { command, sent_at: now, answers: Vec::new(), is_stale: false });
                return Step::Send(text);
            }
            if self.stage == Stage::Off && self.is_wanted && self.is_enabled {
                if !self.is_seeded {
                    return Step::Seed;
                }
                let Some(sbp) = self.sbp.take() else {
                    return Step::ReadSbp;
                };
                self.set_status(Status::Starting);
                return Step::Start(ccci::boot_data(sbp));
            }
            return self.wait(None);
        }
    }

    // A wait up to the first of a command's deadline, the one of what is awaited, the end of the wait for `+EIND: 128`, the end of a backoff, the end of a busy SIM's wait, and the end of a kept attach, which the alarm ends asleep.
    fn wait(&self, deadline: Option<Duration>) -> Step {
        let at_ready = self.channel_opened_at.map(|at| at + AT_READY_LIMIT);
        let awaited = self.waiting.map(|(_, deadline)| deadline);
        let deadlines = [deadline, awaited, at_ready, self.backoff, self.busy_until, self.kept_until];
        Step::Wait(deadlines.into_iter().flatten().min().map(|deadline| deadline.saturating_sub(self.now)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::net::Ipv4Addr;

    const SECOND: Duration = Duration::from_secs(1);
    const SETUP: [&str; 6] = ["ATE0Q0V1", "AT+CMEE=1", "AT+CGEREP=1,0", "AT+CEREG=1", "AT+EGTYPE=1", "AT+CFUN=4"];

    struct Bench {
        modem: Modem,
        now: Duration,
        utc: u64,
        // Whether the device is awake, which every wish tells.
        is_awake: bool,
        // The log lines the steps passed over.
        lines: Vec<String>,
    }

    impl Bench {
        fn new(sim: Sim) -> Bench {
            let modem = Modem::new(true, sim, Attaches::default());
            Bench { modem, now: SECOND, utc: 1_800_000_000, is_awake: false, lines: Vec::new() }
        }

        fn pass(&mut self, time: Duration) {
            self.now += time;
            self.utc += time.as_secs();
        }

        // The next step but a log, which is kept.
        fn next(&mut self) -> Step {
            loop {
                match self.modem.next(self.now, self.utc) {
                    Step::Log(line) => self.lines.push(line),
                    step => return step,
                }
            }
        }

        // The steps up to the next wait.
        fn steps(&mut self) -> Vec<Step> {
            core::iter::from_fn(|| Some(self.next()).filter(|step| !matches!(step, Step::Wait(_)))).collect()
        }

        // The steps up to the next command sent.
        fn until_sent(&mut self) -> (Vec<Step>, String) {
            let mut steps = Vec::new();
            loop {
                match self.next() {
                    Step::Send(text) => return (steps, String::from(text.text())),
                    Step::Wait(_) => panic!("nothing to send after {steps:?}"),
                    step => steps.push(step),
                }
            }
        }

        fn sent(&mut self) -> String {
            let (steps, text) = self.until_sent();
            assert!(
                steps.iter().all(|step| matches!(step, Step::KeepAttaches(_) | Step::KeepSim(_))),
                "{steps:?} before {text}"
            );
            text
        }

        fn answer(&mut self, lines: &[&str]) {
            for line in lines {
                self.modem.take_line(line, self.now, self.utc);
            }
        }

        fn wish(&mut self, wishes: Wishes) {
            let wishes = Wishes { is_wanted: true, keeps_pin: true, is_awake: self.is_awake, ..wishes };
            self.modem.take_wishes(wishes, self.now, self.utc);
        }

        fn ask(&mut self, number: u64) {
            self.wish(Wishes { ask: Some(number), ..Wishes::default() });
        }

        fn talk(&mut self, expected: &str, answer: &[&str]) {
            assert_eq!(self.sent(), expected);
            self.answer(answer);
        }

        // Seeded, the SBP code read: the boot data the modem starts with.
        fn start(&mut self) -> Step {
            assert_eq!(self.next(), Step::ReadSbp);
            self.modem.take_sbp([Ok(None), Ok(None)]);
            self.next()
        }

        // A modem the kernel told ready, its AT channel yet to open.
        fn started(sim: Sim) -> Bench {
            let mut bench = Bench::new(sim);
            bench.wish(Wishes::default());
            assert_eq!(bench.next(), Step::Seed);
            bench.modem.take_seed(Ok(()));
            assert!(matches!(bench.start(), Step::Start(_)));
            bench.modem.take_start(Ok(()));
            bench.modem.take_event(Event::Ready, "");
            bench
        }

        // A modem on its way to ready whose `ttyC0` is open.
        fn channel_opened(sim: Sim) -> Bench {
            let mut bench = Bench::started(sim);
            assert_eq!(bench.steps(), [Step::Open]);
            bench.modem.take_open(Ok(()));
            bench
        }

        fn opened(sim: Sim) -> Bench {
            let mut bench = Bench::channel_opened(sim);
            bench.answer(&["+EIND: 128"]);
            for command in SETUP {
                bench.talk(command, &["OK"]);
            }
            bench
        }

        fn ready() -> Bench {
            let mut bench = Bench::opened(Sim::default());
            bench.talk("AT+CPIN?", &["+CPIN: READY", "OK"]);
            for command in ["AT+EIFCONFIG=3", "AT+EIAREG=1", "AT+EDALLOW=1"] {
                bench.talk(command, &["+CME ERROR: 100"]);
            }
            bench
        }

        fn join(&mut self, number: u64) {
            self.ask(number);
            self.talk("AT+CFUN=1", &["OK"]);
            self.talk("AT+CEREG?", &["+CEREG: 1,2", "OK"]);
            self.answer(&["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1"]);
            self.talk(
                "AT+EAPNACT=1,\"internet\",\"default\",0",
                &["+EIF: 100, ifup, 1, 10.1.2.3, 0", "+EIF: 100, mtu, 1500", "+CGEV: ME PDN ACT 0", "OK"],
            );
            self.talk("AT+EIF=100,\"ifst\",\"up\",1", &["OK"]);
            self.talk("AT+EPDN=0,\"ifst\",20", &["+EPDN: 0, \"new\", 1, 100, 1500, 1, \"10.1.2.3\"", "OK"]);
            self.talk(
                "AT+CGCONTRDP=0",
                &["+CGCONTRDP: 0,5,\"internet.mnc001.mcc001.gprs\",\"\",\"\",\"\",\"\",\"\",\"\",0,0,1430", "OK"],
            );
            assert_eq!(self.next(), Step::Configure(interface()), "the MTU the network gave");
            self.modem.take_configured(Ok(()), self.now, self.utc);
            assert_eq!(self.next(), Step::Grant(number, 0, None), "on ccmni0; a connection with IPv4 needs no NAT64");
        }

        // The ask of that number, made already, takes up the attach kept since the last stay.
        fn rejoin(&mut self, number: u64) {
            self.talk("AT+EIF=100,\"ifst\",\"up\",1", &["OK"]);
            self.talk("AT+EPDN=0,\"ifst\",20", &["OK"]);
            self.talk("AT+CGCONTRDP=0", &["+CGCONTRDP: 0,5,\"internet\",\"\",\"\",\"\",\"\",\"\",\"\",0,0,1430", "OK"]);
            assert_eq!(self.next(), Step::Configure(interface()));
            self.modem.take_configured(Ok(()), self.now, self.utc);
            assert_eq!(self.next(), Step::Grant(number, 0, None), "the attach kept is taken up, with no attach");
        }
    }

    fn interface() -> Interface {
        Interface { id: 100, ipv4: Some(Ipv4Addr::new(10, 1, 2, 3)), ipv6: None, mtu: Some(1430) }
    }

    #[test]
    fn boot_to_ready() {
        let mut bench = Bench::ready();
        assert_eq!(bench.next(), Step::Wait(None));
        assert!(bench.modem.is_usable() && !bench.modem.is_busy());
        assert_eq!(bench.modem.take_status(), Some(Status::Ready));
        assert_eq!(bench.modem.take_status(), None, "once");
        assert!(
            bench.lines.iter().any(|line| line == "cell: the SIM is ready 0 ms after the modem's start"),
            "{:?}",
            bench.lines
        );
    }

    #[test]
    fn join_times_logged() {
        let mut bench = Bench::ready();
        bench.lines.clear();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.now += 2 * SECOND;
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        bench.now += SECOND;
        bench.talk("AT+EAPNACT=1,\"internet\",\"default\",0", &["+EIF: 100, ifup, 1, 10.1.2.3, 0", "OK"]);
        bench.talk("AT+EIF=100,\"ifst\",\"up\",1", &["OK"]);
        bench.talk("AT+CGCONTRDP", &["+CGCONTRDP: 0,5,\"internet\",\"\",\"\",\"\",\"\",\"\",\"\",0,0,1430", "OK"]);
        assert_eq!(bench.next(), Step::Configure(interface()));
        bench.modem.take_configured(Ok(()), bench.now, bench.utc);
        assert_eq!(bench.next(), Step::Grant(1, 0, None));
        let lines: Vec<&str> = bench.lines.iter().map(String::as_str).filter(|line| line.contains(" ms ")).collect();
        assert_eq!(
            lines,
            [
                "cell: registered (1), 2000 ms after the radio came on",
                "cell: online 3000 ms after the ask, attaching from the radio off"
            ]
        );
    }

    #[test]
    fn held_attach_kept_until_let_go() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.wish(Wishes { release: true, holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()));
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        assert_eq!(bench.next(), Step::Wait(None), "the attach the console holds stays");
        assert_eq!(bench.modem.attaches.to_text(), "1800000000\n", "one attach");
        bench.wish(Wishes { ask: Some(2), holds_attach: true, ..Wishes::default() });
        bench.rejoin(2);
        assert_eq!(
            bench.lines.last().map(String::as_str),
            Some("cell: online 0 ms after the ask, on the attach kept since the last stay")
        );
        assert_eq!(bench.modem.attaches.to_text(), "1800000000\n", "and no second");
        bench.wish(Wishes { release: true, holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()));
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        bench.wish(Wishes::default());
        assert_eq!(bench.sent(), "AT+CFUN=4", "let go of, the attach goes");
    }

    #[test]
    fn kept_attach_dropped_out_of_network() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.wish(Wishes { release: true, holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()));
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        bench.answer(&["+CEREG: 2"]);
        assert_eq!(bench.next(), Step::Wait(None), "a search keeps it");
        bench.answer(&["+CEREG: 0"]);
        assert_eq!(bench.sent(), "AT+CFUN=4", "out of the network for good it goes");
        bench.answer(&["OK"]);
        bench.wish(Wishes { ask: Some(2), holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.sent(), "AT+CFUN=1", "and the next stay attaches anew");
    }

    #[test]
    fn console_answers_logged() {
        let mut bench = Bench::new(Sim::default());
        bench.wish(Wishes { commands: vec![at::Command::new("AT+CPSMS?")], ..Wishes::default() });
        assert_eq!(
            bench.modem.next(bench.now, bench.utc),
            Step::Log(format!("cell: AT+CPSMS? is not sent: {NOT_READY}"))
        );
        let mut bench = Bench::ready();
        bench.wish(Wishes { commands: vec![at::Command::new("AT+CPSMS?")], ..Wishes::default() });
        bench.talk("AT+CPSMS?", &["+CPSMS: 0", "OK"]);
        assert_eq!(bench.modem.next(bench.now, bench.utc), Step::Log("cell: AT+CPSMS? answered +CPSMS: 0 | OK".into()));
        bench.wish(Wishes { commands: vec![at::Command::new("AT+CPIN=\"1234\"")], ..Wishes::default() });
        bench.talk("AT+CPIN=\"1234\"", &["+CME ERROR: 3"]);
        assert_eq!(
            bench.modem.next(bench.now, bench.utc),
            Step::Log("cell: AT+CPIN answered ERROR 3".into()),
            "the arguments, which may hold a PIN, are left out"
        );
        assert_eq!(bench.next(), Step::Wait(None), "and nothing else is told");
    }

    #[test]
    fn pin_hidden_where_formatted() {
        let mut bench = Bench::opened(Sim::default());
        bench.talk("AT+CPIN?", &["+CPIN: SIM PIN", "OK"]);
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        bench.talk("AT+EPINC", &["+EPINC: 2,3,10,10", "OK"]);
        let step = bench.next();
        assert!(matches!(&step, Step::Send(text) if text.text() == "AT+CPIN=\"4321\""), "the PIN goes to the SIM");
        let shown = format!("{step:?}");
        assert!(!shown.contains("4321"), "and tells nothing of it where formatted: {shown}");
        let mut state = State::<u8>::default();
        state.send(String::from("AT+CPIN=\"4321\""));
        let shown = format!("{:?}", state.take_wishes());
        assert!(!shown.contains("4321"), "nor where the console handed it over: {shown}");
    }

    #[test]
    fn absent_modem() {
        let mut modem = Modem::new(false, Sim::default(), Attaches::default());
        modem.take_wishes(Wishes { is_wanted: true, ask: Some(1), ..Wishes::default() }, SECOND, 0);
        assert_eq!(modem.next(SECOND, 0), Step::Refuse(1, NO_MODEM.into()));
        assert_eq!(modem.next(SECOND, 0), Step::Wait(None), "nothing starts");
        assert_eq!(modem.take_status(), Some(Status::Absent));
    }

    #[test]
    fn kept_pin_only_with_every_try() {
        let mut bench = Bench::opened(Sim { pin: Pin::parse("1234") });
        bench.talk("AT+CPIN?", &["+CPIN: SIM PIN", "OK"]);
        bench.talk("AT+EPINC", &["+EPINC: 3,3,10,10", "OK"]);
        bench.talk("AT+CPIN=\"1234\"", &["+CME ERROR: 16"]);
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::KeepSim(Sim::default())], "AT+CPIN?"),
            "a kept PIN the SIM refuses is forgotten"
        );
        bench.answer(&["+CPIN: SIM PIN", "OK"]);
        bench.talk("AT+EPINC", &["+EPINC: 2,3,10,10", "OK"]);
        assert_eq!(bench.next(), Step::Wait(None));
        assert_eq!(bench.modem.take_status(), Some(Status::Pin(2)), "two tries left: the owner is asked");
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        bench.talk("AT+EPINC", &["+EPINC: 2,3,10,10", "OK"]);
        bench.talk("AT+CPIN=\"4321\"", &["OK"]);
        let (steps, text) = bench.until_sent();
        let kept = Sim { pin: Pin::parse("4321") };
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::KeepSim(kept)], "AT+CPIN?"),
            "a typed PIN the SIM takes is kept, and asked for once"
        );
    }

    #[test]
    fn pin_kept_only_while_wanted() {
        let mut bench = Bench::opened(Sim { pin: Pin::parse("1234") });
        let typed = Wishes { is_wanted: true, pin: Pin::parse("4321"), ..Wishes::default() };
        bench.modem.take_wishes(typed, bench.now, bench.utc);
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::KeepSim(Sim::default())], "AT+CPIN?"),
            "a kept PIN is forgotten once none is to be kept"
        );
        bench.answer(&["+CPIN: SIM PIN", "OK"]);
        bench.talk("AT+EPINC", &["+EPINC: 3,3,10,10", "OK"]);
        bench.talk("AT+CPIN=\"4321\"", &["OK"]);
        let (steps, text) = bench.until_sent();
        assert_eq!((steps, text.as_str()), (vec![], "AT+CPIN?"), "and a typed one the SIM takes is not kept");
    }

    #[test]
    fn typed_pin_unasked() {
        let mut bench = Bench::ready();
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        assert!(bench.modem.typed.is_none(), "a PIN typed for a SIM that is ready is held for nothing");
        let mut bench = Bench::opened(Sim::default());
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        bench.talk("AT+CPIN?", &["+CPIN: READY", "OK"]);
        assert!(bench.modem.typed.is_none(), "nor one typed before the SIM turned out ready");
        let mut bench = Bench::opened(Sim::default());
        bench.wish(Wishes { pin: Pin::parse("4321"), ..Wishes::default() });
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert!(bench.modem.typed.is_none(), "nor one typed for a modem stopped since");
    }

    #[test]
    fn no_sim_and_busy() {
        let mut bench = Bench::opened(Sim::default());
        bench.talk("AT+CPIN?", &["+CME ERROR: 14"]);
        assert_eq!(bench.next(), Step::Wait(Some(BUSY_WAIT)), "a busy SIM is given its wait");
        bench.pass(BUSY_WAIT / 2);
        assert_eq!(bench.next(), Step::Wait(Some(BUSY_WAIT / 2)), "an event within it sends nothing");
        bench.pass(BUSY_WAIT / 2);
        assert_eq!(bench.sent(), "AT+CPIN?", "and once it passed, the SIM is asked again");
        bench.answer(&["+CME ERROR: 10"]);
        assert_eq!(bench.next(), Step::Wait(None));
        assert_eq!(bench.modem.take_status(), Some(Status::NoSim));
        assert!(!bench.modem.is_usable());
    }

    #[test]
    fn join_and_leave() {
        let mut bench = Bench::ready();
        bench.join(7);
        assert_eq!(bench.modem.take_status(), Some(Status::Online));
        assert!(!bench.modem.is_busy());
        assert_eq!(bench.modem.attaches.to_text(), "1800000000\n", "the attach is counted");
        bench.wish(Wishes { release: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()), "a stay begun asleep keeps nothing: no alarm");
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        bench.talk("AT+CFUN=4", &["OK"]);
        assert_eq!(bench.next(), Step::Wait(None));
        assert_eq!(bench.modem.take_status(), Some(Status::Ready));
    }

    #[test]
    fn awake_stay_keeps_the_attach() {
        let mut bench = Bench::ready();
        bench.is_awake = true;
        bench.join(1);
        bench.lines.clear();
        bench.wish(Wishes { release: true, ..Wishes::default() });
        let (end, down) = (bench.utc + KEEP.as_secs() + 1, "AT+EIF=100,\"ifst\",\"down\",1");
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Arm(Some(end)), Step::Unconfigure(interface())], down),
            "the alarm a second past the end, as the second of UTC is cut down"
        );
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(KEEP)), "the attach stays, idle");
        assert_eq!(bench.modem.take_status(), Some(Status::Attached));
        assert!(!bench.modem.is_busy(), "and the device may sleep on it");
        bench.modem.arm_again();
        assert_eq!(bench.next(), Step::Arm(Some(end)), "the clocks set, the alarm is set again to the same end");
        let kept_at = bench.now;
        bench.now += KEEP - Duration::from_millis(300);
        bench.modem.arm_again();
        assert_eq!(
            bench.next(),
            Step::Arm(Some(bench.utc + 2)),
            "never before the end: an alarm that came early waits it out"
        );
        bench.now = kept_at;
        bench.pass(KEEP / 3);
        bench.ask(2);
        bench.rejoin(2);
        assert_eq!(bench.modem.attaches.to_text(), "1800000000\n", "a stay within the keep attaches not");
        bench.wish(Wishes { release: true, ..Wishes::default() });
        let later = bench.utc + KEEP.as_secs() + 1;
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Arm(Some(later)), Step::Unconfigure(interface())], down),
            "each stay keeps it anew"
        );
        bench.answer(&["OK"]);
        bench.pass(KEEP);
        let (steps, text) = bench.until_sent();
        assert_eq!((steps, text.as_str()), (vec![Step::Arm(None)], "AT+CFUN=4"), "run out, the attach goes");
        assert_eq!(
            bench.lines.last().map(String::as_str),
            Some("cell: the attach kept 15 min after its last stay goes")
        );
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(None));
        assert_eq!(bench.modem.take_status(), Some(Status::Ready));
        bench.ask(3);
        assert_eq!(bench.sent(), "AT+CFUN=1", "and the next stay attaches anew");
    }

    #[test]
    fn asleep_stay_outlasts_keep() {
        let mut bench = Bench::ready();
        bench.is_awake = true;
        bench.join(1);
        bench.wish(Wishes { release: true, ..Wishes::default() });
        let (steps, text) = bench.until_sent();
        assert!(matches!(steps.as_slice(), [Step::Arm(Some(_)), Step::Unconfigure(_)]), "{steps:?}");
        assert_eq!(text, "AT+EIF=100,\"ifst\",\"down\",1");
        bench.answer(&["OK"]);
        bench.is_awake = false;
        bench.pass(KEEP / 2);
        bench.ask(2);
        bench.rejoin(2);
        bench.pass(KEEP);
        assert_eq!(bench.steps(), [Step::Arm(None)], "the keep runs out under the stay, which goes on");
        assert_eq!(bench.modem.take_status(), Some(Status::Online));
        bench.wish(Wishes { release: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()), "begun asleep, the stay keeps nothing of its own");
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4");
    }

    #[test]
    fn refusal_waits_the_whole_backoff() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"internet\",\"default\",0");
        bench.lines.clear();
        bench.answer(&["+CME ERROR: 4355"]);
        let (steps, text) = bench.until_sent();
        let reason = "the network refused the connection (Some(4355))";
        assert_eq!((steps, text.as_str()), (vec![Step::Refuse(1, reason.into())], "AT+CFUN=4"));
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(LAST_BACKOFF)), "the standard's wait, not the first of the doubling");
        assert!(!bench.modem.is_usable(), "no ask attaches meanwhile");
        let mut state = State::<u8>::default();
        state.want(true);
        state.note(&mut bench.modem);
        assert!(!state.is_usable(bench.now), "the radio asks for no link meanwhile");
        assert!(
            state.is_usable(bench.now + LAST_BACKOFF),
            "and asks once the boot time is past the wait, though the thread slept through its end"
        );
        bench.pass(LAST_BACKOFF);
        assert_eq!(bench.next(), Step::Wait(None), "the wait's end wakes the thread");
        assert!(
            state.note(&mut bench.modem) && !state.note(&mut bench.modem),
            "and is news once, the status unchanged, so that the main loop lends the modem again at once"
        );
        assert_eq!(
            bench.lines,
            [
                String::from("cell: AT+EAPNACT ended in Error(Some(4355))"),
                String::from("cell: refused after 1 attaches in the last day: none for 12 min"),
                format!("cell: no data connection: {reason}")
            ]
        );
    }

    #[test]
    fn unanswered_activation_backoff() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"internet\",\"default\",0");
        bench.pass(ACTIVATE_LIMIT);
        let (steps, text) = bench.until_sent();
        assert_eq!((steps, text.as_str()), (vec![Step::Refuse(1, NO_ANSWER.into())], "AT+CFUN=4"));
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(FIRST_BACKOFF)), "no refusal: the doubling's first wait");
    }

    #[test]
    fn lost_link() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.answer(&["+CEREG: 2"]);
        assert_eq!(bench.next(), Step::Wait(None), "a search keeps the link");
        bench.answer(&["+CGEV: NW PDN DEACT 0"]);
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Lose, Step::Unconfigure(interface())], "AT+EIF=100,\"ifst\",\"down\",1")
        );
        bench.answer(&["OK"]);
        bench.talk("AT+CFUN=4", &["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(FIRST_BACKOFF)));
        assert!(!bench.modem.is_usable(), "a loss is waited out");
        bench.now += FIRST_BACKOFF;
        assert_eq!(bench.next(), Step::Wait(None));
        assert!(bench.modem.is_usable());
    }

    #[test]
    fn losses_by_their_ids() {
        let down = "AT+EIF=100,\"ifst\",\"down\",1";
        for line in ["+EIF: 100, ifdown, 0", "+CGEV: NW DETACH", "+CGEV: ME DETACH", "+CGEV: ME PDN DEACT 0"] {
            let mut bench = Bench::ready();
            bench.join(1);
            bench.answer(&["+EIF: 101, ifdown, 0", "+CGEV: NW PDN DEACT 5"]);
            assert_eq!(
                bench.next(),
                Step::Wait(None),
                "another interface's end and another connection's keep the link"
            );
            bench.answer(&[line]);
            let (steps, text) = bench.until_sent();
            assert_eq!((steps, text.as_str()), (vec![Step::Lose, Step::Unconfigure(interface())], down), "{line}");
        }
        let mut idle = Bench::ready();
        idle.answer(&["+CGEV: NW DETACH", "+EIF: 100, ifdown, 0", "+CGEV: NW PDN DEACT 0"]);
        assert_eq!(idle.next(), Step::Wait(None), "with no connection there is none to lose");
    }

    #[test]
    fn registration_denied_and_late() {
        let mut bench = Bench::ready();
        bench.ask(3);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,3", "OK"]);
        let (steps, text) = bench.until_sent();
        assert_eq!((steps, text.as_str()), (vec![Step::Refuse(3, REGISTRATION_DENIED.into())], "AT+CFUN=4"));
        bench.answer(&["OK"]);
        bench.now += LAST_BACKOFF;
        bench.ask(4);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,2", "OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(ATTACH_LIMIT)));
        bench.now += ATTACH_LIMIT;
        let (steps, text) = bench.until_sent();
        assert_eq!((steps, text.as_str()), (vec![Step::Refuse(4, NOT_REGISTERED.into())], "AT+CFUN=4"));
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(2 * FIRST_BACKOFF)), "the backoff doubles");
    }

    #[test]
    fn ask_too_early() {
        let mut bench = Bench::opened(Sim::default());
        bench.ask(1);
        assert_eq!(bench.until_sent().0, [Step::Refuse(1, NOT_READY.into())]);
    }

    #[test]
    fn ipv6_alone_grants_servers() {
        let mut bench = Bench::ready();
        bench.ask(2);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet.v6\",2,0", "+CEREG: 1,1", "OK"]);
        bench.talk(
            "AT+EAPNACT=1,\"internet.v6\",\"default\",0",
            &["+EIF: 100, ifup, 2, FE:80:00:00:00:00:00:00:00:00:00:00:00:00:00:02, 0", "+CGEV: ME PDN ACT 0", "OK"],
        );
        bench.talk("AT+EIF=100,\"ifst\",\"up\",2", &["OK"]);
        bench.talk("AT+EPDN=0,\"ifst\",20", &["OK"]);
        bench.talk(
            "AT+CGCONTRDP=0",
            &["+CGCONTRDP: 0,5,\"internet.v6.mnc001.mcc001.gprs\",\"\",\"\",\"2001:db8::53\",\"\"", "OK"],
        );
        assert!(matches!(bench.next(), Step::Configure(interface) if interface.ipv4.is_none()));
        bench.modem.take_configured(Ok(()), bench.now, bench.utc);
        assert_eq!(
            bench.next(),
            Step::Grant(2, 0, Some(vec!["2001:db8::53".parse().unwrap()])),
            "a connection of IPv6 alone comes with the servers that tell the network's NAT64"
        );
    }

    #[test]
    fn contexts_name_the_apn() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,5", "OK"]);
        bench.talk("AT+CGCONTRDP", &["+CGCONTRDP: 1,5,\"web.mnc001.mcc001.gprs\"", "OK"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"web\",\"default\",0");
    }

    #[test]
    fn apn_named_after_the_registration() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,1", "OK"]);
        bench.talk("AT+CGCONTRDP", &["OK"]);
        assert_eq!(
            bench.next(),
            Step::Wait(Some(ATTACH_LIMIT)),
            "no connection named yet: the attach's APN is awaited"
        );
        bench.answer(&["+EIAREG: ME ATTACH \"internet\",3,0"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"internet\",\"default\",0");
        let mut bench = Bench::ready();
        bench.ask(2);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,1", "OK"]);
        bench.talk("AT+CGCONTRDP", &["+EIAREG: ME ATTACH \"internet\",3,0", "OK"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"internet\",\"default\",0", "named while the question was out");
        let mut bench = Bench::ready();
        bench.ask(3);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+CEREG: 1,1", "OK"]);
        bench.talk("AT+CGCONTRDP", &["OK"]);
        bench.now += ATTACH_LIMIT;
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Refuse(3, NO_APN.into())], "AT+CFUN=4"),
            "never named, the join fails"
        );
    }

    #[test]
    fn interface_after_activation() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        bench.talk("AT+EAPNACT=1,\"internet\",\"default\",0", &["OK"]);
        assert_eq!(bench.next(), Step::Wait(Some(INTERFACE_LIMIT)));
        bench.answer(&["+EIF: 100, ifup, 1, 10.1.2.3, 0"]);
        assert_eq!(bench.sent(), "AT+EIF=100,\"ifst\",\"up\",1");
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CGCONTRDP", "no id named: every connection");
    }

    #[test]
    fn taken_back_during_the_join() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        assert_eq!(bench.sent(), "AT+CEREG?");
        bench.wish(Wishes { give_up: true, ..Wishes::default() });
        bench.answer(&["+CEREG: 1,1", "OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4", "nothing activates for an ask taken back");
        bench.answer(&["OK"]);
        assert_eq!(bench.next(), Step::Wait(None), "and nothing is waited out");
        assert!(bench.modem.is_usable());
    }

    #[test]
    fn configure_after_give_up() {
        let mut bench = Bench::ready();
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        bench.talk(
            "AT+EAPNACT=1,\"internet\",\"default\",0",
            &["+EIF: 100, ifup, 1, 10.1.2.3, 0", "+CGEV: ME PDN ACT 0", "OK"],
        );
        bench.talk("AT+EIF=100,\"ifst\",\"up\",1", &["OK"]);
        bench.talk("AT+EPDN=0,\"ifst\",20", &["OK"]);
        bench.talk("AT+CGCONTRDP=0", &["+CGCONTRDP: 0,5,\"internet\",\"\",\"\",\"\",\"\",\"\",\"\",0,0,1430", "OK"]);
        bench.wish(Wishes { give_up: true, ..Wishes::default() });
        assert_eq!(
            bench.sent(),
            "AT+EIF=100,\"ifst\",\"down\",1",
            "no interface is set up for an ask given up as its connection came"
        );
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4");
    }

    #[test]
    fn grant_failure() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.modem.take_grant(Err("no descriptor".into()));
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Unconfigure(interface())], "AT+EIF=100,\"ifst\",\"down\",1"),
            "a link the radio was not handed is let go of"
        );
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4");
        assert_eq!(bench.modem.take_status(), Some(Status::Ready));
    }

    #[test]
    fn stale_activation_dropped() {
        let mut bench = Bench::ready();
        bench.is_awake = true;
        bench.ask(1);
        bench.talk("AT+CFUN=1", &["OK"]);
        bench.talk("AT+CEREG?", &["+EIAREG: ME ATTACH \"internet\",3,0", "+CEREG: 1,1", "OK"]);
        assert_eq!(bench.sent(), "AT+EAPNACT=1,\"internet\",\"default\",0");
        bench.answer(&["+EIF: 100, ifup, 1, 10.1.2.3, 0", "+CGEV: ME PDN ACT 0"]);
        bench.wish(Wishes { give_up: true, ask: Some(2), ..Wishes::default() });
        bench.answer(&["OK"]);
        assert!(matches!(bench.next(), Step::Arm(Some(_))), "the earlier ask, let go of awake, keeps the attach");
        bench.rejoin(2);
        assert_eq!(
            bench.next(),
            Step::Wait(Some(KEEP)),
            "the earlier ask's activation, answered late, set up nothing more"
        );
        assert!(bench.modem.is_granted(), "and the link stays granted");
    }

    #[test]
    fn stale_radio_on_dropped() {
        let mut bench = Bench::ready();
        bench.ask(1);
        assert_eq!(bench.sent(), "AT+CFUN=1");
        bench.wish(Wishes { give_up: true, ask: Some(2), ..Wishes::default() });
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=1", "the later ask turns the radio on itself");
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CEREG?");
        assert_eq!(bench.modem.attaches.to_text(), "1800000000\n", "one attach counted");
        let mut bench = Bench::ready();
        bench.ask(1);
        assert_eq!(bench.sent(), "AT+CFUN=1");
        bench.wish(Wishes { give_up: true, ..Wishes::default() });
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4", "a radio no ask wants goes off again");
    }

    #[test]
    fn kept_join_lost_out_of_network() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.wish(Wishes { release: true, holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()));
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        bench.wish(Wishes { ask: Some(2), holds_attach: true, ..Wishes::default() });
        assert_eq!(bench.sent(), "AT+EIF=100,\"ifst\",\"up\",1");
        bench.answer(&["+CEREG: 0", "OK"]);
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Refuse(2, "the network let the device go".into())], "AT+CFUN=4"),
            "out of the network, a join on the kept attach fails"
        );
    }

    #[test]
    fn refused_radio_off_counts_on() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.wish(Wishes { release: true, ..Wishes::default() });
        assert_eq!(bench.next(), Step::Unconfigure(interface()));
        bench.talk("AT+EIF=100,\"ifst\",\"down\",1", &["OK"]);
        bench.talk("AT+CFUN=4", &["+CME ERROR: 3"]);
        assert_eq!(bench.next(), Step::Wait(None));
        bench.ask(2);
        assert_eq!(bench.sent(), "AT+CEREG?", "a radio the modem kept on is not turned on again");
        bench.wish(Wishes { give_up: true, ..Wishes::default() });
        bench.answer(&["OK"]);
        assert_eq!(bench.sent(), "AT+CFUN=4", "and the next leave turns it off");
    }

    #[test]
    fn nothing_sent_after_power_off() {
        let mut bench = Bench::opened(Sim { pin: Pin::parse("1234") });
        assert_eq!(bench.sent(), "AT+CPIN?");
        let stop = |commands| Wishes { keeps_pin: true, commands, ..Wishes::default() };
        bench.modem.take_wishes(stop(Vec::new()), bench.now, bench.utc);
        bench.answer(&["+CPIN: SIM PIN", "OK"]);
        assert_eq!(bench.sent(), "AT+EPOF", "the SIM asked as the stop began is heard no more");
        bench.modem.take_wishes(stop(vec![at::Command::new("AT+CPSMS?")]), bench.now, bench.utc);
        bench.answer(&["OK"]);
        assert_eq!(bench.steps(), [Step::Close, Step::Stop(true)]);
        assert_eq!(bench.next(), Step::Wait(None), "and nothing is sent after AT+EPOF");
        let refused = format!("cell: AT+CPSMS? is not sent: {NOT_READY}");
        assert!(bench.lines.contains(&refused), "a console command is told not sent: {:?}", bench.lines);
    }

    #[test]
    fn open_dropped_by_a_stop() {
        let mut bench = Bench::started(Sim::default());
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(
            bench.steps(),
            [Step::Close, Step::Stop(false)],
            "a channel not opened yet is not opened for a stop"
        );
        let mut bench = Bench::started(Sim::default());
        bench.modem.take_event(Event::Exception, "");
        assert_eq!(bench.steps(), [Step::Close], "nor for a failure");
    }

    #[test]
    fn port_error_fails() {
        let mut bench = Bench::ready();
        bench.modem.take_port_error("reading /dev/ttyC0: Input/output error");
        assert_eq!(bench.steps(), [Step::Close]);
        assert_eq!(bench.modem.take_status(), Some(Status::Failed));
        assert_eq!(bench.lines.last().map(String::as_str), Some("cell: reading /dev/ttyC0: Input/output error"));
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.steps(), [Step::Stop(false)], "switched off, the modem is stopped, to start anew");
    }

    #[test]
    fn port_error_in_a_stop() {
        let mut bench = Bench::ready();
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.sent(), "AT+EPOF");
        bench.modem.take_port_error("reading /dev/ccci_fs: Input/output error");
        bench.answer(&["OK"]);
        assert_eq!(bench.steps(), [Step::Close, Step::Stop(true)], "the stop goes on");
        bench.modem.take_stop(Ok(()));
        bench.modem.take_event(Event::Stop, "");
        assert_eq!(bench.steps(), [Step::Unload], "its end heard");
        assert_eq!(bench.modem.take_status(), Some(Status::Off), "and the modem off, not failed");
    }

    #[test]
    fn unsent_command() {
        let unsent = || Err(format!("writing to /dev/ttyC0: {NO_CHANNEL}"));
        let mut bench = Bench::ready();
        bench.ask(1);
        assert_eq!(bench.sent(), "AT+CFUN=1");
        bench.modem.take_sent(Ok(()));
        assert_eq!(bench.next(), Step::Wait(Some(ANSWER_LIMIT)), "a command that left is waited for");
        bench.lines.clear();
        bench.modem.take_sent(unsent());
        assert_eq!(
            bench.steps(),
            [Step::Refuse(1, "the radio does not come on".into())],
            "one that did not leave ends at once, as one unanswered"
        );
        assert_eq!(
            bench.lines[..2],
            [
                String::from("cell: writing to /dev/ttyC0: the AT channel is not open"),
                String::from("cell: AT+CFUN ended in no answer")
            ]
        );
        let mut bench = Bench::ready();
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.sent(), "AT+EPOF");
        bench.modem.take_port_error("reading /dev/ttyC0: Input/output error");
        bench.modem.take_sent(unsent());
        assert_eq!(
            bench.steps(),
            [Step::Close, Step::Stop(true)],
            "a stop whose channel is gone waits for no answer to what it could not send"
        );
    }

    #[test]
    fn thread_end() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.modem.end("interrupted");
        assert_eq!(
            bench.steps(),
            [Step::Lose, Step::Unconfigure(interface()), Step::Close, Step::Stop(false), Step::Unload],
            "the radio hears its link is lost, the interface is let go of, the modem stopped and its files written"
        );
        assert_eq!(bench.lines.last().map(String::as_str), Some("cell: waiting: interrupted"));
        let mut bench = Bench::new(Sim::default());
        bench.modem.end("interrupted");
        assert_eq!(bench.steps(), [Step::Unload], "a modem never started is not stopped");
    }

    #[test]
    fn sbp_read() {
        let mut bench = Bench::new(Sim::default());
        bench.wish(Wishes::default());
        assert_eq!(bench.next(), Step::Seed);
        bench.modem.take_seed(Ok(()));
        assert_eq!(bench.next(), Step::ReadSbp);
        bench.modem.take_sbp([Err("bad block".into()), Ok(Some(9u32.to_le_bytes().to_vec()))]);
        assert_eq!(bench.next(), Step::Start(ccci::boot_data(9)), "the code of the file that was read");
        assert_eq!(bench.lines, ["cell: the SBP code's APCFG/APRDCL/CXP_SBP: bad block"], "and the failure told");
    }

    #[test]
    fn stop_powers_off() {
        let mut bench = Bench::ready();
        bench.join(1);
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert!(bench.modem.attach_apn.is_none(), "the next start's SIM names its own APN");
        let (steps, text) = bench.until_sent();
        assert_eq!(
            (steps, text.as_str()),
            (vec![Step::Lose, Step::Unconfigure(interface())], "AT+EIF=100,\"ifst\",\"down\",1")
        );
        bench.answer(&["OK"]);
        bench.talk("AT+CFUN=4", &["OK"]);
        bench.talk("AT+EPOF", &["OK"]);
        assert_eq!(bench.steps(), [Step::Close, Step::Stop(true)]);
        bench.modem.take_stop(Ok(()));
        assert!(!bench.modem.is_busy() && bench.modem.is_running(), "the kernel stops it, awake on its own");
        bench.modem.take_event(Event::Stop, "");
        assert_eq!(bench.steps(), [Step::Unload], "its files written once it is stopped");
        assert!(!bench.modem.is_running());
        assert_eq!(bench.modem.take_status(), Some(Status::Off));
    }

    #[test]
    fn refused_stop_fails() {
        let mut bench = Bench::ready();
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        bench.talk("AT+EPOF", &["OK"]);
        assert_eq!(bench.steps(), [Step::Close, Step::Stop(true)]);
        bench.modem.take_stop(Err("refused".into()));
        assert_eq!(bench.modem.take_status(), Some(Status::Failed));
        assert!(bench.modem.is_running(), "it may still run");
    }

    #[test]
    fn exception_fails_until_wanted_anew() {
        let mut bench = Bench::ready();
        bench.modem.take_event(Event::Exception, "");
        assert_eq!(bench.steps(), [Step::Close]);
        assert_eq!(bench.modem.take_status(), Some(Status::Failed));
        bench.ask(1);
        assert_eq!(bench.steps(), [Step::Refuse(1, NOT_READY.into())], "nothing restarts while wanted");
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.steps(), [Step::Stop(false)], "a failed modem is stopped by force");
        bench.modem.take_stop(Ok(()));
        bench.modem.take_event(Event::Stop, "");
        bench.wish(Wishes::default());
        assert_eq!(bench.next(), Step::Unload);
        assert!(matches!(bench.start(), Step::Start(_)), "and starts again once wanted anew");
    }

    #[test]
    fn second_failure_still_stops() {
        let mut bench = Bench::ready();
        bench.modem.take_event(Event::Exception, "");
        assert_eq!(bench.steps(), [Step::Close]);
        bench.modem.take_port_error("reading /dev/ccci_fs: Input/output error");
        assert!(bench.modem.is_running(), "a modem that failed twice may still run");
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.steps(), [Step::Stop(false)], "and is stopped once it is wanted no more");
        let mut bench = Bench::ready();
        bench.modem.take_event(Event::Exception, "");
        assert_eq!(bench.steps(), [Step::Close]);
        bench.modem.end("interrupted");
        assert_eq!(bench.steps(), [Step::Stop(false), Step::Unload], "as it is where the thread ends after a failure");
    }

    #[test]
    fn failed_start_needs_no_stop() {
        let mut bench = Bench::new(Sim::default());
        bench.wish(Wishes::default());
        assert_eq!(bench.next(), Step::Seed);
        bench.modem.take_seed(Err("no partition".into()));
        assert_eq!(bench.next(), Step::Wait(None), "no start after a failure");
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.next(), Step::Wait(None), "nothing to stop");
        assert_eq!(bench.modem.take_status(), Some(Status::Off));
        bench.wish(Wishes::default());
        assert_eq!(bench.next(), Step::Seed);
    }

    #[test]
    fn first_command_waits_for_at() {
        let mut bench = Bench::channel_opened(Sim::default());
        assert_eq!(bench.next(), Step::Wait(Some(AT_READY_LIMIT)), "nothing is sent before +EIND: 128");
        assert!(bench.modem.is_busy());
        let word_at = Duration::from_millis(1500);
        bench.now += word_at;
        bench.answer(&["+EIND: 64"]);
        assert_eq!(bench.next(), Step::Wait(Some(AT_READY_LIMIT - word_at)), "another event changes nothing");
        bench.answer(&["+EIND: 128"]);
        assert_eq!(bench.sent(), "ATE0Q0V1");
        assert_eq!(
            bench.lines.last().map(String::as_str),
            Some("cell: the AT channel takes commands 1500 ms after it opened")
        );
        let mut bench = Bench::channel_opened(Sim::default());
        bench.now += AT_READY_LIMIT;
        assert_eq!(bench.sent(), "ATE0Q0V1", "a word waited out sends the first command regardless");
        let mut bench = Bench::channel_opened(Sim::default());
        bench.modem.take_wishes(Wishes::default(), bench.now, bench.utc);
        assert_eq!(bench.steps(), [Step::Close, Step::Stop(false)], "a stop while waiting");
        bench.answer(&["+EIND: 128"]);
        bench.now += AT_READY_LIMIT;
        assert_eq!(bench.next(), Step::Wait(None), "and nothing is sent after it");
    }

    #[test]
    fn silent_modem() {
        let mut bench = Bench::channel_opened(Sim::default());
        bench.answer(&["+EIND: 128"]);
        assert_eq!(bench.sent(), "ATE0Q0V1");
        assert_eq!(bench.next(), Step::Wait(Some(ANSWER_LIMIT)));
        bench.now += ANSWER_LIMIT;
        assert_eq!(bench.steps(), [Step::Close]);
        assert_eq!(bench.modem.take_status(), Some(Status::Failed));
    }

    #[test]
    fn shared_state() {
        let mut state = State::<u8>::default();
        assert!(state.is_off() && !state.is_usable(SECOND) && !state.is_running());
        assert!(state.want(true) && !state.want(true));
        assert!(state.is_off(), "a wish before the thread comes to its work keeps no sleep off");
        let number = state.ask_link();
        let wishes = state.take_wishes();
        assert_eq!((wishes.is_wanted, wishes.ask), (true, Some(number)));
        let mut modem = Bench::ready().modem;
        modem.take_wishes(Wishes { is_wanted: true, ask: Some(number), ..Wishes::default() }, SECOND, 0);
        assert!(state.note(&mut modem), "a new status is news");
        assert!(!state.note(&mut modem), "once");
        assert!(state.is_usable(SECOND) && state.is_running() && !state.is_off(), "a join under way");
        assert_eq!(state.take_answer(number), None);
        state.answer(number, Ok(5));
        assert_eq!(state.take_answer(number), Some(Ok(5)));
        assert_eq!(state.take_report().status, Some(Status::Attaching));
        assert_eq!(state.take_report().status, None, "once");
        let number = state.ask_link();
        assert_eq!(state.take_back(number), (None, false), "the thread need not hear of an ask it never took");
        assert_eq!(
            state.take_wishes(),
            Wishes { is_wanted: true, keeps_pin: true, ..Wishes::default() },
            "an ask the thread never took is dropped here"
        );
        assert!(
            !state.keep_pin(true) && state.keep_pin(false) && !state.take_wishes().keeps_pin,
            "a PIN no longer to be kept is news"
        );
        let number = state.ask_link();
        state.take_wishes();
        assert_eq!(state.take_back(number), (None, true), "the thread hears of one it took");
        assert!(state.take_wishes().give_up, "one it took is given up");
        state.answer(number, Ok(6));
        assert_eq!(state.take_answer(number), None);
        assert!(state.take_wishes().release, "and a link granted for it after is let go");
        assert!(state.hold_attach(true) && !state.hold_attach(true), "a hold of the attach is news once");
        assert!(state.set_awake(true) && !state.set_awake(true), "and so is the device awake");
        state.send(String::from("AT"));
        let wishes = state.take_wishes();
        let held = (true, true, vec![at::Command::new("AT")]);
        assert_eq!((wishes.holds_attach, wishes.is_awake, wishes.commands), held);
        assert!(state.take_wishes().commands.is_empty(), "each command is handed over once");
        state.hold_attach(false);
        let mut idle = Modem::new(true, Sim::default(), Attaches::default());
        state.note(&mut idle);
        assert!(!state.is_off(), "a wish the thread has not taken yet keeps it from sleep");
        state.take_wishes();
        state.note(&mut idle);
        assert!(state.is_off() && !state.is_usable(SECOND) && !state.is_running());
        let number = state.ask_link();
        state.end_thread();
        assert_eq!(state.take_answer(number), Some(Err(THREAD_GONE.into())), "nobody waits on a thread that has ended");
        assert_eq!(state.take_answer(number), None);
        assert_eq!(state.take_report().status, Some(Status::Ended), "the screens show its end");
        assert!(
            state.set_awake(false) && state.want(false) && state.is_off(),
            "and a wish after its end keeps no sleep off"
        );
    }

    #[test]
    fn rest_is_news() {
        let mut state = State::<u8>::default();
        let mut bench = Bench::ready();
        assert!(state.note(&mut bench.modem), "a modem that came ready is news");
        bench.wish(Wishes { commands: vec![at::Command::new("AT")], ..Wishes::default() });
        assert!(!state.note(&mut bench.modem) && !state.is_off(), "a command to send keeps sleep off");
        bench.talk("AT", &["OK"]);
        assert!(!state.note(&mut bench.modem), "and its answer, to be logged, too");
        assert_eq!(bench.next(), Step::Wait(None));
        assert!(state.note(&mut bench.modem) && state.is_off(), "the thread at rest is news, the status unchanged");
        assert!(!state.note(&mut bench.modem), "once");
    }

    #[test]
    fn wish_before_first_note() {
        let mut state = State::<u8>::default();
        assert!(state.set_awake(true) && state.set_awake(false), "the device fell asleep");
        assert!(state.is_off(), "a thread still waiting for the partition keeps no suspend off");
        let mut idle = Modem::new(true, Sim::default(), Attaches::default());
        state.note(&mut idle);
        assert!(!state.is_off(), "the thread at its work takes the wish first");
        state.take_wishes();
        state.note(&mut idle);
        assert!(state.is_off());
    }
}
