//! The radio thread's requests, how a round takes those that piled up, and the steps that follow.

use crate::base::handoff::Ticket;
use crate::cell::modem::KEEP;
use crate::cell::modem::NOT_READY;
use crate::wifi::join::Limits;
use crate::wifi::join::Purpose;
use crate::wifi::known::Change;
use crate::wifi::known::Entry;
use crate::wifi::known::Known;
use crate::wifi::known::TrialError;
use crate::wifi::known::UNKNOWN;
use crate::wifi::places::Place;
use crate::wifi::places::Places;
use crate::wifi::scan::Network;
use crate::wifi::search::ScanOutcome;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem;
use core::time::Duration;

pub const ASLEEP: &str = "the device sleeps";
pub const UNWATCHED: &str = "the link cannot be watched";
pub const NO_LOOK: &str = "the radio could not look for networks";
const NO_POWER: &str = "the radio does not come on";
const THREAD_GONE: &str = "the radio's thread has ended";
const NOT_OFF: &str = "the radio is not off in the time a way down waits";
const NEAR_LINE: &str = "radio: a known network is heard: the next stay looks for WiFi first";
const AWAY_LINE: &str = "radio: no known network is heard: the next stay goes to the modem";

fn to_passed_over_line(network: &Entry) -> String {
    format!("wifi: {:?} is passed over for {} min, the modem standing in", network.name, KEEP.as_secs() / 60)
}

/// A request to the radio's thread; `A` answers a hold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request<A> {
    Power(bool),
    // A pass of listening for the list of networks, by its number, or none any more.
    Pass(Option<u32>),
    // A join that is to prove a network for the list, by its number with the change that waits for it, or none any more.
    Trial(Option<(u32, Change)>),
    // To be in the default network, or in none any more.
    Link(bool),
    // To be in the modem's network alone, WiFi passed over, or not any more.
    Cell(bool),
    // To be in the modem's network alone with every stay routed, its IPv4 packets handed to the router, or not any more.
    Route(bool),
    // Asleep the device is in no network, whoever asks.
    Rest(bool),
    // To be in a network for as long as the asker holds on to it; the answer comes when the device is in, or cannot be.
    Hold(A),
    // A holder of the stay of that number has done.
    Release(u64),
}

impl<A> Request<A> {
    // A scan or a join that runs gives way to the request; one that only changes who holds the network waits its turn.
    const fn is_urgent(&self) -> bool {
        !matches!(self, Request::Hold(_) | Request::Release(_))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Count {
    urgent: usize,
    normal: usize,
}

impl Count {
    const fn add<A>(&mut self, request: &Request<A>) {
        if request.is_urgent() {
            self.urgent += 1;
        } else {
            self.normal += 1;
        }
    }

    const fn is_zero(self) -> bool {
        self.urgent == 0 && self.normal == 0
    }

    const fn remove(&mut self, taken: Count) {
        self.urgent -= taken.urgent;
        self.normal -= taken.normal;
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Round<A> {
    askers: Vec<A>,
    releases: Vec<u64>,
    count: Count,
}

impl<A> Round<A> {
    fn add(&mut self, wants: &mut Wants, request: Request<A>) {
        self.count.add(&request);
        match request {
            Request::Power(is_on) => wants.power = is_on,
            Request::Pass(number) => wants.pass = number,
            Request::Trial(trial) => wants.trial = trial,
            Request::Link(is_on) => wants.link = is_on,
            Request::Cell(is_cell_only) => wants.is_cell_only = is_cell_only,
            Request::Route(is_routed) => wants.is_routed = is_routed,
            Request::Rest(is_resting) => wants.is_resting = is_resting,
            Request::Hold(answer) => self.askers.push(answer),
            Request::Release(stay) => self.releases.push(stay),
        }
    }

    fn release_count(&self, stay: u64) -> usize {
        self.releases.iter().filter(|number| **number == stay).count()
    }
}

/// How the device is in a network.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Way {
    Wifi,
    Cell,
}

/// The end of a join that was to prove a network for the list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TrialEnd {
    // The place it was joined at, and left again; or why the network is not proved.
    Done(Result<Place, TrialError>),
    // A new request came first; the join is made again once that is seen through, if it is still wanted.
    GaveWay,
}

/// The outcome of a pass of listening: its number, and the networks, or none if the radio could not listen.
#[derive(PartialEq, Eq, Debug)]
pub struct ScanResult {
    pub pass: u32,
    pub networks: Option<Vec<Network>>,
}

/// The outcome of a join that was to prove a network: its number, and the place it was joined at, or why it is not proved.
#[derive(PartialEq, Eq, Debug)]
pub struct TrialResult {
    pub number: u32,
    pub outcome: Result<Place, TrialError>,
}

/// The end of a round: how the core and the stay are left, its outcomes, and the requests it handled.
#[derive(PartialEq, Eq, Debug)]
pub struct RoundEnd {
    has_power: bool,
    link: Option<Way>,
    scan: Option<ScanResult>,
    trial: Option<TrialResult>,
    count: Count,
}

/// What a shutdown does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shut {
    Off,
    Wait(Duration),
    Late(&'static str),
}

/// The radio thread's report: how the device is in a network, if it is, the last pass of listening that ended, and the last join that was to prove a network.
#[derive(Default)]
pub struct Report {
    pub link: Option<Way>,
    pub scan: Option<ScanResult>,
    pub trial: Option<TrialResult>,
}

/// The state the radio's thread, its askers and the main loop share; `G` is a grant.
pub struct State<G> {
    requests: Vec<Request<u64>>,
    answers: BTreeMap<u64, Result<G, String>>,
    holds: u64,
    is_ended: bool,
    power: Ticket,
    link: Ticket,
    is_cell_wanted: bool,
    is_route_wanted: bool,
    is_resting: bool,
    pass: Ticket,
    trial: Ticket,
    has_no_power: bool,
    count: Count,
    report: Report,
}

impl<G> Default for State<G> {
    fn default() -> State<G> {
        State {
            requests: Vec::new(),
            answers: BTreeMap::new(),
            holds: 0,
            is_ended: false,
            power: Ticket::default(),
            link: Ticket::default(),
            is_cell_wanted: false,
            is_route_wanted: false,
            is_resting: false,
            pass: Ticket::default(),
            trial: Ticket::default(),
            has_no_power: true,
            count: Count::default(),
            report: Report::default(),
        }
    }
}

impl<G> State<G> {
    /// Puts a request on its way, unless the thread has ended.
    pub fn ask(&mut self, request: Request<u64>) {
        if !self.is_ended {
            self.count.add(&request);
            self.requests.push(request);
        }
    }

    /// Asks for the network; the number is the answer's.
    pub fn ask_hold(&mut self) -> u64 {
        self.holds += 1;
        self.ask(Request::Hold(self.holds));
        self.holds
    }

    /// Takes the requests that piled up.
    pub fn take_requests(&mut self) -> Vec<Request<u64>> {
        core::mem::take(&mut self.requests)
    }

    /// Answers the hold of that number.
    pub fn answer(&mut self, number: u64, answer: Result<G, String>) {
        self.answers.insert(number, answer);
    }

    /// Takes the answer to the hold of that number, once there is one.
    pub fn take_answer(&mut self, number: u64) -> Option<Result<G, String>> {
        self.answers.remove(&number).or(self.is_ended.then(|| Err(THREAD_GONE.into())))
    }

    /// Notes the thread's end: no request goes out any more, nobody waits for an answer, and the device is in no network through it.
    pub const fn end_thread(&mut self) {
        (self.is_ended, self.report.link) = (true, None);
    }

    // Puts the request of a wish on its way where the wish is new; true if it went out.
    fn ask_new(&mut self, is_new: bool, request: impl FnOnce() -> Request<u64>) -> bool {
        if is_new {
            self.ask(request());
        }
        is_new
    }

    /// Asks for the core by the console's ask of that number, or for none; true if a request went out, as it does again for an ask anew after a core that did not come on spent the wish.
    pub fn want_power(&mut self, ask: Option<u32>) -> bool {
        let is_new = self.power.want(ask);
        self.ask_new(is_new, || Request::Power(ask.is_some()))
    }

    /// Asks for a link by the console's ask of that number, or for none; true if a request went out, as it does again for an ask anew after a failed or lost link spent the wish.
    pub fn want_link(&mut self, ask: Option<u32>) -> bool {
        let is_new = self.link.want(ask);
        self.ask_new(is_new, || Request::Link(ask.is_some()))
    }

    /// Sets whether the modem's network alone is wanted, WiFi passed over; true if a request went out.
    pub fn want_cell(&mut self, is_wanted: bool) -> bool {
        let is_new = core::mem::replace(&mut self.is_cell_wanted, is_wanted) != is_wanted;
        self.ask_new(is_new, || Request::Cell(is_wanted))
    }

    /// Sets whether every stay is to be routed over the modem's network; true if a request went out.
    pub fn want_route(&mut self, is_wanted: bool) -> bool {
        let is_new = core::mem::replace(&mut self.is_route_wanted, is_wanted) != is_wanted;
        self.ask_new(is_new, || Request::Route(is_wanted))
    }

    /// Sets whether the device sleeps; true if a request went out.
    pub fn set_resting(&mut self, is_resting: bool) -> bool {
        let is_new = core::mem::replace(&mut self.is_resting, is_resting) != is_resting;
        self.ask_new(is_new, || Request::Rest(is_resting))
    }

    /// Asks for a pass of listening by its number, once for every pass, or for none any more; true if a request went out.
    pub fn want_pass(&mut self, pass: Option<u32>) -> bool {
        let is_new = self.pass.want(pass);
        self.ask_new(is_new, || Request::Pass(pass))
    }

    /// Asks for a join that is to prove a network by its number, once for every join, or for none any more; true if a request went out.
    pub fn want_trial(&mut self, trial: Option<(u32, &Change)>) -> bool {
        let is_new = self.trial.want(trial.map(|(number, _)| number));
        self.ask_new(is_new, || Request::Trial(trial.map(|(number, change)| (number, change.clone()))))
    }

    const fn is_idle(&self) -> bool {
        self.count.is_zero()
    }

    /// The thread's wait: none, but at once while requests are counted that it has not taken, as a scan or join that gave way to them has taken their nudge.
    pub const fn next_wait(&self) -> Option<Duration> {
        if self.is_idle() { None } else { Some(Duration::ZERO) }
    }

    /// What a shutdown does next, with `remaining` of its time.
    pub const fn next_shut(&self, remaining: Duration) -> Shut {
        if self.is_off() {
            Shut::Off
        } else if remaining.is_zero() {
            Shut::Late(NOT_OFF)
        } else {
            Shut::Wait(remaining)
        }
    }

    /// True while more urgent requests are on their way than the `count` a scan or a join works for: it gives way.
    pub const fn has_more(&self, count: usize) -> bool {
        self.count.urgent > count
    }

    /// Notes the core coming up, which counts as having power.
    pub const fn note_powering(&mut self) {
        self.has_no_power = false;
    }

    /// Takes the end of a round.
    pub fn end_round(&mut self, end: RoundEnd) {
        self.has_no_power = !end.has_power;
        self.report.link = end.link;
        self.tell(end.scan, end.trial);
        self.count.remove(end.count);
    }

    /// Takes how a pass of listening and a join that was to prove a network ended, where they did.
    pub fn tell(&mut self, scan: Option<ScanResult>, trial: Option<TrialResult>) {
        if scan.is_some() {
            self.report.scan = scan;
        }
        if trial.is_some() {
            self.report.trial = trial;
        }
    }

    /// True while the core has no power and no request is on its way to the thread, and once the thread has ended, which works no more.
    pub const fn is_off(&self) -> bool {
        self.is_ended || (self.is_idle() && self.has_no_power)
    }

    /// Takes the report; a pass or a join that ended may be asked for again.
    pub fn take_report(&mut self) -> Report {
        let report = Report { link: self.report.link, scan: self.report.scan.take(), trial: self.report.trial.take() };
        if let Some(scan) = &report.scan {
            self.pass.answer(scan.pass);
        }
        if let Some(trial) = &report.trial {
            self.trial.answer(trial.number);
        }
        report
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Wants {
    power: bool,
    pass: Option<u32>,
    trial: Option<(u32, Change)>,
    link: bool,
    is_cell_only: bool,
    is_routed: bool,
    is_resting: bool,
}

impl Wants {
    // WiFi serves no stay: the console passed it over, or every stay is routed, which the modem's connection alone is.
    const fn is_cell_alone(&self) -> bool {
        self.is_cell_only || self.is_routed
    }

    // The refusal of whoever asks: asleep, or why no way into a network can serve where `no_way` tells it.
    fn take_refusal(&mut self, no_way: Option<&'static str>) -> Option<&'static str> {
        let reason = match (self.is_resting, no_way) {
            (true, _) => ASLEEP,
            (false, Some(reason)) => reason,
            (false, None) => return None,
        };
        self.spend_link();
        Some(reason)
    }

    const fn is_awake(&self) -> bool {
        !self.is_resting
    }

    const fn is_on_trial(&self) -> bool {
        self.trial.is_some()
    }

    const fn end_pass(&mut self) {
        self.pass = None;
    }

    fn trial(&self) -> Option<&Change> {
        self.trial.as_ref().map(|(_, change)| change)
    }

    fn end_trial(&mut self, end: Option<TrialEnd>) -> Option<(u32, Result<Place, TrialError>)> {
        let outcome = match end {
            Some(TrialEnd::GaveWay) => return None,
            Some(TrialEnd::Done(outcome)) => outcome,
            None => Err(TrialError::NoRadio),
        };
        self.trial.take().map(|(number, _)| (number, outcome))
    }

    const fn should_link(&self, has_waiters: bool, is_held: bool) -> bool {
        !self.is_resting && (self.link || has_waiters || is_held)
    }

    const fn should_power(&self, is_wifi_linked: bool) -> bool {
        self.power || self.pass.is_some() || self.trial.is_some() || is_wifi_linked
    }

    // A join of either way may begin: a link is wanted and no stay stands; a join that is to prove a network goes first, the waiters seen to once it is over.
    const fn may_join(&self, is_linked: bool, has_stay: bool) -> bool {
        is_linked && self.trial.is_none() && !has_stay
    }

    const fn spend_link(&mut self) {
        self.link = false;
    }

    const fn spend_power(&mut self) {
        (self.power, self.link) = (false, false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stay {
    number: u64,
    holders: usize,
    way: Way,
    is_routed: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Stage {
    #[default]
    Refusal,
    Trial,
    Askers,
    Link,
    LowBand,
    Power,
    Survey,
    Prove,
    Join,
    Cell,
    Grant,
    Quality,
    Look,
    PowerOff,
    // The thread's last round: the stay ends and the core goes off.
    Ending,
    End,
}

/// What a round of the radio's thread does next.
#[derive(PartialEq, Eq, Debug)]
pub enum Step {
    // Tells the holds of these numbers why there is no network.
    Refuse(Vec<u64>, String),
    // Hands the stay to the hold of that number.
    Grant(u64),
    // Asks how the WiFi link just joined does, for the log: once whoever waited has the stay, as nobody waits for the answer.
    Quality,
    EndStay,
    // Looks for the network just left on its low band channel.
    LowBand,
    // `Rounds::set_powered` takes the outcome.
    PowerUp,
    PowerDown,
    // Listens for the list of networks; `Rounds::end_survey` takes the outcome.
    Survey,
    // Proves the network of this change; `Rounds::end_trial` takes the outcome.
    Prove(Change),
    // Joins the default network; `Rounds::begin_stay` or `Rounds::fail_join` takes the outcome.
    Join,
    // Asks the modem for a data connection; `Rounds::begin_stay` or `Rounds::fail_join` takes the outcome.
    CellJoin,
    // Looks for the known networks where they were joined before, a stay over the modem having ended; `Rounds::end_look` takes the word.
    Look,
    // Tells the list how a pass and a trial ended before a join, which may take as long as an attach; `State::tell` takes it.
    Tell(Option<ScanResult>, Option<TrialResult>),
    End(RoundEnd),
}

/// The rounds of the radio's thread: the wishes, the stay, its waiters, and the stage of a round.
#[derive(Default)]
pub struct Rounds {
    wants: Wants,
    stay: Option<Stay>,
    stay_count: u64,
    waiters: Vec<u64>,
    is_known: bool,
    // A known network has a place, which a narrow look reaches.
    is_placed: bool,
    // The modem takes asks for a data connection.
    is_cellular: bool,
    // WiFi serves no join this round, its core not on or its join failed: the modem's connection stands in.
    is_wifi_failed: bool,
    // The modem's join failed this round: WiFi serves alone, with nothing standing in.
    is_cell_failed: bool,
    // Known networks that were heard and did not serve, each passed over until that boot time while the modem takes asks.
    passed_over: Vec<(Entry, Duration)>,
    // The last look's word: no known network is heard, so a stay the modem stands in for goes to the modem first.
    is_wifi_away: bool,
    // A look for the known networks is owed since a stay over the modem ended.
    is_look_due: bool,
    // Boot time as of the round's beginning.
    now: Duration,
    joining: Option<Way>,
    is_powered: bool,
    is_linked: bool,
    round: Round<u64>,
    stage: Stage,
    refusal: Option<Step>,
    scan: Option<ScanResult>,
    trial: Option<TrialResult>,
}

impl Rounds {
    /// Begins a round with the requests that piled up.
    pub fn begin(&mut self, requests: Vec<Request<u64>>) {
        self.round = Round::default();
        requests.into_iter().for_each(|request| self.round.add(&mut self.wants, request));
        if let Some(stay) = &mut self.stay {
            stay.holders -= self.round.release_count(stay.number);
        }
        (self.stage, self.joining) = (Stage::Refusal, None);
        (self.is_wifi_failed, self.is_cell_failed) = (false, false);
    }

    /// Begins the thread's last round, as it can wait no more: the stay ends and the core goes off.
    pub fn end(&mut self) {
        (self.round, self.stage) = (Round::default(), Stage::Ending);
    }

    /// Takes what a join can draw on this round, at `now` in boot time: whether the modem takes asks, and the known networks with their places.
    pub fn set_ways(&mut self, is_cellular: bool, known: &Known, places: &Places, now: Duration) {
        self.passed_over.retain(|(_, until)| now < *until);
        (self.is_cellular, self.now) = (is_cellular, now);
        let joinable = self.to_joinable(known.clone());
        (self.is_known, self.is_placed) = (!joinable.is_empty(), places.holds_known(&joinable));
    }

    /// Those of `known` a stay's join looks for: where the modem takes asks, none that is passed over.
    pub fn to_joinable(&self, mut known: Known) -> Known {
        if self.is_cellular {
            known.retain(|network| !self.passed_over.iter().any(|(passed, _)| passed == network));
        }
        known
    }

    // The modem stands in for WiFi this round: it takes asks, and its own join has not failed.
    const fn is_backed(&self) -> bool {
        self.is_cellular && !self.is_cell_failed
    }

    /// True where a WiFi join for `purpose` this round is to look only where it joined before: the modem stands in where a stay's hears nothing there, and never for a trial's, which is the owner's own asking of one network.
    pub const fn is_look_narrow(&self, purpose: Purpose) -> bool {
        matches!(purpose, Purpose::Stay) && self.is_backed()
    }

    /// The time the steps of a WiFi join for `purpose` get this round: short, with its reach proved, where the modem stands in for a network that takes longer, which is where its look is narrow.
    pub const fn join_limits(&self, purpose: Purpose) -> Limits {
        if self.is_look_narrow(purpose) { Limits::BACKED } else { Limits::ALONE }
    }

    /// Notes a known network that was heard and did not serve a stay's join: where the modem stands in, it is passed over for as long as the modem keeps an attach, and the log's line is returned.
    pub fn pass_over(&mut self, network: Entry) -> Option<String> {
        self.is_backed().then(|| {
            let line = to_passed_over_line(&network);
            self.passed_over.retain(|(passed, _)| *passed != network);
            self.passed_over.push((network, self.now + KEEP));
            line
        })
    }

    /// Takes the word of a look after a stay over the modem: whether a known network is heard, which decides the next stay's first way, with the log's line; a look that gave way or could not be made is owed still.
    pub const fn end_look(&mut self, word: Option<bool>) -> Option<&'static str> {
        match word {
            Some(is_heard) => {
                self.is_wifi_away = !is_heard;
                Some(if is_heard { NEAR_LINE } else { AWAY_LINE })
            }
            None => {
                self.is_look_due = true;
                None
            }
        }
    }

    // A look can tell the next stay's way: the modem takes asks, and a network a stay may join has a place to look at.
    const fn can_look(&self) -> bool {
        self.is_cellular && self.is_known && self.is_placed && !self.wants.is_cell_alone()
    }

    /// True where the stay a join of the modem's makes this round is to be routed.
    pub const fn is_routed(&self) -> bool {
        self.wants.is_routed
    }

    // Why no way into a network can serve, where none can: no network known and no modem, or the modem's alone wanted and the modem taking no asks.
    const fn to_no_way(&self) -> Option<&'static str> {
        match (self.is_cellular, self.wants.is_cell_alone(), self.is_known) {
            (true, ..) | (false, false, true) => None,
            (false, true, _) => Some(NOT_READY),
            (false, false, false) => Some(UNKNOWN),
        }
    }

    // A WiFi join can serve: a network is known, its join has not failed this round, the console has not passed WiFi over, a narrow look has a place to look at, and where the modem stands in the last look heard a known network.
    const fn can_join_wifi(&self) -> bool {
        self.is_known
            && !self.is_wifi_failed
            && !self.wants.is_cell_alone()
            && (self.is_placed || !self.is_backed())
            && !(self.is_backed() && self.is_wifi_away)
    }

    // The link wanted goes over WiFi: a WiFi stay stands, or no stay stands and a WiFi join can serve.
    fn needs_wifi(&self) -> bool {
        self.is_linked
            && match self.stay {
                Some(stay) => stay.way == Way::Wifi,
                None => self.can_join_wifi(),
            }
    }

    /// The urgent requests the round handles: a scan or a join gives way to any more.
    pub const fn urgent_count(&self) -> usize {
        self.round.count.urgent
    }

    /// Notes the stay lost; one lost after the round's survey has the round join nothing for its holders, as one lost at its start does.
    pub const fn lose_stay(&mut self) {
        self.wants.spend_link();
        (self.stay, self.is_linked) = (None, false);
    }

    /// Takes the outcome of a power-up: a wish that could not be met is spent, and whoever waits is told, unless the modem stands in.
    pub fn set_powered(&mut self, is_powered: bool) {
        self.is_powered = is_powered;
        if is_powered {
            return;
        }
        self.is_look_due = false;
        if self.is_backed() && self.is_linked {
            self.is_wifi_failed = true;
            self.wants.power = false;
            return;
        }
        self.wants.spend_power();
        self.is_linked = false;
        self.refusal = Some(Step::Refuse(mem::take(&mut self.waiters), NO_POWER.into()));
    }

    /// Takes the outcome of a pass of listening; one that gave way is made again.
    pub fn end_survey(&mut self, outcome: ScanOutcome) {
        let networks = match outcome {
            ScanOutcome::Found(networks) => Some(networks),
            ScanOutcome::GaveWay => return,
            ScanOutcome::Failed => None,
        };
        if let Some(pass) = self.wants.pass {
            self.wants.end_pass();
            self.scan = Some(ScanResult { pass, networks });
        }
    }

    /// Takes the end of a trial; none where the core had no power to make it. A network it proved is passed over no more, and the next stay looks for WiFi first.
    pub fn end_trial(&mut self, end: Option<TrialEnd>) {
        if let Some(TrialEnd::Done(Ok(place))) = &end {
            self.passed_over.retain(|(passed, _)| *passed != place.network);
            self.is_wifi_away = false;
        }
        self.trial = self.wants.end_trial(end).map(|(number, outcome)| TrialResult { number, outcome });
    }

    /// Takes a join that succeeded: the number of its stay, which goes to whoever waits.
    pub const fn begin_stay(&mut self, way: Way) -> u64 {
        self.stay_count += 1;
        let is_routed = self.wants.is_routed && matches!(way, Way::Cell);
        self.stay = Some(Stay { number: self.stay_count, holders: 0, way, is_routed });
        self.stage = Stage::Grant;
        self.is_wifi_away &= !matches!(way, Way::Wifi);
        self.is_look_due = false;
        self.stay_count
    }

    /// Takes a join that failed. A WiFi join the modem stands in for is told nobody, and the next stay goes to the modem first; nor is a join of the modem's that WiFi can still serve after.
    pub fn fail_join(&mut self, reason: String) {
        match self.joining {
            Some(Way::Wifi) if self.is_backed() => {
                (self.is_wifi_failed, self.is_wifi_away) = (true, true);
                return;
            }
            Some(Way::Cell) => {
                self.is_cell_failed = true;
                if self.can_join_wifi() {
                    self.stage = Stage::Power;
                    return;
                }
            }
            _ => {}
        }
        self.wants.spend_link();
        self.is_linked = false;
        self.refusal = Some(Step::Refuse(mem::take(&mut self.waiters), reason));
    }

    // How a pass and a trial ended, where the round holds either, which the list is told before a join: it waits on no join.
    fn take_outcomes(&mut self) -> Option<Step> {
        (self.scan.is_some() || self.trial.is_some()).then(|| Step::Tell(self.scan.take(), self.trial.take()))
    }

    /// What the round does next.
    pub fn next_step(&mut self) -> Step {
        if let Some(refusal) = self.refusal.take() {
            return refusal;
        }
        loop {
            match self.stage {
                Stage::Refusal => {
                    self.stage = Stage::Trial;
                    if let Some(reason) = self.wants.take_refusal(self.to_no_way()) {
                        self.waiters.append(&mut self.round.askers);
                        return Step::Refuse(mem::take(&mut self.waiters), reason.into());
                    }
                }
                Stage::Trial => {
                    self.stage = Stage::Askers;
                    // A join that is to prove a network has the interface to itself, and the modem's network alone leaves no WiFi stay; a stay of the modem's goes on beside either, but one set up otherwise than stays are wanted now.
                    let ends_wifi = self.wants.is_on_trial() || self.wants.is_cell_alone();
                    let is_routed = self.wants.is_routed;
                    let ends = |stay: Stay| match stay.way {
                        Way::Wifi => ends_wifi,
                        Way::Cell => stay.is_routed != is_routed,
                    };
                    if self.stay.is_some_and(ends) {
                        self.stay = None;
                        return Step::EndStay;
                    }
                }
                Stage::Askers if self.round.askers.is_empty() => self.stage = Stage::Link,
                Stage::Askers => {
                    let number = self.round.askers.remove(0);
                    match &mut self.stay {
                        Some(stay) => {
                            stay.holders += 1;
                            return Step::Grant(number);
                        }
                        None => self.waiters.push(number),
                    }
                }
                Stage::Link => {
                    let is_held = self.stay.is_some_and(|stay| stay.holders > 0);
                    self.is_linked = self.wants.should_link(!self.waiters.is_empty(), is_held);
                    self.stage = Stage::Power;
                    // A look owed waits for a round nobody asks for a network in.
                    self.is_look_due &= !self.is_linked;
                    if !self.is_linked
                        && let Some(stay) = self.stay.take()
                    {
                        match stay.way {
                            Way::Wifi => self.stage = Stage::LowBand,
                            Way::Cell => self.is_look_due = self.can_look(),
                        }
                        return Step::EndStay;
                    }
                }
                Stage::LowBand => {
                    self.stage = Stage::Power;
                    if self.wants.is_awake() {
                        return Step::LowBand;
                    }
                }
                Stage::Power => {
                    self.stage = Stage::Survey;
                    let should_power = self.wants.should_power(self.needs_wifi()) || self.is_look_due;
                    if should_power && !self.is_powered {
                        return Step::PowerUp;
                    }
                    if !should_power && self.is_powered {
                        self.is_powered = false;
                        return Step::PowerDown;
                    }
                }
                Stage::Survey => {
                    self.stage = Stage::Prove;
                    if self.wants.pass.is_some() {
                        if self.is_powered {
                            return Step::Survey;
                        }
                        self.end_survey(ScanOutcome::Failed);
                    }
                }
                Stage::Prove => {
                    self.stage = Stage::Join;
                    if let Some(change) = self.wants.trial() {
                        if self.is_powered {
                            return Step::Prove(change.clone());
                        }
                        self.end_trial(None);
                    }
                }
                Stage::Join => {
                    let is_wifi = self.can_join_wifi() && self.is_powered;
                    if is_wifi && self.wants.may_join(self.is_linked, self.stay.is_some()) {
                        if let Some(tell) = self.take_outcomes() {
                            return tell;
                        }
                        (self.stage, self.joining) = (Stage::Cell, Some(Way::Wifi));
                        return Step::Join;
                    }
                    self.stage = Stage::Cell;
                }
                Stage::Cell => {
                    let is_cell = self.is_backed() && !self.can_join_wifi();
                    if is_cell && self.wants.may_join(self.is_linked, self.stay.is_some()) {
                        // The WiFi core goes off before the modem's join, which may take as long as an attach.
                        if self.is_powered && !self.wants.should_power(self.needs_wifi()) {
                            self.is_powered = false;
                            return Step::PowerDown;
                        }
                        if let Some(tell) = self.take_outcomes() {
                            return tell;
                        }
                        (self.stage, self.joining) = (Stage::PowerOff, Some(Way::Cell));
                        return Step::CellJoin;
                    }
                    self.stage = Stage::Look;
                }
                Stage::Grant if self.waiters.is_empty() => self.stage = Stage::Quality,
                Stage::Grant => {
                    if let Some(stay) = &mut self.stay {
                        stay.holders += 1;
                    }
                    return Step::Grant(self.waiters.remove(0));
                }
                Stage::Quality => {
                    self.stage = Stage::Look;
                    if self.stay.is_some_and(|stay| stay.way == Way::Wifi) {
                        return Step::Quality;
                    }
                }
                Stage::Look => {
                    self.stage = Stage::PowerOff;
                    if mem::take(&mut self.is_look_due) && self.is_powered {
                        return Step::Look;
                    }
                }
                Stage::PowerOff => {
                    self.stage = Stage::End;
                    // Power only the list needed goes before the list is told how it went: a frame drawn while the core goes off takes four times as long as another.
                    if !self.wants.should_power(self.needs_wifi()) && self.is_powered {
                        self.is_powered = false;
                        return Step::PowerDown;
                    }
                }
                Stage::Ending => {
                    if self.stay.take().is_some() {
                        return Step::EndStay;
                    }
                    self.stage = Stage::End;
                    if mem::take(&mut self.is_powered) {
                        return Step::PowerDown;
                    }
                }
                Stage::End => {
                    return Step::End(RoundEnd {
                        has_power: self.is_powered,
                        link: self.stay.map(|stay| stay.way),
                        scan: self.scan.take(),
                        trial: self.trial.take(),
                        count: self.round.count,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::known;
    use crate::wifi::known::Entry;
    use crate::wifi::known::TrialError;
    use alloc::string::String;

    #[test]
    fn link_wish() {
        let mut wants = Wants::default();
        assert!(!wants.should_link(false, false), "nobody asks");
        assert!(wants.should_link(true, false) && wants.should_link(false, true), "one waits, or one holds the stay");
        wants.link = true;
        assert!(wants.should_link(false, false), "the console asks");
        wants.is_resting = true;
        assert!(!wants.should_link(true, true), "asleep, whoever asks");
        wants.is_resting = false;
        wants.spend_link();
        assert!(!wants.should_link(false, false), "a link that is spent is wanted no more");
    }

    #[test]
    fn power_and_join_wishes() {
        let mut wants = Wants::default();
        assert!(!wants.should_power(false) && wants.should_power(true), "for a link");
        wants.pass = Some(3);
        assert!(wants.should_power(false), "for the list's pass");
        wants.pass = None;
        wants.trial = Some((4, Change::ForgetAll));
        assert!(wants.should_power(false), "for the list's join");
        assert!(!wants.may_join(true, false), "which goes before the default network's");
        wants.trial = None;
        assert!(wants.may_join(true, false), "then it is joined");
        assert!(
            !wants.may_join(false, false) && !wants.may_join(true, true),
            "but not unwanted, nor beside a stay that stands"
        );
        (wants.power, wants.link) = (true, true);
        wants.spend_power();
        assert_eq!(wants, Wants::default(), "a core that did not come on spends both wishes");
    }

    #[test]
    fn round_requests() {
        let remember = |name: &str| Change::Remember(known::Network { name: name.into(), password: String::new() });
        let (mut round, mut wants) = (Round::default(), Wants::default());
        for request in [
            Request::Power(true),
            Request::Hold('a'),
            Request::Pass(Some(3)),
            Request::Trial(Some((4, remember("home")))),
            Request::Release(7),
            Request::Link(true),
            Request::Power(false),
            Request::Hold('b'),
            Request::Pass(None),
            Request::Trial(Some((5, remember("cafe")))),
            Request::Release(7),
            Request::Release(6),
            Request::Rest(true),
            Request::Cell(true),
        ] {
            round.add(&mut wants, request);
        }
        assert_eq!(
            wants,
            Wants {
                power: false,
                pass: None,
                trial: Some((5, remember("cafe"))),
                link: true,
                is_cell_only: true,
                is_routed: false,
                is_resting: true,
            },
            "of each kind the last wish counts"
        );
        assert_eq!(round.askers, ['a', 'b'], "every asker in turn");
        assert_eq!(
            (round.release_count(7), round.release_count(6), round.release_count(8)),
            (2, 1, 0),
            "each letting go counts for its stay"
        );
        assert_eq!(round.count, Count { urgent: 9, normal: 5 });
    }

    // The round's end, past the asking how a WiFi link just joined does.
    fn round_end(rounds: &mut Rounds) -> RoundEnd {
        match rounds.next_step() {
            Step::Quality => round_end(rounds),
            Step::End(end) => end,
            other => panic!("the round goes on with {other:?}"),
        }
    }

    #[test]
    fn hold_join_grant() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Hold(1), Request::Hold(2)]);
        rounds.is_known = true;
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        assert_eq!(rounds.begin_stay(Way::Wifi), 1);
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::Grant(1), Step::Grant(2)));
        assert_eq!(rounds.next_step(), Step::Quality, "how the link does is asked once nobody waits for the stay");
        let Step::End(end) = rounds.next_step() else { panic!("asked once") };
        assert!(end.has_power && end.link == Some(Way::Wifi) && end.count == Count { urgent: 0, normal: 2 });
    }

    #[test]
    fn stay_end_on_release() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Hold(1)]);
        rounds.is_known = true;
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Hold(3)]);
        assert_eq!(rounds.next_step(), Step::Grant(3), "a stay that stands is the asker's at once");
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Release(1)]);
        assert!(round_end(&mut rounds).link.is_some(), "one holder is left");
        rounds.begin(alloc::vec![Request::Release(1)]);
        assert_eq!(
            (rounds.next_step(), rounds.next_step(), rounds.next_step()),
            (Step::EndStay, Step::LowBand, Step::PowerDown)
        );
        let end = round_end(&mut rounds);
        assert!(!end.has_power && end.link.is_none());
    }

    #[test]
    fn rest_stay_end() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Link(true)]);
        rounds.is_known = true;
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Rest(true)]);
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::Refuse(Vec::new(), ASLEEP.into()), Step::EndStay));
        assert_eq!(rounds.next_step(), Step::PowerDown, "and no look on the low band");
        round_end(&mut rounds);
    }

    #[test]
    fn waiter_refusals() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Hold(1), Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![1, 2], UNKNOWN.into()));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Hold(3)]);
        rounds.is_known = true;
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(false);
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![3], NO_POWER.into()));
        assert!(!round_end(&mut rounds).has_power);
        rounds.begin(alloc::vec![Request::Hold(4)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.fail_join(String::from("not heard"));
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![4], String::from("not heard")));
        assert_eq!(rounds.next_step(), Step::PowerDown);
        assert!(!round_end(&mut rounds).has_power);
    }

    #[test]
    fn pass_trial_power() {
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Pass(Some(3)), Request::Trial(Some((4, Change::ForgetAll)))]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(false);
        assert_eq!(rounds.next_step(), Step::Refuse(Vec::new(), NO_POWER.into()));
        let end = round_end(&mut rounds);
        assert_eq!(end.scan, Some(ScanResult { pass: 3, networks: None }));
        assert_eq!(end.trial, Some(TrialResult { number: 4, outcome: Err(TrialError::NoRadio) }));
        rounds.begin(alloc::vec![Request::Pass(Some(5)), Request::Trial(Some((6, Change::ForgetAll)))]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Survey);
        rounds.end_survey(ScanOutcome::GaveWay);
        assert_eq!(rounds.next_step(), Step::Prove(Change::ForgetAll));
        rounds.end_trial(Some(TrialEnd::GaveWay));
        let end = round_end(&mut rounds);
        assert!(end.has_power && end.scan.is_none() && end.trial.is_none(), "what gave way is made again");
    }

    #[test]
    fn outcomes_before_join() {
        let mut rounds = Rounds { is_cellular: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Pass(Some(3)), Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Survey);
        rounds.end_survey(ScanOutcome::Found(Vec::new()));
        assert_eq!(rounds.next_step(), Step::PowerDown, "the core goes off once the pass is over");
        assert_eq!(
            (rounds.next_step(), rounds.next_step()),
            (Step::Tell(Some(ScanResult { pass: 3, networks: Some(Vec::new()) }), None), Step::CellJoin),
            "and the list is told before the modem's join"
        );
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        let end = round_end(&mut rounds);
        assert!(end.scan.is_none() && end.link == Some(Way::Cell), "nothing is told twice");
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Trial(Some((4, Change::ForgetAll))), Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Prove(Change::ForgetAll));
        rounds.end_trial(Some(TrialEnd::Done(Err(TrialError::NotHeard))));
        assert_eq!(
            (rounds.next_step(), rounds.next_step()),
            (Step::Tell(None, Some(TrialResult { number: 4, outcome: Err(TrialError::NotHeard) })), Step::Join),
            "a trial's word comes before the join of the default network too"
        );
    }

    #[test]
    fn stay_lost_after_survey() {
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Pass(Some(3))]);
        assert_eq!(rounds.next_step(), Step::Survey, "a pass during the stay");
        rounds.end_survey(ScanOutcome::Found(Vec::new()));
        rounds.lose_stay();
        assert_eq!(rounds.next_step(), Step::PowerDown, "a loss its scans read joins nothing for the stay's holders");
        let end = round_end(&mut rounds);
        assert!(end.link.is_none() && end.scan == Some(ScanResult { pass: 3, networks: Some(Vec::new()) }));
    }

    #[test]
    fn ending_round() {
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.end();
        assert_eq!(
            (rounds.next_step(), rounds.next_step()),
            (Step::EndStay, Step::PowerDown),
            "the stay ends, with no look on the low band, and the core goes off"
        );
        let end = round_end(&mut rounds);
        assert!(!end.has_power && end.link.is_none() && end.count == Count::default(), "{end:?}");
        let mut idle = Rounds::default();
        idle.end();
        assert!(!round_end(&mut idle).has_power, "with nothing to end, nothing is done");
    }

    #[test]
    fn cell_without_wifi_known() {
        let mut rounds = Rounds::default();
        (rounds.is_known, rounds.is_cellular) = (false, true);
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::CellJoin, "no refusal and no WiFi power");
        assert_eq!(rounds.begin_stay(Way::Cell), 1);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        let Step::End(end) = rounds.next_step() else { panic!("nothing is asked of the modem's link") };
        assert!(!end.has_power && end.link == Some(Way::Cell), "the modem's stay, told as the modem's");
        rounds.begin(alloc::vec![Request::Release(1)]);
        assert_eq!(rounds.next_step(), Step::EndStay);
        assert!(round_end(&mut rounds).link.is_none(), "a stay of the modem's looks for no low band");
    }

    #[test]
    fn cell_after_wifi_failure() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Hold(1)]);
        (rounds.is_known, rounds.is_placed, rounds.is_cellular) = (true, true, true);
        assert!(rounds.is_look_narrow(Purpose::Stay), "WiFi looks where it joined before alone");
        assert!(
            !rounds.is_look_narrow(Purpose::Trial) && rounds.join_limits(Purpose::Trial) == Limits::ALONE,
            "a join that is to prove a network looks everywhere with every step's whole time, whatever stands in"
        );
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.fail_join(String::from("not heard"));
        assert_eq!(rounds.next_step(), Step::PowerDown, "the core goes off before the modem joins");
        assert_eq!(rounds.next_step(), Step::CellJoin);
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        assert!(!round_end(&mut rounds).has_power);
        rounds.begin(alloc::vec![Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::Grant(2), "the modem's stay serves the next asker");
        assert!(!round_end(&mut rounds).has_power, "and needs no WiFi power");
        rounds.begin(alloc::vec![Request::Trial(Some((4, Change::ForgetAll)))]);
        assert_eq!(rounds.next_step(), Step::PowerUp, "a join to prove a network leaves the modem's stay");
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Prove(Change::ForgetAll));
        rounds.end_trial(Some(TrialEnd::Done(Err(TrialError::NotHeard))));
        assert_eq!(rounds.next_step(), Step::PowerDown);
        assert_eq!(round_end(&mut rounds).link, Some(Way::Cell), "the modem's stay stands through the trial");
    }

    #[test]
    fn cell_without_wifi_power() {
        let mut rounds = Rounds::default();
        (rounds.is_known, rounds.is_placed, rounds.is_cellular) = (true, true, true);
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(false);
        assert_eq!(rounds.next_step(), Step::CellJoin, "nobody is refused");
        rounds.fail_join(String::from("no network took the device in"));
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![1], String::from("no network took the device in")));
        assert!(round_end(&mut rounds).link.is_none());
    }

    #[test]
    fn narrow_look_without_place() {
        let mut rounds = Rounds::default();
        (rounds.is_known, rounds.is_cellular) = (true, true);
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::CellJoin, "no place to look at: the modem at once");
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp, "without the modem the look goes everywhere, no place needed");
    }

    #[test]
    fn console_cell_only() {
        let mut rounds = Rounds::default();
        (rounds.is_known, rounds.is_placed, rounds.is_cellular) = (true, true, true);
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Cell(true)]);
        assert_eq!(
            (rounds.next_step(), rounds.next_step()),
            (Step::EndStay, Step::PowerDown),
            "the WiFi stay ends and the core goes off"
        );
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::CellJoin, "every hold goes to the modem, the core left off");
        rounds.fail_join(String::from("no network took the device in"));
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![2], String::from("no network took the device in")));
        round_end(&mut rounds);
        rounds.is_cellular = false;
        rounds.begin(alloc::vec![Request::Hold(3)]);
        assert_eq!(
            rounds.next_step(),
            Step::Refuse(alloc::vec![3], NOT_READY.into()),
            "with the modem taking no asks, nobody waits for it"
        );
    }

    #[test]
    fn routed_stays_cell_only() {
        let mut rounds = Rounds::default();
        (rounds.is_known, rounds.is_placed, rounds.is_cellular) = (true, true, true);
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Route(true), Request::Hold(2)]);
        assert_eq!(
            (rounds.next_step(), rounds.next_step(), rounds.next_step()),
            (Step::EndStay, Step::PowerDown, Step::CellJoin),
            "the WiFi stay ends, the core goes off, and the modem is asked"
        );
        assert!(rounds.is_routed());
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(2));
        assert_eq!(round_end(&mut rounds).link, Some(Way::Cell));
        rounds.begin(alloc::vec![Request::Hold(3)]);
        assert_eq!(rounds.next_step(), Step::Grant(3), "the routed stay serves every asker");
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Route(false)]);
        assert_eq!(
            rounds.next_step(),
            Step::EndStay,
            "routed no more, the stay that was set up routed ends, whoever holds it"
        );
        assert!(round_end(&mut rounds).link.is_none(), "its holders ask anew as they need the network");
        rounds.begin(alloc::vec![Request::Cell(true), Request::Hold(4)]);
        assert_eq!(rounds.next_step(), Step::CellJoin);
        assert!(!rounds.is_routed());
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(4));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Route(true)]);
        assert_eq!(
            rounds.next_step(),
            Step::EndStay,
            "a stay of the modem's that is not routed ends as routing begins"
        );
        assert!(round_end(&mut rounds).link.is_none());
        rounds.is_cellular = false;
        rounds.begin(alloc::vec![Request::Hold(5)]);
        assert_eq!(
            rounds.next_step(),
            Step::Refuse(alloc::vec![5], NOT_READY.into()),
            "without the modem nothing is routed, whatever network is known"
        );
    }

    fn home() -> Entry {
        Entry { name: "home".into(), is_locked: true }
    }

    // A round's ways where the modem takes asks and `home` is known with a place.
    fn backed_at(rounds: &mut Rounds, now: Duration) -> (Known, Places) {
        let known = Known::parse("name home\npassword secret12\nname cafe\npassword \n");
        let mut places = Places::default();
        places.add_if_known(Place { network: home(), frequency: 2412, other_band: None }, &known);
        rounds.set_ways(true, &known, &places, now);
        (known, places)
    }

    #[test]
    fn modem_first_when_away() {
        let mut rounds = Rounds::default();
        rounds.begin(alloc::vec![Request::Hold(1)]);
        backed_at(&mut rounds, Duration::ZERO);
        assert_eq!((rounds.next_step(), rounds.join_limits(Purpose::Stay)), (Step::PowerUp, Limits::BACKED));
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.fail_join(String::from("none of the 2 known networks is heard"));
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::PowerDown, Step::CellJoin));
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Release(1)]);
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::EndStay, Step::PowerUp));
        rounds.set_powered(true);
        assert_eq!(
            rounds.next_step(),
            Step::Look,
            "the look for WiFi is made once the stay is over, when nobody waits"
        );
        assert_eq!(rounds.end_look(Some(false)), Some(AWAY_LINE));
        assert_eq!(rounds.next_step(), Step::PowerDown);
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::CellJoin, "away, the next stay goes to the modem with no look before it");
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(2));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Release(2), Request::Rest(true)]);
        assert_eq!(
            (rounds.next_step(), rounds.next_step(), rounds.next_step()),
            (Step::Refuse(Vec::new(), ASLEEP.into()), Step::EndStay, Step::PowerUp)
        );
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Look, "a stay's end has its look as the device goes to rest too");
        assert_eq!(rounds.end_look(None), None, "a look that gave way");
        assert_eq!(rounds.next_step(), Step::PowerDown);
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Power(false)]);
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::Refuse(Vec::new(), ASLEEP.into()), Step::PowerUp));
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Look, "is owed still");
        assert_eq!(rounds.end_look(Some(true)), Some(NEAR_LINE));
        assert_eq!(rounds.next_step(), Step::PowerDown);
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Rest(false), Request::Hold(3)]);
        assert_eq!(rounds.next_step(), Step::PowerUp, "a known network heard, the next stay looks for WiFi first");
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(3));
        assert_eq!(round_end(&mut rounds).link, Some(Way::Wifi));
    }

    #[test]
    fn owed_look_waits() {
        let mut rounds = Rounds::default();
        backed_at(&mut rounds, Duration::ZERO);
        rounds.is_wifi_away = true;
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::CellJoin);
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Release(1)]);
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::EndStay, Step::PowerUp));
        rounds.set_powered(false);
        assert_eq!(rounds.next_step(), Step::Refuse(Vec::new(), NO_POWER.into()));
        assert!(!round_end(&mut rounds).has_power, "a core that does not come on makes no look");
        rounds.is_look_due = true;
        rounds.begin(alloc::vec![Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::CellJoin, "a look owed powers no core while somebody waits");
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(2));
        assert!(!round_end(&mut rounds).has_power && !rounds.is_look_due, "it is owed anew at this stay's end");
    }

    #[test]
    fn wifi_after_modem_failure() {
        let mut rounds = Rounds::default();
        backed_at(&mut rounds, Duration::ZERO);
        rounds.is_wifi_away = true;
        rounds.begin(alloc::vec![Request::Hold(1)]);
        assert_eq!(rounds.next_step(), Step::CellJoin);
        rounds.fail_join(String::from("no network took the device in"));
        assert_eq!(rounds.next_step(), Step::PowerUp, "the modem's join failed: WiFi is looked for after all");
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        assert!(
            !rounds.is_look_narrow(Purpose::Stay) && rounds.join_limits(Purpose::Stay) == Limits::ALONE,
            "everywhere and with every step's whole time, as nothing stands in any more"
        );
        rounds.begin_stay(Way::Wifi);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        assert_eq!(round_end(&mut rounds).link, Some(Way::Wifi));
        let mut rounds = Rounds::default();
        backed_at(&mut rounds, Duration::ZERO);
        rounds.is_wifi_away = true;
        rounds.begin(alloc::vec![Request::Hold(2)]);
        assert_eq!(rounds.next_step(), Step::CellJoin);
        rounds.fail_join(String::from("no network took the device in"));
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        rounds.fail_join(String::from("none of the 2 known networks is heard"));
        assert_eq!(
            rounds.next_step(),
            Step::Refuse(alloc::vec![2], String::from("none of the 2 known networks is heard")),
            "both ways failed: whoever waits is told"
        );
        assert_eq!(rounds.next_step(), Step::PowerDown);
        assert!(round_end(&mut rounds).link.is_none());
    }

    #[test]
    fn unserving_network_passed_over() {
        let (minute, mut rounds) = (Duration::from_secs(60), Rounds::default());
        rounds.begin(alloc::vec![Request::Hold(1)]);
        let (known, places) = backed_at(&mut rounds, minute);
        assert_eq!(rounds.next_step(), Step::PowerUp);
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        assert_eq!(
            rounds.pass_over(home()),
            Some(String::from("wifi: \"home\" is passed over for 15 min, the modem standing in"))
        );
        rounds.fail_join(String::from("no reach: Silent"));
        assert_eq!((rounds.next_step(), rounds.next_step()), (Step::PowerDown, Step::CellJoin));
        rounds.begin_stay(Way::Cell);
        assert_eq!(rounds.next_step(), Step::Grant(1));
        round_end(&mut rounds);
        rounds.begin(alloc::vec![Request::Release(1)]);
        rounds.set_ways(true, &known, &places, 2 * minute);
        assert_eq!(rounds.to_joinable(known.clone()).entries(), [Entry { name: "cafe".into(), is_locked: false }]);
        assert_eq!(rounds.next_step(), Step::EndStay);
        assert!(!round_end(&mut rounds).has_power, "the one network with a place is passed over: nothing to look for");
        rounds.begin(alloc::vec![Request::Hold(2)]);
        rounds.set_ways(false, &known, &places, 2 * minute);
        assert_eq!(rounds.to_joinable(known.clone()), known, "without the modem no network is passed over");
        assert_eq!(rounds.next_step(), Step::PowerUp, "and WiFi is tried, as it is all there is");
        rounds.set_powered(true);
        assert_eq!(rounds.next_step(), Step::Join);
        assert_eq!(rounds.pass_over(home()), None, "nor is one that fails then");
        rounds.fail_join(String::from("no address: Silent"));
        assert_eq!(rounds.next_step(), Step::Refuse(alloc::vec![2], String::from("no address: Silent")));
        assert_eq!(rounds.next_step(), Step::PowerDown);
        round_end(&mut rounds);
        rounds.set_ways(true, &known, &places, minute + KEEP - Duration::from_millis(1));
        assert_eq!(rounds.to_joinable(known.clone()).len(), 1, "passed over for as long as an attach is kept");
        rounds.set_ways(true, &known, &places, minute + KEEP);
        assert_eq!(rounds.to_joinable(known.clone()), known, "and no longer");
        rounds.begin(alloc::vec![Request::Trial(Some((4, Change::Prefer(home()))))]);
        rounds.set_ways(true, &known, &places, 2 * minute + KEEP);
        rounds.is_powered = true;
        assert_eq!(rounds.pass_over(home()).is_some(), rounds.to_joinable(known.clone()).len() == 1);
        rounds.is_wifi_away = true;
        let place = Place { network: home(), frequency: 2412, other_band: None };
        rounds.end_trial(Some(TrialEnd::Done(Ok(place))));
        assert!(
            rounds.to_joinable(known.clone()) == known && !rounds.is_wifi_away,
            "a network its owner's join proved is passed over no more, and WiFi is looked for first"
        );
    }

    #[test]
    fn shut_steps() {
        let second = Duration::from_secs(1);
        let mut state = State::<()>::default();
        assert_eq!(state.next_shut(second), Shut::Off);
        state.ask(Request::Pass(None));
        assert_eq!((state.next_shut(second), state.next_wait()), (Shut::Wait(second), Some(Duration::ZERO)));
        assert_eq!(state.next_shut(Duration::ZERO), Shut::Late(NOT_OFF));
        let mut rounds = Rounds { is_known: true, ..Rounds::default() };
        rounds.begin(state.take_requests());
        state.end_round(round_end(&mut rounds));
        assert_eq!(
            (state.next_shut(second), state.next_wait()),
            (Shut::Off, None),
            "the round took the request, and the core has no power"
        );
    }

    #[test]
    fn thread_end() {
        let mut state = State::<()>::default();
        let number = state.ask_hold();
        state.take_requests();
        state.note_powering();
        assert!(!state.is_off());
        state.end_thread();
        assert!(state.is_off(), "an ended thread keeps no sleep off, whatever it left counted or powered");
        assert_eq!(state.take_answer(number), Some(Err(THREAD_GONE.into())), "nobody waits for it");
        assert_eq!(state.take_report().link, None, "and the device is in no network through it");
        assert!(state.want_power(Some(1)) && state.take_requests().is_empty(), "nor is it asked anything");
    }

    #[test]
    fn console_asks() {
        let mut state = State::<()>::default();
        assert!(state.want_link(Some(1)) && !state.want_link(Some(1)), "an ask goes out once");
        assert!(state.want_link(Some(2)), "and an ask anew again, as a failed or lost link spends the wish");
        assert!(state.want_link(None) && !state.want_link(None));
        assert!(state.want_power(Some(3)) && !state.want_power(Some(3)) && state.want_power(Some(4)));
        assert_eq!(
            state.take_requests(),
            [
                Request::Link(true),
                Request::Link(true),
                Request::Link(false),
                Request::Power(true),
                Request::Power(true)
            ]
        );
    }

    #[test]
    fn urgent_requests() {
        let urgent: [Request<()>; 7] = [
            Request::Power(true),
            Request::Pass(None),
            Request::Trial(None),
            Request::Link(false),
            Request::Cell(true),
            Request::Route(true),
            Request::Rest(true),
        ];
        assert!(urgent.iter().all(Request::is_urgent));
        assert!(
            !Request::Hold(()).is_urgent() && !Request::<()>::Release(3).is_urgent(),
            "holding and letting go wait their turn"
        );
        let mut state = State::<()>::default();
        state.ask(Request::Power(true));
        state.ask_hold();
        assert!(state.has_more(0) && !state.has_more(1), "a scan or a join gives way to urgent requests alone");
    }

    #[test]
    fn refusals_and_trials() {
        let mut wants = Wants { link: true, ..Wants::default() };
        assert!(wants.take_refusal(None).is_none() && wants.link, "awake the network is handed out");
        assert_eq!(wants.take_refusal(Some(UNKNOWN)), Some(UNKNOWN), "but not where no way into one can serve");
        assert!(
            !wants.link && !wants.should_power(wants.should_link(false, false)),
            "and the wish for a link is spent, so that the core does not come on for it"
        );
        assert!(wants.is_awake());
        (wants.link, wants.is_resting) = (true, true);
        assert!(
            wants.take_refusal(None) == Some(ASLEEP) && !wants.link,
            "asleep whoever asks is refused, and the wish for a link is spent"
        );
        assert!(!wants.is_awake(), "and a stay that ends looks for nothing");
        let change = Change::ForgetAll;
        let mut wants = Wants { trial: Some((4, change.clone())), ..Wants::default() };
        assert!(wants.is_on_trial() && wants.trial() == Some(&change), "a join to prove a network has the interface");
        assert_eq!(wants.end_trial(Some(TrialEnd::GaveWay)), None, "one that gave way is made again");
        assert!(wants.is_on_trial());
        assert_eq!(wants.end_trial(None), Some((4, Err(TrialError::NoRadio))), "a core without power proves nothing");
        assert!(!wants.is_on_trial() && wants.trial().is_none(), "and it is wanted no more");
        let place =
            Place { network: Entry { name: "home".into(), is_locked: false }, frequency: 2412, other_band: None };
        let mut wants = Wants { trial: Some((5, change)), ..Wants::default() };
        assert_eq!(wants.end_trial(Some(TrialEnd::Done(Ok(place.clone())))), Some((5, Ok(place))));
        assert_eq!(wants.end_trial(None), None, "once");
        let mut wants = Wants { pass: Some(3), ..Wants::default() };
        assert_eq!(wants.pass, Some(3));
        wants.end_pass();
        assert_eq!(wants.pass, None, "a pass that ended is wanted no more");
    }
}
