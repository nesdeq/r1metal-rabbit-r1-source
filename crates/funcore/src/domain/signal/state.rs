//! State shared by the Signal thread and the main loop.

use super::auto::{self, Auto, Limits};
use super::list::{
    CallRow, Contactable, ConversationView, MAX_MESSAGES, Summary, to_call_rows, to_callees, to_contacts,
    to_conversation_view, to_summaries,
};
use super::pictures::{self, Photo};
use super::robot::{self, Question, Reply};
use super::voices;
use crate::base::time::SECONDS_PER_MINUTE;
use crate::domain::cause::Cause;
use crate::gfx::palette::Roles;
use crate::openai::fault::Fault;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::attachment::{Finish, Job};
use crate::signal::command::{Command, Outbound, Outgoing};
use crate::signal::connection::Status;
use crate::signal::device::{Device, Event, Incoming, Membership};
use crate::signal::link::{self, Codes};
use crate::signal::service::Relay;
use alloc::collections::{BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

const RETRY_WAIT: Duration = Duration::from_secs(30);
/// The log line of a device the service refuses as unknown.
pub const UNLINKED_LINE: &str = "signal: the service no longer knows this device: unlinked";
/// The log line of an alarm that fetches the waiting messages.
pub const FETCH_LINE: &str = "signal: the alarm fetches what waits";
/// The log line of files found without an account.
pub const LEFTOVERS_LINE: &str =
    "signal: files without an account are taken away, as an unlink cut short or a link that ended left them";

/// The account's state, as the files give it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LinkStatus {
    #[default]
    Reading,
    Unlinked,
    Linked,
}

/// The step of a link under way.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum LinkStep {
    #[default]
    Idle,
    Connecting,
    // The QR code's text.
    Code(String),
    // The phone sent the account: the R1 makes its keys.
    MakingKeys,
    Failed(Cause),
}

impl LinkStep {
    /// A link whose keys the library did not make, told as any failure the service's answers do not name.
    pub const NO_KEYS: LinkStep = LinkStep::Failed(Cause::Refused);
    /// A link whose new account could not be written.
    pub const UNWRITTEN: LinkStep = LinkStep::Failed(Cause::NoStorage);

    /// A link failed by `fault`: refused where the service refused it, else for want of the network.
    pub const fn from_fault(fault: &Fault) -> LinkStep {
        LinkStep::Failed(Cause::from_fault(fault, Cause::NoNetwork))
    }

    /// A link the service refused.
    pub const fn from_refusal(refusal: link::Refusal) -> LinkStep {
        LinkStep::Failed(match refusal {
            link::Refusal::TooOld => Cause::TooOld,
            link::Refusal::TooManyDevices => Cause::TooManyDevices,
            link::Refusal::Other => Cause::Refused,
        })
    }
}

/// The Signal screen that needs the thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignalView {
    Nothing,
    Link,
    List,
    // The contacts a new message may go to, to choose from.
    Contacts,
    Conversation(Peer),
}

/// What the thread needs of the network.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    Nothing,
    // The provisioning socket, for a link.
    Provision,
    // The account's socket.
    Connect,
}

impl Need {
    /// The thread's work for this need, holding that device.
    pub fn to_work<D>(self, device: Option<&mut D>) -> Work<'_, D> {
        match (self, device) {
            (Need::Provision, None) => Work::Link,
            (Need::Connect, Some(device)) => Work::Stay(device),
            _ => Work::Rest,
        }
    }
}

/// The thread's next work.
#[derive(Debug, PartialEq, Eq)]
pub enum Work<'a, D> {
    Link,
    Stay(&'a mut D),
    Rest,
}

/// The thread's step while nothing is due.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Await {
    Work(Need),
    // Sets the alarm for that UTC time, or clears it, and sleeps.
    Arm(Option<u64>),
}

/// The next step of a link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodeStep {
    // Its window closed.
    Stop,
    // Every code was shown, and the link has failed for it.
    Expired,
    // The next code goes up.
    Open,
}

/// A stay's next step after a round.
pub struct Round {
    /// True if a turn of auto mode waits for its writer.
    pub is_turn_due: bool,
    pub should_leave: bool,
    pub wait_ms: u64,
}

/// The thread's status for the screens.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ThreadStatus {
    pub link_status: LinkStatus,
    pub link_step: LinkStep,
    pub is_connected: bool,
    pub failure: Option<Cause>,
    pub unread: usize,
    pub auto: Option<ServiceId>,
}

/// The Signal thread's report for the screens.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The conversations, the pinned first in their order, then the newest first, when they changed.
    pub summaries: Option<Vec<Summary>>,
    /// The open conversation, when it changed.
    pub conversation: Option<ConversationView>,
    /// The contacts a new message may go to, when they changed while their list shows.
    pub contacts: Option<Vec<Contactable>>,
    /// Contacts' new messages: conversation, text, and whether the notice tells it.
    pub arrivals: Vec<(Peer, String, bool)>,
    /// What Signal answers the robot's question.
    pub reply: Option<Reply>,
    /// The numbers of the playings of voice notes that ended.
    pub ended: Vec<u32>,
    /// The pictures photos were decoded to, none where one could not be, for the width they stand at.
    pub pictures: Option<(Vec<Photo>, usize)>,
    /// Photos that went up, each from the file it went up from to the one its message holds it in, told with the conversation that holds it there.
    pub moved: Vec<(String, String)>,
    pub status: ThreadStatus,
}

impl core::fmt::Debug for Report {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Report")
            .field("summaries", &self.summaries)
            .field("opened", &self.conversation)
            .field("contacts", &self.contacts.as_ref().map(Vec::len))
            .field("arrived", &self.arrivals.len())
            .field("reply", &self.reply)
            .field("ended", &self.ended.len())
            .field("pictures", &self.pictures.as_ref().map(|(drawn, _)| drawn.len()))
            .field("moved", &self.moved.len())
            .finish_non_exhaustive()
    }
}

/// What the Signal thread has for the call layer, and the call log for the screens.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CallReport {
    // The call messages that passed the gate, as they came.
    pub(crate) signals: Vec<Incoming>,
    // The relays of a one-to-one call, when they were asked; none where none could be had.
    pub(crate) relays: Option<Vec<Relay>>,
    // What the groups' calls asked for are joined with, each none where its token could not be had.
    pub(crate) tokens: Vec<(Peer, Option<Membership>)>,
    // The call's signals that were not sent and go no more.
    pub(crate) unsent: Vec<Outbound>,
    /// The call log, the newest first, when it changed.
    pub log: Option<Vec<CallRow>>,
    /// The contacts and groups a call may go to, when they changed.
    pub callees: Option<Vec<Contactable>>,
}

impl core::fmt::Debug for CallReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CallReport")
            .field("signals", &self.signals.len())
            .field("relays", &self.relays.as_ref().map(Vec::len))
            .field("tokens", &self.tokens.len())
            .field("unsent", &self.unsent.len())
            .field("log", &self.log.as_ref().map(Vec::len))
            .field("callees", &self.callees.as_ref().map(Vec::len))
            .finish()
    }
}

/// State shared by the Signal thread and the main loop.
pub struct State {
    commands: VecDeque<Command>,
    is_awake: bool,
    // A call stands or the calls' screen shows, which holds the socket.
    is_calling: bool,
    view: SignalView,
    // What the conversations are published for changed since they last were: the screen that shows, or whether photos and voice notes are fetched and of which conversation.
    is_republish_due: bool,
    is_catching_up: bool,
    has_radio: bool,
    fetch_every_min: u8,
    last_fetch_s: u64,
    link_status: LinkStatus,
    link_step: LinkStep,
    is_connected: bool,
    failure: Option<Cause>,
    summaries: Vec<Summary>,
    conversation: Option<ConversationView>,
    contacts: Vec<Contactable>,
    calls: Vec<CallRow>,
    callees: Vec<Contactable>,
    // Whether the photos and voice notes of the conversation that shows are fetched, which the settings' text alone has not.
    is_fetching: bool,
    // The transfers of attachments the thread that carries them is to carry out, those it carries out, what they ended in that the device has not taken, and those that broke or whose fetch was refused, held until the next stay.
    jobs: VecDeque<Job>,
    working: Vec<Job>,
    done: Vec<(Job, Finish)>,
    held: Vec<Job>,
    // The files photos went up from and those their messages hold them in, for the next publishing.
    moved: Vec<(String, String)>,
    question: Option<Question>,
    voices: voices::State,
    pictures: pictures::State,
    errand_count: usize,
    is_busy: bool,
    auto: Auto,
    has_reply_waiting: bool,
    is_ended: bool,
    // The turn of auto mode its writer is handed.
    turn: Option<(ServiceId, String)>,
    pub report: Report,
    call_report: CallReport,
}

impl Default for State {
    fn default() -> State {
        State {
            commands: VecDeque::new(),
            is_awake: true,
            is_calling: false,
            view: SignalView::Nothing,
            is_republish_due: false,
            is_catching_up: true,
            has_radio: false,
            fetch_every_min: 0,
            last_fetch_s: 0,
            link_status: LinkStatus::Reading,
            link_step: LinkStep::Idle,
            is_connected: false,
            failure: None,
            summaries: Vec::new(),
            conversation: None,
            contacts: Vec::new(),
            calls: Vec::new(),
            callees: Vec::new(),
            is_fetching: true,
            jobs: VecDeque::new(),
            working: Vec::new(),
            done: Vec::new(),
            held: Vec::new(),
            moved: Vec::new(),
            question: None,
            voices: voices::State::default(),
            pictures: pictures::State::default(),
            errand_count: 0,
            is_busy: false,
            auto: Auto::default(),
            has_reply_waiting: false,
            is_ended: false,
            turn: None,
            report: Report::default(),
            call_report: CallReport::default(),
        }
    }
}

impl State {
    pub fn set_link_step(&mut self, step: LinkStep) {
        self.link_step = step;
    }

    /// Takes the report, with the thread's status.
    pub fn take_report(&mut self) -> Report {
        let status = ThreadStatus {
            link_status: self.link_status,
            link_step: self.link_step.clone(),
            is_connected: self.is_connected,
            failure: self.failure,
            unread: self.unread_count(),
            auto: self.auto.contact,
        };
        Report { status, ..core::mem::take(&mut self.report) }
    }

    /// Takes what the thread has for the call layer, and the call log for the screens.
    pub fn take_call_report(&mut self) -> CallReport {
        core::mem::take(&mut self.call_report)
    }

    /// Sets whether the call layer holds the socket, a call standing or the calls' screen showing; true if that is new.
    pub fn set_calling(&mut self, is_calling: bool) -> bool {
        core::mem::replace(&mut self.is_calling, is_calling) != is_calling
    }

    // A signal, relays or a token the call layer asked, which waits for no later stay.
    fn has_call_asks(&self) -> bool {
        self.commands.iter().any(|command| matches!(command, Command::Signal(_) | Command::Relays | Command::Token(_)))
    }

    // Lets go of what the call layer asked of the network that no stay carries now: a signal is told not sent, relays and a token told none had.
    fn drop_call_asks(&mut self) {
        for command in core::mem::take(&mut self.commands) {
            match command {
                Command::Signal(outbound) => self.call_report.unsent.push(outbound),
                Command::Relays => self.call_report.relays = Some(Vec::new()),
                Command::Token(peer) => self.call_report.tokens.push((peer, None)),
                command => self.commands.push_back(command),
            }
        }
    }

    /// Adds the screens' commands: auto mode switches at once, and off where the owner writes there.
    pub fn add_commands(&mut self, commands: impl IntoIterator<Item = Command>) {
        for command in commands {
            match &command {
                Command::Auto(contact) => self.auto.set(*contact),
                Command::Send(Outgoing { peer: Peer::Contact(contact), is_robot: false, .. }) => {
                    self.auto.note_owner_wrote(*contact)
                }
                Command::Read(_) if !self.is_awake => continue,
                _ => {}
            }
            self.commands.push_back(command);
        }
    }

    /// Sets what the main loop wants of Signal and whether the photos and voice notes of the conversation that shows are fetched, letting go of the fetches wanted no more; true if any of it changed.
    pub fn set_wanted(
        &mut self,
        view: SignalView,
        is_awake: bool,
        fetch_every_min: u8,
        limits: Limits,
        is_fetching: bool,
    ) -> bool {
        let fetched = self.fetched();
        // The interval comes first: a sleep reads it to decide whether auto mode stops.
        let has_new_every = core::mem::replace(&mut self.fetch_every_min, fetch_every_min) != fetch_every_min;
        let has_new_limits = self.auto.set_limits(limits);
        let has_new_screen = self.set_view(view);
        let has_new_wake = self.set_awake(is_awake);
        let has_new_fetching = self.set_fetching(is_fetching);
        if self.fetched() != fetched {
            // The next publishing plans the fetches due now.
            self.jobs.retain(|job| matches!(job, Job::Up(_)));
            self.is_republish_due = true;
        }
        has_new_every || has_new_limits || has_new_screen || has_new_wake || has_new_fetching
    }

    // The conversation whose photos and voice notes are fetched: the one that shows on a device awake, where the settings fetch them.
    fn fetched(&self) -> Option<Peer> {
        match self.view {
            SignalView::Conversation(peer) if self.is_awake && self.is_fetching => Some(peer),
            _ => None,
        }
    }

    fn set_view(&mut self, view: SignalView) -> bool {
        let is_new = core::mem::replace(&mut self.view, view) != view;
        self.is_republish_due |= is_new;
        self.end_shown_failure();
        is_new
    }

    fn set_fetching(&mut self, is_fetching: bool) -> bool {
        let is_new = core::mem::replace(&mut self.is_fetching, is_fetching) != is_fetching;
        self.is_republish_due |= is_new;
        is_new
    }

    fn set_awake(&mut self, is_awake: bool) -> bool {
        let is_new = core::mem::replace(&mut self.is_awake, is_awake) != is_awake;
        if is_new {
            self.is_catching_up = is_awake;
            self.has_radio = false;
        }
        if is_new && is_awake {
            self.auto.resume();
        }
        if !is_awake && self.fetch_every_min == 0 {
            self.auto.set(None);
        }
        self.end_shown_failure();
        is_new
    }

    fn end_shown_failure(&mut self) {
        let is_shown = self.is_awake && self.view == SignalView::Link;
        if matches!(self.link_step, LinkStep::Failed(_)) && !is_shown {
            self.link_step = LinkStep::Idle;
        }
    }

    fn next_fetch_s(&self) -> Option<u64> {
        let minutes = u64::from(self.fetch_every_min);
        let is_linked = self.link_status == LinkStatus::Linked;
        (is_linked && minutes > 0).then(|| self.last_fetch_s + minutes * SECONDS_PER_MINUTE)
    }

    pub fn next_wait(&self) -> Await {
        match self.need() {
            Need::Nothing => Await::Arm(self.next_fetch_s()),
            need => Await::Work(need),
        }
    }

    /// Takes the alarm; true if a fetch is due.
    pub fn take_alarm(&mut self, now_s: u64) -> bool {
        let is_due = self.next_fetch_s().is_some_and(|at| now_s >= at);
        if is_due {
            self.last_fetch_s = now_s;
            self.begin_fetch();
        }
        is_due
    }

    fn begin_fetch(&mut self) {
        self.is_catching_up = self.link_status == LinkStatus::Linked;
        self.has_radio = false;
        self.auto.resume();
    }

    /// Notes a stay's end, which counts as a fetch.
    pub fn note_stay_end(&mut self, now_s: u64) {
        self.last_fetch_s = now_s;
    }

    pub const fn is_asleep(&self) -> bool {
        !self.is_awake
    }

    /// Frees the radio; true if that is news.
    pub fn grant_radio(&mut self) -> bool {
        let is_news = !self.is_awake && !self.has_radio;
        self.has_radio |= is_news;
        is_news
    }

    fn need(&self) -> Need {
        if self.is_busy && self.is_connected {
            return Need::Connect;
        }
        match self.link_status {
            LinkStatus::Reading => Need::Nothing,
            LinkStatus::Unlinked => {
                let is_shown = self.is_awake && self.view == SignalView::Link;
                let has_failed = matches!(self.link_step, LinkStep::Failed(_));
                if is_shown && !has_failed { Need::Provision } else { Need::Nothing }
            }
            LinkStatus::Linked if !self.is_awake => {
                if self.has_radio && !self.is_off() {
                    Need::Connect
                } else {
                    Need::Nothing
                }
            }
            LinkStatus::Linked => {
                let is_shown =
                    matches!(self.view, SignalView::List | SignalView::Contacts | SignalView::Conversation(_));
                let has_work = self.errand_count > 0
                    || !self.commands.is_empty()
                    || self.auto.is_pending()
                    || self.is_transferring();
                if self.is_catching_up || is_shown || has_work || self.is_calling {
                    Need::Connect
                } else {
                    Need::Nothing
                }
            }
        }
    }

    /// The next step of a link, by the codes shown so far; once every code was shown the link has failed as expired.
    pub fn next_code_step(&mut self, codes: &Codes) -> CodeStep {
        if self.need() != Need::Provision {
            CodeStep::Stop
        } else if codes.has_more() {
            CodeStep::Open
        } else {
            self.link_step = LinkStep::Failed(Cause::CodeExpired);
            CodeStep::Expired
        }
    }

    /// The time left of the current code at `now_ms`, none once it is over or its window closed.
    pub fn to_code_wait(&self, codes: &Codes, now_ms: u64) -> Option<Duration> {
        codes.time_left(now_ms).filter(|_| self.need() == Need::Provision).map(Duration::from_millis)
    }

    fn note_arrival(&mut self, peer: Peer, (text, is_noticed): (String, bool), now_ms: u64) {
        self.report.arrivals.push((peer, text, is_noticed));
        if let Peer::Contact(sender) = peer {
            self.auto.note_message(sender, now_ms);
        }
    }

    fn note_owner_wrote(&mut self, peer: Peer) {
        if let Peer::Contact(contact) = peer {
            self.auto.note_owner_wrote(contact);
        }
    }

    /// Takes the events of a delivery, a fetch of groups or a stay; what is for the call layer goes into its report, and the log that changed is published with the conversations. True where the call layer has news, which waits for no publishing.
    pub fn take_events(&mut self, events: Vec<Event>, now_ms: u64) -> bool {
        let mut has_call_news = false;
        for event in events {
            match event {
                Event::Arrived { peer, text, is_noticed, .. } => {
                    self.note_arrival(peer, (text, is_noticed), now_ms);
                    continue;
                }
                Event::Wrote(peer) => {
                    self.note_owner_wrote(peer);
                    continue;
                }
                Event::Called(incoming) => self.call_report.signals.push(*incoming),
                Event::Relays(relays) => self.call_report.relays = Some(relays),
                Event::Token(peer, membership) => {
                    self.call_report.tokens.push((peer, membership.map(|membership| *membership)));
                }
                Event::Unsent(outbound) => self.call_report.unsent.push(*outbound),
                Event::Changed(_) | Event::SafetyChanged(_) | Event::Logged => continue,
            }
            has_call_news = true;
        }
        has_call_news
    }

    /// Takes a stay's round: its status, a due turn of auto mode, and whether the stay ends.
    pub fn take_round(&mut self, status: Status, device: &Device, now_ms: u64) -> Round {
        self.errand_count = status.errands;
        self.is_busy = status.is_busy;
        self.is_catching_up &= !status.is_caught_up;
        let turn = self.begin_auto_turn(now_ms).map(|contact| {
            let peer = Peer::Contact(contact);
            let name = device.name_of(&peer);
            let recent = device.resolved_entries(auto::recent(device.entries(&peer)));
            (contact, auto::to_request(self.auto.thinking(), name.as_deref(), &recent))
        });
        let is_turn_due = turn.is_some();
        if is_turn_due {
            self.turn = turn;
        }
        let due_ms = self.auto.next_turn_ms().map_or(u64::MAX, |at| at.saturating_sub(now_ms));
        Round { is_turn_due, should_leave: self.need() != Need::Connect, wait_ms: status.due_in.min(due_ms) }
    }

    /// The turn of auto mode that waits for its writer: the contact and the request.
    pub const fn take_turn(&mut self) -> Option<(ServiceId, String)> {
        self.turn.take()
    }

    /// True once what the conversations are published for changed since they last were.
    pub const fn is_republish_due(&self) -> bool {
        self.is_republish_due
    }

    // Once the service has sent what waited, so that a burst that waited gets one answer.
    fn begin_auto_turn(&mut self, now_ms: u64) -> Option<ServiceId> {
        if self.is_catching_up { None } else { self.auto.begin_turn(now_ms) }
    }

    /// Takes the text of a turn of auto mode, none where it failed.
    pub fn take_auto_answer(&mut self, contact: ServiceId, text: Option<&str>, now_ms: u64) {
        if self.auto.contact != Some(contact) {
            return self.auto.drop_turn();
        }
        match text.and_then(auto::to_reply) {
            Some(body) => {
                self.auto.note_answered(now_ms);
                let peer = Peer::Contact(contact);
                self.commands.push_back(Command::Send(Outgoing { peer, body, is_robot: true, enclosures: Vec::new() }));
                self.has_reply_waiting = true;
            }
            None => self.auto.note_failed(),
        }
    }

    /// Takes the commands the thread has not taken, on a stay that stands: what they ask of the service counts as on its way until the round's status tells so itself.
    pub fn take_commands(&mut self) -> Vec<Command> {
        self.has_reply_waiting = false;
        self.is_busy |= !self.commands.is_empty();
        self.commands.drain(..).collect()
    }

    /// Takes the messages to send and leaves the rest.
    pub fn take_sends(&mut self) -> Vec<Outgoing> {
        let mut sends = Vec::new();
        self.commands.retain_mut(|command| match command {
            Command::Send(send) => {
                sends.push(send.clone());
                false
            }
            _ => true,
        });
        self.has_reply_waiting = false;
        sends
    }

    // A transfer is due or under way, which holds the radio.
    fn is_carrying(&self) -> bool {
        !self.jobs.is_empty() || !self.working.is_empty()
    }

    // A transfer is carried or done and not taken, which a message waits for awake.
    fn is_transferring(&self) -> bool {
        self.is_carrying() || !self.done.is_empty()
    }

    // The attachments of the owner's messages that wait to go up are due, and, awake and where the settings fetch them, those of the messages the view of the conversation that shows holds, the newest first, to be fetched; none under way, done and not taken, or held until the next stay.
    fn plan_transfers(&mut self, device: &Device) {
        let mut due: Vec<Job> = device.uploads_due().into_iter().map(Job::Up).collect();
        if let Some(peer) = self.fetched() {
            due.extend(device.downloads_due(&peer, MAX_MESSAGES).into_iter().map(Job::Down));
        }
        let is_taken = |job: &Job| {
            self.working.contains(job) || self.held.contains(job) || self.done.iter().any(|(done, _)| done == job)
        };
        let due: VecDeque<Job> = due.into_iter().filter(|job| !is_taken(job)).collect();
        self.jobs = due;
    }

    /// The next transfer for the thread that carries them, under way from then on.
    pub fn take_job(&mut self) -> Option<Job> {
        let job = self.jobs.pop_front()?;
        self.working.push(job.clone());
        Some(job)
    }

    /// Notes what a transfer ended in: what it changes waits for the device; one that broke, and a fetch refused, are held until the next stay.
    pub fn note_finished(&mut self, job: Job, finish: Finish) {
        self.working.retain(|working| *working != job);
        match (&job, &finish) {
            (_, Finish::Broke) | (Job::Down(_), Finish::Refused) => self.held.push(job),
            _ => self.done.push((job, finish)),
        }
    }

    /// Holds every transfer due until the next stay, as the network was refused one of them: none asks the radio again for what it just refused.
    pub fn hold_transfers(&mut self) {
        self.held.extend(self.jobs.drain(..));
    }

    /// Takes what the transfers ended in that the device has not taken; the photos that went up into files of their own are told the screens with the next publishing.
    pub fn take_done(&mut self) -> Vec<(Job, Finish)> {
        let moved = self.done.iter().filter_map(|(job, finish)| job.moved(finish));
        self.moved.extend(moved.map(|(from, to)| (String::from(from), String::from(to))));
        core::mem::take(&mut self.done)
    }

    /// The files of attachments that something besides the device's messages holds: what the messages not yet taken carry, and what the transfers under way and those done and not taken write.
    pub fn claimed_files(&self) -> BTreeSet<String> {
        let sends = self.commands.iter().filter_map(|command| match command {
            Command::Send(send) => Some(send.enclosures.iter().map(|enclosure| enclosure.file.clone())),
            _ => None,
        });
        let sends = sends.flatten().filter(|file| !file.is_empty());
        let transfers = self.working.iter().chain(self.done.iter().map(|(job, _)| job));
        sends.chain(transfers.map(|job| String::from(job.writes()))).collect()
    }

    /// Takes the robot's question, which the thread answers of the device as it is, without the network; at once where the R1 is not linked or the thread has ended.
    pub fn add_question(&mut self, question: Question) {
        if self.is_ended {
            return self.note_reply(robot::ended());
        }
        if self.link_status == LinkStatus::Unlinked {
            return self.note_reply(robot::unlinked());
        }
        self.question = Some(question);
    }

    /// Notes the thread's end, as it can wait no more: Signal holds off no sleep and no rest of the radio, no transfer is due, and nobody waits for an answer of it.
    pub fn end_thread(&mut self) {
        self.is_ended = true;
        self.jobs.clear();
        self.drop_call_asks();
        if self.question.take().is_some() {
            self.note_reply(robot::ended());
        }
    }

    /// Takes the question that waits for the thread.
    pub fn take_question(&mut self) -> Option<Question> {
        self.question.take()
    }

    /// Notes the thread's answer to the robot's question, for the report.
    pub fn note_reply(&mut self, reply: Reply) {
        self.report.reply = Some(reply);
    }

    /// Takes the voice note the screen wants played, by its file and the number of its playing; true where the voice notes' thread is to act.
    pub fn want_voice(&mut self, wanted: Option<(&str, u32)>) -> bool {
        self.voices.want(wanted)
    }

    /// The voice note to play in place of any other, or none to stop the one that plays; none while there is no order.
    pub fn take_voice_order(&mut self) -> Option<Option<(String, u32)>> {
        self.voices.take_asked()
    }

    /// Notes that the playing of that number ended.
    pub fn note_voice_ended(&mut self, round: u32) {
        self.report.ended.push(round);
    }

    /// Takes the photos the screen wants decoded, by their files, and the width they stand at, and the colours they are drawn over; true where the pictures' thread is to act.
    pub fn want_pictures(&mut self, wanted: Option<(Vec<String>, usize)>, roles: Roles) -> bool {
        self.pictures.roles = roles;
        self.pictures.want(wanted)
    }

    /// The next photo to decode, the width it stands at and the colours it is drawn over; none while no order holds one.
    pub fn take_picture(&mut self) -> Option<(String, usize, Roles)> {
        self.pictures.take_next()
    }

    /// Adds pictures the photos were decoded to for a width, none where one could not be; those of another width than the report holds replace them.
    pub fn give_pictures(&mut self, drawn: Vec<Photo>, wide: usize) {
        self.pictures.note_given();
        match &mut self.report.pictures {
            Some((held, held_wide)) if *held_wide == wide => held.extend(drawn),
            other => *other = Some((drawn, wide)),
        }
    }

    /// Notes the files read; a start fetches the waiting messages, so it counts as a fetch.
    pub fn note_files_read(&mut self, is_linked: bool, now_s: u64) {
        self.link_status = if is_linked { LinkStatus::Linked } else { LinkStatus::Unlinked };
        self.last_fetch_s = now_s;
    }

    /// Notes that the phone unlinked the R1, or that it was never linked.
    pub fn note_unlinked(&mut self) {
        self.link_status = LinkStatus::Unlinked;
        self.link_step = LinkStep::Idle;
        self.summaries.clear();
        self.report.summaries = Some(Vec::new());
        self.conversation = None;
        self.report.conversation = None;
        self.contacts.clear();
        self.report.contacts = Some(Vec::new());
        (self.calls, self.callees) = (Vec::new(), Vec::new());
        (self.call_report.log, self.call_report.callees) = (Some(Vec::new()), Some(Vec::new()));
        self.auto.set(None);
        self.drop_call_asks();
        self.commands.clear();
        (self.jobs, self.done, self.held, self.moved) = (VecDeque::new(), Vec::new(), Vec::new(), Vec::new());
    }

    /// Notes a link's end without a device.
    pub fn note_link_end(&mut self) {
        if !matches!(self.link_step, LinkStep::Failed(_)) {
            self.link_step = LinkStep::Idle;
        }
        self.end_shown_failure();
    }

    pub fn note_linked(&mut self) {
        self.link_status = LinkStatus::Linked;
        self.link_step = LinkStep::Idle;
        self.is_catching_up = true;
    }

    /// Notes the socket standing or standing no more: a stay that begins clears a failure and tries again the transfers held for it.
    pub fn set_connected(&mut self, is_connected: bool) {
        self.is_connected = is_connected;
        if is_connected {
            self.failure = None;
            self.held.clear();
        }
    }

    /// Notes a socket that failed or was lost.
    pub fn fail(&mut self, fault: &Fault) {
        self.fail_with(Cause::from_fault(fault, Cause::NoNetwork));
    }

    /// Notes files that could not be written.
    pub fn fail_storage(&mut self) {
        self.fail_with(Cause::NoStorage);
    }

    fn fail_with(&mut self, cause: Cause) {
        self.failure = Some(cause);
        self.is_catching_up = false;
        self.drop_call_asks();
        if !self.is_awake {
            self.has_reply_waiting = false;
            self.auto.defer();
            self.has_radio &= self.auto.is_pending();
        }
    }

    /// The rest after a failed stay; none asleep once nothing holds off a sleep.
    pub fn retry_wait(&self) -> Option<Duration> {
        (self.is_awake || !self.is_off()).then_some(RETRY_WAIT)
    }

    /// Publishes the device's conversations to the report, with the contacts while their list shows and the photos that moved, and takes the transfers due.
    pub fn publish(&mut self, device: &Device, now_ms: u64) {
        self.report.moved.append(&mut self.moved);
        let summaries = to_summaries(device, now_ms);
        if summaries != self.summaries {
            self.summaries.clone_from(&summaries);
            self.report.summaries = Some(summaries);
        }
        let conversation = match self.view {
            SignalView::Conversation(peer) => Some(to_conversation_view(device, &peer)),
            _ => None,
        };
        if conversation != self.conversation {
            self.conversation.clone_from(&conversation);
            self.report.conversation = conversation;
        }
        if self.view == SignalView::Contacts {
            let contacts = to_contacts(device, now_ms);
            if contacts != self.contacts {
                self.contacts.clone_from(&contacts);
                self.report.contacts = Some(contacts);
            }
        } else {
            self.contacts.clear();
            self.report.contacts = None;
        }
        let calls = to_call_rows(device);
        if calls != self.calls {
            self.calls.clone_from(&calls);
            self.call_report.log = Some(calls);
        }
        let callees = to_callees(device, now_ms);
        if callees != self.callees {
            self.callees.clone_from(&callees);
            self.call_report.callees = Some(callees);
        }
        self.plan_transfers(device);
        self.is_republish_due = false;
    }

    fn unread_count(&self) -> usize {
        self.summaries.iter().map(Summary::unread_count).sum()
    }

    /// True while Signal holds off neither a sleep nor the radio's rest, and once its thread has ended.
    pub fn is_off(&self) -> bool {
        let is_linked = self.link_status == LinkStatus::Linked;
        let is_waiting = self.is_catching_up
            || self.auto.is_pending()
            || self.has_reply_waiting
            || self.is_carrying()
            || self.is_calling
            || self.has_call_asks();
        let is_holding =
            (self.is_busy && self.is_connected) || self.link_step == LinkStep::MakingKeys || (is_linked && is_waiting);
        self.is_ended || !is_holding
    }
}

#[cfg(test)]
mod tests {
    use super::super::auto::SETTLE_MS;
    use super::*;
    use crate::gfx::Picture;
    use crate::signal::attachment::Enclosure;
    use crate::signal::attachment::samples::uploaded;
    use crate::signal::tests::Standin;

    const ALICE: ServiceId = ServiceId::Aci([1; 16]);
    const PEER_ALICE: Peer = Peer::Contact(ALICE);

    fn say(body: &str, is_robot: bool) -> Outgoing {
        Outgoing { peer: PEER_ALICE, body: body.into(), is_robot, enclosures: Vec::new() }
    }

    fn with_auto() -> State {
        let mut state = State::default();
        state.note_files_read(true, 0);
        state.is_catching_up = false;
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.take_commands();
        state
    }

    #[test]
    fn connect_on_need() {
        let mut state = State::default();
        assert_eq!(state.need(), Need::Nothing, "the files are not read yet");
        state.note_files_read(true, 0);
        assert_eq!(state.need(), Need::Connect, "a start fetches what waits");
        state.is_catching_up = false;
        assert_eq!(state.need(), Need::Nothing);
        state.set_view(SignalView::List);
        assert_eq!(state.need(), Need::Connect, "while the list or a conversation shows");
        state.set_view(SignalView::Nothing);
        state.add_commands([Command::Send(say("hi", false))]);
        assert_eq!(state.need(), Need::Connect, "while something is to be sent");
        state.take_commands();
        (state.errand_count, state.is_busy) = (1, true);
        assert!(state.is_off(), "a message waiting for a socket holds nothing");
        state.set_connected(true);
        (state.errand_count, state.is_busy) = (0, true);
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, false),
            "a request on its way keeps the socket, off a sleep"
        );
        state.set_awake(true);
        state.set_awake(false);
        assert_eq!(state.need(), Need::Connect, "asleep too");
        (state.errand_count, state.is_busy) = (0, false);
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, true), "asleep, the wake's catch-up is given up");
        state.begin_fetch();
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, false), "the alarm's waits for the radio");
        assert!(state.grant_radio() && !state.grant_radio(), "let off its rest once");
        assert_eq!(state.need(), Need::Connect, "then fetches, asleep");
        state.is_catching_up = false;
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, true));
        state.set_awake(true);
        assert_eq!(state.need(), Need::Connect, "a wake fetches what waits");
        state.fail(&Fault::Offline("asleep".into()));
        assert_eq!(state.need(), Need::Nothing, "awake too, a fetch that fails is given up until the next");
        state.set_view(SignalView::List);
        assert_eq!(state.need(), Need::Connect, "while what shows is tried again");
    }

    #[test]
    fn auto_turns_and_stops() {
        let mut state = with_auto();
        assert_eq!((state.auto.contact, state.need()), (Some(ALICE), Need::Nothing), "on, it holds nothing of its own");
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 1_000);
        assert_eq!(state.begin_auto_turn(1_000 + SETTLE_MS), Some(ALICE));
        state.take_auto_answer(ALICE, Some(" Soon\n"), 9_000);
        assert_eq!(
            state.take_commands(),
            [Command::Send(say("\u{1F916} Soon", true))],
            "under auto mode's mark, its whitespace taken off"
        );
        assert_eq!(state.auto.contact, Some(ALICE), "its own message does not stop it");
        state.add_commands([Command::Send(say("I'm back", false))]);
        assert_eq!(state.auto.contact, None, "the owner's does");
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.note_owner_wrote(PEER_ALICE);
        assert_eq!(state.auto.contact, None, "and so does one the owner sent from the phone");
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 100_000);
        assert_eq!(state.begin_auto_turn(100_000 + SETTLE_MS), Some(ALICE));
        state.take_auto_answer(ALICE, Some(" \n"), 110_000);
        assert_eq!((state.auto.contact, state.is_off()), (None, true), "a turn that says nothing ends it");
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 200_000);
        assert_eq!(state.begin_auto_turn(200_000 + SETTLE_MS), Some(ALICE));
        state.take_auto_answer(ALICE, None, 210_000);
        assert_eq!((state.auto.contact, state.is_off()), (None, true), "and so does a turn that fails");
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.set_awake(false);
        assert_eq!(state.auto.contact, None, "a sleep switches it off where the alarm fetches nothing");
    }

    #[test]
    fn sleep_without_interval_stops_auto() {
        let mut state = with_auto();
        assert!(state.set_wanted(SignalView::Nothing, true, 5, Limits::default(), true), "an interval set");
        assert!(state.set_wanted(SignalView::Nothing, false, 0, Limits::default(), true));
        assert_eq!(state.auto.contact, None, "the sleep goes by the interval that came with it");
    }

    #[test]
    fn auto_at_each_fetch() {
        let mut state = with_auto();
        state.fetch_every_min = 5;
        state.last_fetch_s = 1_000;
        assert_eq!(
            (state.need(), state.next_fetch_s()),
            (Need::Nothing, Some(1_300)),
            "awake, nothing until the fetch"
        );
        assert!(state.take_alarm(1_300) && state.need() == Need::Connect);
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 0);
        state.is_catching_up = false;
        assert_eq!(
            (state.need(), state.begin_auto_turn(SETTLE_MS)),
            (Need::Connect, Some(ALICE)),
            "held for the burst's turn"
        );
        state.take_auto_answer(ALICE, Some("Later"), 20_000);
        state.take_commands();
        state.note_arrival(PEER_ALICE, ("and?".into(), true), 30_000);
        assert_eq!(
            (state.need(), state.auto.next_turn_ms()),
            (Need::Nothing, Some(50_000)),
            "let go while the pace holds"
        );
        state.set_awake(false);
        state.add_commands([Command::Read(PEER_ALICE)]);
        assert!(state.take_commands().is_empty(), "a conversation left open reads nothing asleep");
        assert_eq!(state.next_fetch_s(), Some(1_600), "asleep, the alarm is set for the same interval");
        assert!(state.take_alarm(1_600) && !state.is_off() && state.grant_radio());
        state.is_catching_up = false;
        assert_eq!(state.begin_auto_turn(60_000), Some(ALICE), "and answers it then");
        state.take_auto_answer(ALICE, Some("Later"), 70_000);
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, false),
            "the reply holds the socket until it is taken"
        );
        state.set_connected(true);
        assert_eq!(state.take_commands().len(), 1);
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, false),
            "taken on a stay, it holds the socket and the radio until the round tells how the stay stands"
        );
        let idle = Status { errands: 0, is_busy: false, is_caught_up: true, due_in: 30_000 };
        assert!(state.take_round(idle, &Device::unkeyed(), 70_000).should_leave, "sent and answered: let go");
        state.set_connected(false);
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, true));
        state.begin_fetch();
        state.grant_radio();
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 200_000);
        state.fail(&Fault::Temporary("the socket broke".into()));
        assert_eq!(
            (state.need(), state.is_off(), state.auto.contact),
            (Need::Nothing, true, Some(ALICE)),
            "a fetch that fails is given up until the next, and owes the answer"
        );
        state.begin_fetch();
        assert_eq!(state.begin_auto_turn(300_000), None, "the next fetch owes it once what waited has come");
        state.is_catching_up = false;
        assert_eq!(state.begin_auto_turn(300_000), Some(ALICE));
        state.add_commands([Command::Auto(Some(ServiceId::Aci([2; 16])))]);
        state.take_auto_answer(ALICE, None, 310_000);
        assert_eq!(
            state.auto.contact,
            Some(ServiceId::Aci([2; 16])),
            "an answer for a conversation it left switches nothing off"
        );
    }

    #[test]
    fn alarm_fetch_interval() {
        let mut state = State { fetch_every_min: 60, ..State::default() };
        assert_eq!(state.next_fetch_s(), None, "not linked yet");
        state.note_files_read(true, 1_000);
        state.is_catching_up = false;
        assert_eq!(state.next_fetch_s(), Some(4_600), "from the start");
        assert!(
            !state.take_alarm(4_599) && state.need() == Need::Nothing,
            "early, as after the clocks were set: nothing"
        );
        assert!(state.take_alarm(4_600) && state.need() == Need::Connect, "due: what waits is fetched");
        state.is_catching_up = false;
        state.note_stay_end(5_000);
        assert_eq!(state.next_fetch_s(), Some(8_600), "as it does from the end of a stay");
        state.fetch_every_min = 0;
        assert!(state.next_fetch_s().is_none() && !state.take_alarm(9_000), "never");
        state.fetch_every_min = 5;
        state.note_unlinked();
        assert!(!state.take_alarm(9_000));
        assert_eq!((state.next_fetch_s(), state.need()), (None, Need::Nothing));
    }

    #[test]
    fn provision_on_link_window() {
        let mut state = State::default();
        state.note_files_read(false, 0);
        assert_eq!(state.need(), Need::Nothing);
        state.set_view(SignalView::Link);
        assert_eq!(state.need(), Need::Provision);
        state.set_awake(false);
        assert_eq!(state.need(), Need::Nothing);
    }

    #[test]
    fn work_or_alarm() {
        let mut state = State { fetch_every_min: 5, ..State::default() };
        assert_eq!(state.next_wait(), Await::Arm(None), "the files are not read yet");
        state.note_files_read(true, 1_000);
        assert_eq!(state.next_wait(), Await::Work(Need::Connect));
        state.is_catching_up = false;
        assert_eq!(state.next_wait(), Await::Arm(Some(1_300)));
        let mut device = 7;
        assert_eq!(Need::Connect.to_work(Some(&mut device)), Work::Stay(&mut 7));
        assert_eq!(Need::Provision.to_work::<u32>(None), Work::Link);
        assert_eq!(Need::Provision.to_work(Some(&mut device)), Work::Rest, "linked meanwhile");
        assert_eq!(Need::Connect.to_work::<u32>(None), Work::Rest, "unlinked meanwhile");
        assert_eq!(Need::Nothing.to_work::<u32>(None), Work::Rest);
    }

    #[test]
    fn link_codes() {
        let (mut state, mut codes) = (State::default(), Codes::default());
        state.note_files_read(false, 0);
        assert_eq!(state.next_code_step(&codes), CodeStep::Stop, "no window");
        state.set_view(SignalView::Link);
        assert_eq!(state.next_code_step(&codes), CodeStep::Open);
        assert_eq!(state.to_code_wait(&codes, 0), None, "no code stands yet");
        codes.begin(0);
        assert_eq!(state.to_code_wait(&codes, 45_000 - 7), Some(Duration::from_millis(7)));
        assert_eq!(state.to_code_wait(&codes, 45_000), None, "it is over");
        (0..5).for_each(|_| codes.begin(0));
        assert_eq!(state.next_code_step(&codes), CodeStep::Expired);
        assert_eq!(
            (state.link_step.clone(), state.next_code_step(&codes)),
            (LinkStep::Failed(Cause::CodeExpired), CodeStep::Stop),
            "the link has failed as expired, and no further code goes up"
        );
        assert_eq!(
            [LinkStep::NO_KEYS, LinkStep::UNWRITTEN],
            [LinkStep::Failed(Cause::Refused), LinkStep::Failed(Cause::NoStorage)],
            "keys the library did not make, and an account that could not be written"
        );
        state.set_view(SignalView::Nothing);
        assert_eq!((state.next_code_step(&codes), state.to_code_wait(&codes, 0)), (CodeStep::Stop, None));
    }

    #[test]
    fn link_failure() {
        let mut state = State::default();
        state.note_files_read(false, 0);
        state.set_view(SignalView::Link);
        state.link_step = LinkStep::MakingKeys;
        assert!(!state.is_off(), "the link request on its way holds off a sleep and the radio's rest");
        state.link_step = LinkStep::Failed(Cause::NoStorage);
        state.note_link_end();
        assert_eq!(
            (state.need(), state.link_step.clone(), state.is_off()),
            (Need::Nothing, LinkStep::Failed(Cause::NoStorage), true),
            "a failure stands, and the window tells it"
        );
        state.set_view(SignalView::Nothing);
        state.set_view(SignalView::Link);
        assert_eq!(
            (state.need(), state.link_step.clone()),
            (Need::Provision, LinkStep::Idle),
            "opened again, it begins anew"
        );
        state.link_step = LinkStep::Failed(Cause::CodeExpired);
        state.set_awake(false);
        state.set_awake(true);
        assert_eq!(state.link_step.clone(), LinkStep::Idle, "and so it does after a sleep");
        state.link_step = LinkStep::Code("sgnl://".into());
        state.note_link_end();
        assert_eq!(state.link_step.clone(), LinkStep::Idle, "a link that ended without a failure is over");
        state.set_view(SignalView::Nothing);
        state.link_step = LinkStep::Failed(Cause::Refused);
        state.note_link_end();
        assert_eq!(
            state.link_step.clone(),
            LinkStep::Idle,
            "and a failure whose window closed before it came is over too"
        );
    }

    #[test]
    fn link_refusal_and_end() {
        use link::Refusal;
        assert_eq!(
            [Refusal::TooOld, Refusal::TooManyDevices, Refusal::Other].map(LinkStep::from_refusal),
            [Cause::TooOld, Cause::TooManyDevices, Cause::Refused].map(LinkStep::Failed),
            "the service's refusal as the cause the link's window tells"
        );
        let mut state = State::default();
        state.note_files_read(false, 0);
        state.set_view(SignalView::Link);
        state.set_link_step(LinkStep::MakingKeys);
        state.is_catching_up = false;
        state.note_linked();
        let status = state.take_report().status;
        assert_eq!(
            (status.link_status, status.link_step, state.need()),
            (LinkStatus::Linked, LinkStep::Idle, Need::Connect),
            "linked, the link is over and what waits is fetched"
        );
    }

    #[test]
    fn retry_rest() {
        let mut state = with_auto();
        assert_eq!(state.retry_wait(), Some(RETRY_WAIT), "awake");
        state.fetch_every_min = 5;
        state.set_awake(false);
        assert_eq!(state.retry_wait(), None, "asleep with nothing to do nothing is tried again before the alarm");
        assert!(state.take_alarm(u64::MAX / 2) && state.grant_radio());
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 0);
        state.is_catching_up = false;
        assert_eq!(state.begin_auto_turn(SETTLE_MS), Some(ALICE), "a turn at work");
        state.fail(&Fault::Offline("the network is gone".into()));
        assert_eq!(
            (state.is_off(), state.retry_wait(), state.grant_radio(), state.need()),
            (false, Some(RETRY_WAIT), false, Need::Connect),
            "the radio stays let off its rest for the turn's reply, which is tried for after a rest"
        );
        state.take_auto_answer(ALICE, Some("Soon"), 9_000);
        state.fail(&Fault::Temporary("the socket broke".into()));
        assert_eq!(
            (state.is_off(), state.retry_wait(), state.need()),
            (true, None, Need::Nothing),
            "once no turn is at work the reply waits for the next fetch"
        );
        assert_eq!(state.take_sends().len(), 1, "where it goes then");
    }

    #[test]
    fn thread_end() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let file = "camera/pictures/20261029_101500.jpg";
        let send = Outgoing { enclosures: alloc::vec![Enclosure::photo(file.into(), (4, 3))], ..say("", false) };
        let mut state = linked();
        state.add_commands([Command::Send(send)]);
        device.keep_sends(&crypto, state.take_sends(), 1_000);
        state.publish(&device, 1_000);
        state.add_question(Question::Read(None));
        assert!(!state.is_off(), "a transfer due holds off a sleep");
        state.end_thread();
        assert!(state.is_off() && state.take_job().is_none(), "an ended thread holds nothing off, and nothing goes up");
        assert_eq!(state.take_report().reply, Some(robot::ended()), "the question it was to answer is answered");
        state.add_question(Question::Draft("Anna".into()));
        assert_eq!(
            (state.take_question(), state.take_report().reply),
            (None, Some(robot::ended())),
            "and every later one at once"
        );
    }

    #[test]
    fn stay_round() {
        let mut device = Device::unkeyed();
        for at in 0..60 {
            device.compose(&Standin::default(), PEER_ALICE, (&alloc::format!("m{at}"), &[]), at, false);
        }
        let status = |errands, is_caught_up| Status { errands, is_busy: errands > 0, is_caught_up, due_in: 30_000 };
        let mut state = State::default();
        state.note_files_read(true, 0);
        assert!(
            !state.take_round(status(0, false), &device, 0).should_leave,
            "a start's stay is held while what waited still comes"
        );
        assert!(
            !state.take_round(status(1, true), &device, 0).should_leave,
            "and once it has come, while an errand is not done"
        );
        assert!(state.take_round(status(0, true), &device, 0).should_leave, "and ends once nothing wants it");
        let mut state = State::default();
        state.note_files_read(true, 0);
        state.add_commands([Command::Auto(Some(ALICE))]);
        state.take_commands();
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 0);
        let round = state.take_round(status(0, false), &device, 1_000);
        assert_eq!(
            (state.turn.take(), round.should_leave, round.wait_ms),
            (None, false, SETTLE_MS - 1_000),
            "the stay waits for the burst to settle"
        );
        let round = state.take_round(status(0, false), &device, SETTLE_MS);
        assert_eq!(
            (state.turn.take(), round.should_leave),
            (None, false),
            "settled, but what waited still comes: no turn yet"
        );
        let round = state.take_round(status(0, true), &device, SETTLE_MS);
        assert!(round.is_turn_due);
        let (contact, request) = state.turn.take().expect("the turn that is due");
        assert_eq!(
            request,
            auto::to_request(state.auto.thinking(), None, &device.entries(&PEER_ALICE)[10..]),
            "given the conversation's last messages"
        );
        assert_eq!(
            (contact, round.should_leave, round.wait_ms),
            (ALICE, false, 30_000),
            "and held while it is at work"
        );
        state.take_auto_answer(ALICE, Some("Soon"), 9_000);
        state.take_commands();
        let round = state.take_round(status(0, true), &device, 9_000);
        assert!(round.should_leave && !round.is_turn_due, "nothing wants the stay any more");
    }

    #[test]
    fn failure_cause() {
        let mut state = State::default();
        state.fail(&Fault::Refused("no".into()));
        assert_eq!(state.failure, Some(Cause::Refused));
        state.fail(&Fault::Temporary("gone".into()));
        assert_eq!(state.failure, Some(Cause::NoNetwork), "a failure this time is told as no network");
        state.fail_storage();
        assert_eq!(state.failure, Some(Cause::NoStorage), "and files that could not be written as no storage");
    }

    #[test]
    fn publish_on_new_view() {
        let mut device = Device::unkeyed();
        device.compose(&Standin::default(), PEER_ALICE, ("hi", &[]), 1_000, false);
        let mut state = State::default();
        state.set_view(SignalView::List);
        assert!(state.is_republish_due, "another screen shows");
        state.publish(&device, 0);
        let report = core::mem::take(&mut state.report);
        assert_eq!(
            (report.summaries.map(|list| list.len()), report.conversation),
            (Some(1), None),
            "the list, and no conversation"
        );
        assert!(!state.is_republish_due && !state.set_view(SignalView::List) && !state.is_republish_due);
        state.set_view(SignalView::Conversation(PEER_ALICE));
        assert!(state.is_republish_due);
        state.publish(&device, 0);
        let report = core::mem::take(&mut state.report);
        assert_eq!(
            (report.summaries, report.conversation.map(|view| view.entries.len())),
            (None, Some(1)),
            "the list as it was"
        );
        assert!(!state.is_republish_due);
    }

    #[test]
    fn sends_leave_the_rest() {
        let mut state = State::default();
        let hi = say("hi", false);
        state.add_commands([Command::Read(PEER_ALICE), Command::Send(hi.clone()), Command::Auto(Some(ALICE))]);
        assert_eq!(state.take_sends(), [hi]);
        assert_eq!(state.take_commands(), [Command::Read(PEER_ALICE), Command::Auto(Some(ALICE))]);
    }

    #[test]
    fn events_reach_report_and_auto() {
        let mut state = with_auto();
        let event = Event::Arrived { peer: PEER_ALICE, timestamp: 7, text: "hi".into(), is_noticed: true };
        assert!(!state.take_events(alloc::vec![event, Event::Changed(PEER_ALICE)], 0), "nothing of a call's");
        assert_eq!(state.report.arrivals, [(PEER_ALICE, "hi".into(), true)]);
        assert!(!state.take_events(alloc::vec![Event::Wrote(PEER_ALICE)], 0));
        assert_eq!(state.auto.contact, None, "the owner wrote");
    }

    #[test]
    fn report_changes_once() {
        let mut device = Device::unkeyed();
        device.compose(&Standin::default(), PEER_ALICE, ("hi", &[]), 1_000, false);
        let mut state = State::default();
        state.publish(&device, 0);
        state.note_arrival(PEER_ALICE, ("hi".into(), true), 0);
        let report = core::mem::take(&mut state.report);
        assert_eq!((report.summaries.map(|list| list.len()), report.arrivals.len()), (Some(1), 1));
        state.publish(&device, 0);
        assert!(core::mem::take(&mut state.report) == Report::default(), "the same again is no change");
    }

    fn linked() -> State {
        let mut state = State::default();
        state.note_files_read(true, 0);
        state.is_catching_up = false;
        state
    }

    #[test]
    fn contacts_while_listed() {
        let mut device = Device::unkeyed();
        device.add_named_contact(ServiceId::Aci([2; 16]), "Boris", false);
        device.add_named_contact(ALICE, "anna", false);
        device.add_incoming(ALICE, ("hi", &[]), 5, 1_000);
        let mut state = linked();
        state.set_view(SignalView::List);
        state.publish(&device, 0);
        assert_eq!(state.take_report().contacts, None, "none while the conversations show");
        state.set_view(SignalView::Contacts);
        assert_eq!(state.need(), Need::Connect, "their list holds the socket as the conversations' does");
        state.publish(&device, 0);
        let contacts = state.take_report().contacts.expect("the contacts");
        assert_eq!(
            contacts.iter().map(|contact| (contact.name.as_str(), contact.has_talked)).collect::<Vec<_>>(),
            [("anna", true), ("Boris", false), ("Note to Self", false)],
            "by name whatever its letters' case, each told whether its conversation holds messages"
        );
        state.publish(&device, 0);
        assert_eq!(state.take_report().contacts, None, "the same again is no news");
        state.set_view(SignalView::List);
        state.publish(&device, 0);
        assert_eq!(state.take_report().contacts, None, "let go of without a word once their list closes");
        state.set_view(SignalView::Contacts);
        state.publish(&device, 0);
        assert_eq!(
            state.take_report().contacts.map(|contacts| contacts.len()),
            Some(3),
            "and told anew as it shows again"
        );
        state.note_unlinked();
        assert_eq!(state.take_report().contacts, Some(Vec::new()), "an unlink empties the list");
    }

    #[test]
    fn question_reply_once() {
        let mut state = State::default();
        state.add_question(Question::Read(None));
        assert_eq!((state.take_question(), state.take_question()), (Some(Question::Read(None)), None));
        let reply = Reply { told: "Nothing yet.".into(), draft: Some(PEER_ALICE) };
        state.note_reply(reply.clone());
        assert_eq!(state.take_report().reply, Some(reply));
        assert_eq!(state.take_report().reply, None, "told once");
        state.note_files_read(false, 0);
        state.add_question(Question::Draft("Anna".into()));
        assert_eq!(
            (state.take_question(), state.take_report().reply),
            (None, Some(robot::unlinked())),
            "not linked: answered at once, for the thread has no device to ask"
        );
    }

    #[test]
    fn upload_held_after_failure() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let file = "camera/pictures/20261029_101500.jpg";
        let send = Outgoing { enclosures: alloc::vec![Enclosure::photo(file.into(), (4, 3))], ..say("", false) };
        let mut state = linked();
        state.add_commands([Command::Send(send)]);
        assert!(state.claimed_files().contains(file), "what waits to be sent holds its file");
        device.keep_sends(&crypto, state.take_sends(), 1_000);
        assert!(state.claimed_files().is_empty(), "the device's message holds it now");
        assert!(device.unsent_orders(&crypto, 2_000).is_empty(), "the message waits for its photo");
        state.publish(&device, 1_000);
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, false),
            "a transfer due holds the socket and the radio"
        );
        let job = state.take_job().expect("the photo to go up");
        assert_eq!(job.writes(), ".r1metal/signal/attachments/1000-0.jpg");
        assert!(state.claimed_files().contains(job.writes()), "under way, what it writes is held");
        state.note_finished(job.clone(), Finish::Broke);
        state.publish(&device, 1_000);
        assert!(state.take_job().is_none() && state.is_off(), "broken, it is held until the next stay");
        state.set_connected(true);
        state.publish(&device, 1_000);
        let job = state.take_job().expect("tried again in the stay");
        state.note_finished(job.clone(), Finish::Refused);
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, true),
            "done and not taken, it wants the socket awake and, as a message waiting for one, holds nothing"
        );
        let done = state.take_done();
        assert_eq!(done, [(job, Finish::Refused)]);
        assert!(device.take_finished(&crypto, done, 3_000).1, "its message is not sent");
        state.publish(&device, 3_000);
        assert!(state.take_job().is_none(), "nothing more to go up");
    }

    #[test]
    fn refused_network_holds_every_transfer() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let photo = |number: u8| Enclosure::photo(alloc::format!("camera/pictures/{number}.jpg"), (4, 3));
        let send = Outgoing { enclosures: (0..3).map(photo).collect(), ..say("", false) };
        let mut state = linked();
        state.add_commands([Command::Send(send)]);
        device.keep_sends(&crypto, state.take_sends(), 1_000);
        state.publish(&device, 1_000);
        let job = state.take_job().expect("the first photo to go up");
        state.hold_transfers();
        state.note_finished(job, Finish::Broke);
        state.publish(&device, 1_000);
        assert!(
            state.take_job().is_none() && state.is_off(),
            "the network refused one: the others wait with it, and none asks the radio again"
        );
        state.set_connected(true);
        state.publish(&device, 1_000);
        let jobs: Vec<Job> = core::iter::from_fn(|| state.take_job()).collect();
        assert_eq!(jobs.len(), 3, "all three are tried again in the next stay");
    }

    #[test]
    fn moved_photos_told_with_publishing() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let file = "camera/pictures/20261029_101500.jpg";
        let send = Outgoing { enclosures: alloc::vec![Enclosure::photo(file.into(), (4, 3))], ..say("", false) };
        let mut state = linked();
        state.set_view(SignalView::Conversation(PEER_ALICE));
        state.add_commands([Command::Send(send)]);
        device.keep_sends(&crypto, state.take_sends(), 1_000);
        state.publish(&device, 1_000);
        state.take_report();
        let job = state.take_job().expect("the photo to go up");
        let kept = String::from(job.writes());
        state.note_finished(job, Finish::Uploaded(uploaded(&kept)));
        let done = state.take_done();
        assert!(state.take_report().moved.is_empty(), "nothing is told before the conversation holds it there");
        device.take_finished(&crypto, done, 2_000);
        state.publish(&device, 2_000);
        let report = state.take_report();
        let entries = report.conversation.iter().flat_map(|view| view.entries.iter());
        let held = entries.flat_map(|entry| entry.attachments.iter()).filter_map(|held| held.file.clone());
        assert_eq!(
            (report.moved, held.collect::<Vec<_>>()),
            (alloc::vec![(String::from(file), kept.clone())], alloc::vec![kept]),
            "told with the conversation that holds it in its own file"
        );
        state.publish(&device, 2_000);
        assert!(state.take_report().moved.is_empty(), "once");
    }

    #[test]
    fn fetch_while_shown() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        device.add_incoming_photo(ALICE, (9, 6), 3_000);
        let mut state = linked();
        state.publish(&device, 3_000);
        assert!(state.take_job().is_none(), "nothing fetched while its conversation does not show");
        state.set_view(SignalView::Conversation(PEER_ALICE));
        assert!(state.set_fetching(false) && !state.set_fetching(false));
        state.publish(&device, 3_000);
        assert!(state.take_job().is_none(), "nor while the settings show text alone");
        assert!(state.set_fetching(true) && state.is_republish_due, "fetched again, the transfers due are taken anew");
        state.set_awake(false);
        state.publish(&device, 3_000);
        assert!(state.take_job().is_none(), "nor asleep");
        state.set_awake(true);
        state.publish(&device, 3_000);
        let job = state.take_job().expect("fetched as it shows");
        assert!(matches!(job, Job::Down(_)));
        state.note_finished(job.clone(), Finish::Refused);
        assert!(state.take_done().is_empty(), "a fetch refused is held, not done");
        state.set_connected(true);
        state.publish(&device, 3_000);
        let job = state.take_job().expect("tried again at the next stay");
        state.note_finished(job.clone(), Finish::Fetched);
        assert!(device.take_finished(&crypto, state.take_done(), 4_000).1, "held by its message");
        assert!(device.held_files().contains(job.writes()));
        state.publish(&device, 4_000);
        assert!(state.take_job().is_none(), "fetched once");
        state.note_unlinked();
        assert!(state.claimed_files().is_empty() && state.take_job().is_none());
    }

    #[test]
    fn fetches_let_go_asleep() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        device.add_incoming_photo(ALICE, (9, 6), 3_000);
        let file = "camera/pictures/20261029_101500.jpg";
        let send = Outgoing { enclosures: alloc::vec![Enclosure::photo(file.into(), (4, 3))], ..say("", false) };
        let conversation = SignalView::Conversation(PEER_ALICE);
        let mut state = linked();
        state.set_view(conversation);
        state.add_commands([Command::Send(send)]);
        device.keep_sends(&crypto, state.take_sends(), 1_000);
        state.publish(&device, 3_000);
        assert!(state.set_wanted(conversation, false, 0, Limits::default(), true) && state.is_republish_due);
        let upload = state.take_job();
        assert!(
            matches!(upload, Some(Job::Up(_))) && state.take_job().is_none(),
            "asleep the photo goes up, and the fetch planned awake is let go of"
        );
        state.publish(&device, 3_000);
        assert!(state.take_job().is_none(), "nor is it planned asleep");
        state.note_finished(upload.expect("the upload"), Finish::Broke);
        assert!(state.is_off(), "nothing is carried, and nothing holds off a sleep");
        assert!(state.set_wanted(conversation, true, 0, Limits::default(), true) && state.is_republish_due);
        state.publish(&device, 3_000);
        assert!(matches!(state.take_job(), Some(Job::Down(_))), "awake again it is fetched");
    }

    #[test]
    fn media_orders_and_report() {
        let mut state = State::default();
        assert!(!state.want_voice(None) && state.take_voice_order().is_none());
        assert!(state.want_voice(Some(("a.m4a", 1))) && !state.want_voice(Some(("a.m4a", 1))));
        assert_eq!(state.take_voice_order(), Some(Some((String::from("a.m4a"), 1))));
        state.note_voice_ended(1);
        state.note_voice_ended(2);
        let wanted = Some((alloc::vec![String::from("a.jpg"), String::from("b.jpg")], 480));
        let roles = Roles::default();
        assert!(state.want_pictures(wanted.clone(), roles) && !state.want_pictures(wanted, roles));
        let (file, wide, _) = state.take_picture().expect("an order");
        assert_eq!((file.as_str(), wide), ("a.jpg", 480));
        let one = |file: &str| (String::from(file), Some(Picture { words: alloc::vec![7; 4], width: 2 }));
        state.give_pictures(alloc::vec![one("a.jpg")], 480);
        state.give_pictures(alloc::vec![(String::from("b.jpg"), None)], 480);
        let report = state.take_report();
        assert_eq!(report.ended, [1, 2]);
        let (drawn, wide) = report.pictures.expect("the pictures");
        assert_eq!(
            (drawn.iter().map(|(file, _)| file.as_str()).collect::<Vec<_>>(), wide),
            (["a.jpg", "b.jpg"].to_vec(), 480)
        );
        assert!(drawn[1].1.is_none(), "a photo that could not be decoded stays told by its word");
        state.give_pictures(alloc::vec![one("a.jpg")], 480);
        state.give_pictures(alloc::vec![one("c.jpg")], 400);
        assert_eq!(
            state.take_report().pictures.map(|(drawn, wide)| (drawn.len(), wide)),
            Some((1, 400)),
            "the width that came last"
        );
        let again = state.take_report();
        assert!(again.ended.is_empty() && again.pictures.is_none(), "taken once");
    }

    #[test]
    fn calls_hold_socket() {
        let mut state = linked();
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, true), "no call: nothing held");
        assert!(state.set_calling(true) && !state.set_calling(true), "new once");
        assert_eq!(
            (state.need(), state.is_off()),
            (Need::Connect, false),
            "a call stands: the socket held, a sleep and the radio's rest held off"
        );
        state.set_awake(false);
        assert_eq!(state.need(), Need::Nothing, "asleep, not before the radio is let off its rest");
        assert!(state.grant_radio(), "which the call lets off its rest");
        assert_eq!(state.need(), Need::Connect, "then held, as the other terms are");
        state.set_calling(false);
        assert_eq!((state.need(), state.is_off()), (Need::Nothing, true), "the call over: nothing held");
    }

    #[test]
    fn call_asks_never_late() {
        use crate::signal::call::Signal;
        use crate::signal::command::Target;
        let mut state = linked();
        state.set_awake(false);
        let group = Peer::Group([1; 32]);
        let busy = Outbound { target: Target::Contact(ALICE), device: None, signal: Signal::Busy { call: 1 } };
        let send = Command::Send(say("hi", false));
        state.add_commands([Command::Signal(busy.clone()), Command::Relays, Command::Token(group), send.clone()]);
        state.add_commands([Command::Seen]);
        assert!(!state.is_off(), "what the call layer asks of the network holds the radio, asleep too");
        state.fail(&Fault::Offline("gone".into()));
        let report = state.take_call_report();
        assert_eq!(
            (report.unsent, report.relays, report.tokens),
            (alloc::vec![busy], Some(Vec::new()), alloc::vec![(group, None)]),
            "a stay that did not stand: dropped, and told"
        );
        assert_eq!(state.take_commands(), [send, Command::Seen], "the rest waits for the next stay");
        assert_eq!(state.take_call_report(), CallReport::default(), "taken once");
    }

    #[test]
    fn call_events_reported() {
        use crate::signal::address::tests::address;
        use crate::signal::call::Signal;
        use crate::signal::command::Target;
        let mut state = linked();
        let incoming = Incoming {
            sender: address(ALICE, 2),
            name: Some("Alice".into()),
            group_name: None,
            signals: alloc::vec![Signal::Busy { call: 1 }],
            age: 9,
            is_accepted: true,
            identity: [1; 32],
            own_identity: [2; 32],
            own_device: 3,
        };
        let busy = Outbound { target: Target::Own, device: None, signal: Signal::Busy { call: 1 } };
        let group = Peer::Group([1; 32]);
        let membership = Membership { token: "t".into(), members: Vec::new(), own: ALICE };
        assert!(!state.take_events(alloc::vec![Event::Logged], 0), "the log alone waits for its publishing");
        let has_call_news = state.take_events(
            alloc::vec![
                Event::Called(alloc::boxed::Box::new(incoming.clone())),
                Event::Relays(Vec::new()),
                Event::Token(group, Some(alloc::boxed::Box::new(membership.clone()))),
                Event::Unsent(alloc::boxed::Box::new(busy.clone())),
                Event::Logged
            ],
            0,
        );
        assert!(has_call_news, "news for the call layer, which the main loop is woken for");
        let report = state.take_call_report();
        assert_eq!(
            report,
            CallReport {
                signals: alloc::vec![incoming],
                relays: Some(Vec::new()),
                tokens: alloc::vec![(group, Some(membership))],
                unsent: alloc::vec![busy],
                log: None,
                callees: None
            },
            "each for the call layer; the log is published as it changed"
        );
        assert_eq!(
            alloc::format!("{report:?}"),
            "CallReport { signals: 1, relays: Some(0), tokens: 1, unsent: 1, log: None, callees: None }",
            "kinds and counts alone"
        );
    }

    #[test]
    fn call_log_published() {
        use crate::signal::call::Kind;
        use crate::signal::device::{Course, Note, Status};
        let mut device = Device::unkeyed();
        device.add_named_contact(ALICE, "Alice", false);
        let note =
            Note { peer: PEER_ALICE, call: 1, kind: Kind::Audio, is_outgoing: false, ringer: Some(ALICE), at: 5 };
        device.add_call(&note, Some(Course::Missed));
        let mut state = linked();
        state.publish(&device, 0);
        let report = state.take_call_report();
        let log = report.log.expect("the log");
        assert_eq!(
            log.iter().map(|row| (row.name.as_deref(), row.status, row.is_seen)).collect::<Vec<_>>(),
            [(Some("Alice"), Status::Missed, false)],
            "by its conversation's name, missed, new"
        );
        assert_eq!(
            report.callees.map(|callees| callees.into_iter().map(|callee| callee.name).collect::<Vec<_>>()),
            Some(alloc::vec![String::from("Alice")]),
            "whom a call may go to"
        );
        state.publish(&device, 0);
        assert_eq!(state.take_call_report(), CallReport::default(), "the same again is no news");
        state.note_unlinked();
        let report = state.take_call_report();
        assert_eq!((report.log, report.callees), (Some(Vec::new()), Some(Vec::new())), "an unlink empties both");
    }

    #[test]
    fn debug_hides_names_and_words() {
        let report = Report { arrivals: alloc::vec![(PEER_ALICE, "Lunch?".into(), true)], ..Report::default() };
        assert_eq!(
            alloc::format!("{report:?}"),
            "Report { summaries: None, opened: None, contacts: None, arrived: 1, reply: None, ended: 0, pictures: None, moved: 0, .. }"
        );
        let told = Reply { told: "Lunch?".into(), draft: Some(PEER_ALICE) };
        let report = Report { reply: Some(told), contacts: Some(Vec::new()), ..Report::default() };
        assert_eq!(
            alloc::format!("{report:?}"),
            "Report { summaries: None, opened: None, contacts: Some(0), arrived: 0, reply: Some(Reply { draft: true, .. }), ended: 0, pictures: None, moved: 0, .. }"
        );
        let views = [SignalView::Conversation(PEER_ALICE), SignalView::Conversation(Peer::Group([1; 32]))];
        assert_eq!(views.map(|view| alloc::format!("{view:?}")), ["Conversation(Aci)", "Conversation(Group)"]);
    }
}
