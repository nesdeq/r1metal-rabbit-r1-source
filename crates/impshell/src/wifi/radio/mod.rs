mod connect;
mod driver;
mod search;

use crate::cell::{self, Cellular};
use crate::crypto;
use crate::log;
use crate::storage::settings::Networks;
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake};
use crate::wifi::association::Association;
use crate::wifi::iface::NAME;
use crate::wifi::{Preempt, address};
use r1_funcore::cell::link::{Routed, to_unrouted_text};
use r1_funcore::cell::modem;
use r1_funcore::net::dhcp::Leases;
use r1_funcore::net::dns::nat64::Search;
use r1_funcore::openai::fault::Fault;
use r1_funcore::wifi::credential::Cache;
use r1_funcore::wifi::join::{Purpose, StayEnd};
use r1_funcore::wifi::known::Change;
use r1_funcore::wifi::netlink::NO_QUALITY_LINE;
use r1_funcore::wifi::radio::{Report, Request, Rounds, Shut, Step, UNWATCHED, Way};
use r1_funcore::wifi::search::{hear_known, survey};
use r1_funcore::wifi::to_wifi_line;
use r1_funcore::wifi::wmt::Setup;
use search::Scans;
use std::fs::File;
use std::io;
use std::net::IpAddr;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REQUEST: u64 = 0;
const LINK_EVENTS: u64 = 1;
const LINK_FRAMES: u64 = 2;

pub(super) struct Grant {
    stay: u64,
    down: NetworkDown,
}

pub(super) type State = r1_funcore::wifi::radio::State<Grant>;

/// Runs in a holder's thread beside every hold, given the hold and whether it is quiet.
pub type HoldHook = Arc<dyn Fn(&Hold, bool) + Send + Sync>;

struct Shared {
    board: Board<State>,
    nudge: Wake,
}

impl Shared {
    fn preempt(&self, urgent_count: usize) -> Preempt<'_> {
        Preempt { nudge: &self.nudge, board: &self.board, urgent_count }
    }
}

fn leave(association: Association, is_lost: bool) {
    if !is_lost {
        association.leave();
    }
    address::clear_address();
}

// What a stay is in a network by: an association of the WiFi core, or the modem's data connection, as it is or routed through the router's forwarder.
enum Link {
    Wifi(Box<Association>),
    Cell(cell::Link),
    Routed(cell::Link),
}

struct Stay {
    link: Link,
    down: NetworkDown,
    number: u64,
}

impl Stay {
    fn grant(&self, shared: &Shared, number: u64) {
        let grant = Grant { stay: self.number, down: self.down.clone() };
        shared.board.lock().answer(number, Ok(grant));
        shared.board.notify();
    }

    // The link's loss, where the news its descriptors brought, or a scan read meanwhile, tells of one.
    fn read_loss(&mut self, ready: &[u64]) -> Option<String> {
        match &mut self.link {
            Link::Wifi(association) => {
                association.serve(ready.contains(&LINK_FRAMES), ready.contains(&LINK_EVENTS));
                association.take_loss()
            }
            Link::Cell(link) | Link::Routed(link) => {
                (ready.contains(&LINK_EVENTS) && link.is_lost()).then(|| String::from(modem::LOST_LINE))
            }
        }
    }

    fn association(&mut self) -> Option<&mut Association> {
        match &mut self.link {
            Link::Wifi(association) => Some(association.as_mut()),
            Link::Cell(_) | Link::Routed(_) => None,
        }
    }

    fn end(self, epoll: &mut Epoll, cellular: &Cellular, is_lost: bool) {
        self.down.end.raise();
        match self.link {
            Link::Wifi(association) => {
                let (events, frames) = association.fds();
                if let Err(e) = epoll.remove(events).and(epoll.remove(frames)) {
                    log!("wifi: ending the link's watch: {e}");
                }
                leave(*association, is_lost);
            }
            Link::Cell(link) => release(epoll, cellular, &link),
            Link::Routed(link) => release_routed(epoll, cellular, &link),
        }
    }
}

// Takes the device's own packets off the router's way, then lets go of the modem's data connection.
fn release_routed(epoll: &mut Epoll, cellular: &Cellular, link: &cell::Link) {
    if let Err(e) = link.unroute() {
        log!("cell: taking the device's own packets off the router's way: {e}");
    }
    release(epoll, cellular, link);
}

// Lets go of the modem's data connection and of its watch.
fn release(epoll: &mut Epoll, cellular: &Cellular, link: &cell::Link) {
    if let Err(e) = epoll.remove(link.fd()) {
        log!("cell: ending the connection's watch: {e}");
    }
    cellular.release();
}

fn refuse(shared: &Shared, waiting: Vec<u64>, reason: &str) {
    let mut state = shared.board.lock();
    waiting.into_iter().for_each(|number| state.answer(number, Err(reason.to_string())));
    drop(state);
    shared.board.notify();
}

struct Worker<'a> {
    shared: &'a Shared,
    networks: &'a Networks,
    cellular: &'a Cellular,
    epoll: Epoll,
    setup: Setup,
    driver: Option<File>,
    rounds: Rounds,
    stay: Option<Stay>,
    leases: Leases,
    keys: Option<Cache>,
}

impl<'a> Worker<'a> {
    fn new(shared: &'a Shared, networks: &'a Networks, cellular: &'a Cellular) -> io::Result<Worker<'a>> {
        let mut epoll = Epoll::new()?;
        epoll.add(shared.nudge.fd(), REQUEST)?;
        Ok(Worker {
            shared,
            networks,
            cellular,
            epoll,
            setup: Setup::Unconfigured,
            driver: None,
            rounds: Rounds::default(),
            stay: None,
            leases: Leases::default(),
            keys: None,
        })
    }

    fn scans(&mut self) -> Scans<'_> {
        let link = self.stay.as_mut().and_then(Stay::association);
        Scans { shared: self.shared, networks: self.networks, urgent_count: self.rounds.urgent_count(), link }
    }

    fn serve(&mut self, ready: &[u64]) {
        let requests = self.shared.board.lock().take_requests();
        self.rounds.begin(requests);
        self.read_stay(ready);
        let (known, places) = (self.networks.known(), self.networks.places());
        let now = Duration::from_nanos(sys::boottime_ns());
        self.rounds.set_ways(self.cellular.is_usable(), &known, &places, now);
        self.run_round();
    }

    // Ends the stay and takes the core's power, as the thread can wait no more.
    fn end(&mut self) {
        self.rounds.end();
        self.run_round();
    }

    fn run_round(&mut self) {
        loop {
            match self.rounds.next_step() {
                Step::Refuse(waiting, reason) => refuse(self.shared, waiting, &reason),
                Step::Grant(number) => self.stay.iter().for_each(|stay| stay.grant(self.shared, number)),
                Step::Quality => self.log_quality(),
                Step::EndStay => {
                    let cellular = self.cellular;
                    self.stay.take().into_iter().for_each(|stay| stay.end(&mut self.epoll, cellular, false));
                }
                Step::LowBand => search::prefer_low_band(&mut self.scans()),
                Step::PowerUp => self.power_up(),
                Step::PowerDown => self.driver.take().into_iter().for_each(driver::power_down),
                Step::Survey => self.listen(),
                Step::Prove(change) => self.prove(&change),
                Step::Join => self.join(),
                Step::CellJoin => self.join_cell(),
                Step::Look => self.look(),
                Step::Tell(scan, trial) => self.report(|state| state.tell(scan, trial)),
                Step::End(end) => return self.report(|state| state.end_round(end)),
            }
        }
    }

    fn read_stay(&mut self, ready: &[u64]) {
        if let Some(line) = self.stay.as_mut().and_then(|stay| stay.read_loss(ready)) {
            log!("{line}");
            self.rounds.lose_stay();
            let cellular = self.cellular;
            self.stay.take().into_iter().for_each(|stay| stay.end(&mut self.epoll, cellular, true));
        }
    }

    fn listen(&mut self) {
        let outcome = survey(&mut self.scans());
        self.rounds.end_survey(outcome);
        // A loss of a WiFi stay the survey's scans read ends the stay now, as one read at the round's start does.
        self.read_stay(&[]);
    }

    fn power_up(&mut self) {
        // The radio counts as on while it comes up; the random generator starts beside the drivers' first start, for the joins and connections that draw on it.
        self.shared.board.lock().note_powering();
        if self.setup.is_unconfigured() {
            crypto::warm_up();
        }
        self.driver = driver::power_up(&mut self.setup);
        self.rounds.set_powered(self.driver.is_some());
    }

    fn prove(&mut self, change: &Change) {
        let look = (self.rounds.urgent_count(), self.rounds.is_look_narrow(Purpose::Trial));
        let limits = self.rounds.join_limits(Purpose::Trial);
        let end =
            connect::run_trial(self.shared, self.networks, change, look, limits, &mut self.leases, &mut self.keys);
        self.rounds.end_trial(Some(end));
    }

    // Hears whether a known network a stay may join is in reach, now that a stay over the modem has ended.
    fn look(&mut self) {
        let known = self.rounds.to_joinable(self.networks.known());
        let places = self.networks.places();
        let word = hear_known(&mut self.scans(), &known, &places);
        self.rounds.end_look(word).into_iter().for_each(|line| log!("{line}"));
    }

    fn join(&mut self) {
        let look = (self.rounds.urgent_count(), self.rounds.is_look_narrow(Purpose::Stay));
        let known = self.rounds.to_joinable(self.networks.known());
        let limits = self.rounds.join_limits(Purpose::Stay);
        match connect::join(self.shared, self.networks, &known, look, limits, &mut self.leases, &mut self.keys) {
            Ok(association) => {
                let (events, frames) = association.fds();
                match self
                    .epoll
                    .add(events, LINK_EVENTS)
                    .and_then(|()| self.epoll.add(frames, LINK_FRAMES))
                    .and_then(|()| Wake::new())
                {
                    Ok(down) => {
                        let number = self.rounds.begin_stay(Way::Wifi);
                        let down = NetworkDown::new(down, number, NAME.to_string_lossy().into_owned(), None, None);
                        self.stay = Some(Stay { link: Link::Wifi(association), down, number });
                    }
                    Err(e) => {
                        log!("wifi: watching the link: {e}");
                        leave(*association, false);
                        self.rounds.fail_join(UNWATCHED.into());
                    }
                }
            }
            Err(StayEnd::GaveWay) => {}
            Err(StayEnd::Failed(reason, culprit)) => {
                log!("{}", to_wifi_line(&reason));
                let passed = culprit.and_then(|network| self.rounds.pass_over(network));
                passed.into_iter().for_each(|line| log!("{line}"));
                self.rounds.fail_join(reason);
            }
        }
    }

    fn log_quality(&mut self) {
        let Some(association) = self.stay.as_mut().and_then(Stay::association) else { return };
        match association.quality() {
            Ok(Some(quality)) => log!("{}", quality.to_log_line()),
            Ok(None) => log!("{NO_QUALITY_LINE}"),
            Err(e) => log!("wifi: asking how the link does: {e}"),
        }
    }

    fn join_cell(&mut self) {
        let preempt = self.shared.preempt(self.rounds.urgent_count());
        let answer = match self.cellular.join(self.shared.nudge.fd(), || preempt.should_yield()) {
            Ok(Some(answer)) => answer,
            Ok(None) => return,
            Err(e) => {
                log!("cell: asking the modem for its connection: {e}");
                Err(e.to_string())
            }
        };
        let link = match answer {
            Ok(link) => link,
            Err(reason) => return self.rounds.fail_join(reason),
        };
        let end = match self.epoll.add(link.fd(), LINK_EVENTS).and_then(|()| Wake::new()) {
            Ok(end) => end,
            Err(e) => {
                log!("cell: watching the connection: {e}");
                release(&mut self.epoll, self.cellular, &link);
                return self.rounds.fail_join(UNWATCHED.into());
            }
        };
        let (interface, resolvers) = (String::from(link.interface()), link.nat64().map(<[IpAddr]>::to_vec));
        if !self.rounds.is_routed() {
            let number = self.rounds.begin_stay(Way::Cell);
            let down = NetworkDown::new(end, number, interface, resolvers, None);
            self.stay = Some(Stay { link: Link::Cell(link), down, number });
            return;
        }
        match link.route() {
            Ok(routed) => {
                let number = self.rounds.begin_stay(Way::Cell);
                let down = NetworkDown::new(end, number, interface, resolvers, Some(routed));
                self.stay = Some(Stay { link: Link::Routed(link), down, number });
            }
            Err(e) => {
                // What the routing changed before it failed is put back, so that a stay made anew finds the interfaces as the modem's thread left them.
                release_routed(&mut self.epoll, self.cellular, &link);
                self.rounds.fail_join(to_unrouted_text(&e.to_string()));
            }
        }
    }

    // Hands the main loop what the round came to.
    fn report(&self, take: impl FnOnce(&mut State)) {
        take(&mut self.shared.board.lock());
        self.shared.board.raise();
    }
}

fn work(shared: &Shared, networks: &Networks, cellular: &Cellular) {
    let mut worker = match Worker::new(shared, networks, cellular) {
        Ok(worker) => worker,
        Err(e) => return log!("radio: the thread cannot wait for wishes: {e}"),
    };
    loop {
        let wait = shared.board.lock().next_wait();
        let ready = match worker.epoll.wait(wait) {
            Ok(ready) => ready,
            Err(e) => {
                log!("radio: waiting: {e}");
                return worker.end();
            }
        };
        shared.nudge.clear();
        worker.serve(&ready);
    }
}

#[derive(Clone)]
struct Asker {
    shared: Arc<Shared>,
    hook: HoldHook,
}

impl Asker {
    fn ask(&self, request: Request<u64>) {
        self.shared.board.lock().ask(request);
        self.shared.nudge.raise();
    }

    fn tell(&self, wish: impl FnOnce(&mut State) -> bool) {
        if wish(&mut self.shared.board.lock()) {
            self.shared.nudge.raise();
        }
    }
}

/// Proof that the radio's rest is lifted, for a part that goes on while the device sleeps.
pub struct RadioGrant(());

/// The main loop's handle of the radio's thread.
pub struct Radio {
    asker: Asker,
}

impl Radio {
    /// Starts the thread, which starts the drivers on first use and joins the default of `networks`, else asks the modem for its connection; `hook` runs beside every hold.
    pub fn start(networks: Networks, hook: HoldHook, cellular: Cellular) -> io::Result<Radio> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, nudge: Wake::new()? });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name("radio".into()).spawn(move || {
            work(&worker, &networks, &cellular);
            worker.board.lock().end_thread();
            worker.board.notify();
            worker.board.raise();
        })?;
        Ok(Radio { asker: Asker { shared, hook } })
    }

    /// Readable once the thread's report may have news: a round ended, a pass or a trial was told, or the thread ended.
    pub fn fd(&self) -> RawFd {
        self.asker.shared.board.fd()
    }

    /// Asks for the core by the console's ask of that number, or for none; the thread follows, and a core that did not come on stays off until the next ask.
    pub fn want(&self, ask: Option<u32>) {
        self.asker.tell(|state| state.want_power(ask));
    }

    /// Asks for a network by the console's ask of that number, or for none; a failed or lost link stays off until the next ask.
    pub fn want_link(&self, ask: Option<u32>) {
        self.asker.tell(|state| state.want_link(ask));
    }

    /// Sets whether every stay is to be in the modem's network alone, WiFi passed over.
    pub fn want_cell(&self, is_wanted: bool) {
        self.asker.tell(|state| state.want_cell(is_wanted));
    }

    /// Sets whether every stay is to be over the modem and routed through the router's forwarder, which a host on the cable asks.
    pub fn want_route(&self, is_wanted: bool) {
        self.asker.tell(|state| state.want_route(is_wanted));
    }

    /// Sets whether the device sleeps, in no network and with holds refused; a lifted rest returns the grant.
    pub fn set_resting(&self, is_resting: bool) -> Option<RadioGrant> {
        self.asker.tell(|state| state.set_resting(is_resting));
        (!is_resting).then_some(RadioGrant(()))
    }

    /// Ends every use of the radio and waits until it has left its network and has no power, or until `deadline`.
    pub fn shut(&self, deadline: Instant) -> io::Result<()> {
        const BOARD: u64 = 0;
        self.want(None);
        self.want_scan(None);
        self.want_trial(None);
        self.set_resting(true);
        let mut epoll = Epoll::new()?;
        epoll.add(self.fd(), BOARD)?;
        loop {
            let limit = deadline.saturating_duration_since(Instant::now());
            let step = self.asker.shared.board.lock().next_shut(limit);
            match step {
                Shut::Off => return Ok(()),
                Shut::Late(words) => return Err(io::Error::new(io::ErrorKind::TimedOut, words)),
                Shut::Wait(limit) => {
                    epoll.wait(Some(limit))?;
                    self.asker.shared.board.clear();
                }
            }
        }
    }

    /// The network handle for threads that need it.
    pub fn online(&self) -> Online {
        Online(self.asker.clone())
    }

    /// Asks for a scan pass by number, once per pass, or for none; `latest` hands over the result.
    pub fn want_scan(&self, pass: Option<u32>) {
        self.asker.tell(|state| state.want_pass(pass));
    }

    /// Asks for a trial join by number, with the change that waits for it, or for none; `latest` hands over the result.
    pub fn want_trial(&self, trial: Option<(u32, &Change)>) {
        self.asker.tell(|state| state.want_trial(trial));
    }

    /// Takes the thread's report; an ended pass or trial is asked again if still wanted.
    pub fn latest(&self) -> Report {
        self.asker.shared.board.take(State::take_report)
    }

    /// True while the core has no power and no wish is on its way, and once the thread has ended.
    pub fn is_off(&self) -> bool {
        self.asker.shared.board.lock().is_off()
    }
}

/// The network handle: the device joins when the first asks and leaves when the last has done.
#[derive(Clone)]
pub struct Online(Asker);

impl Online {
    /// Waits until the device is in a network and holds it there until dropped; or says why it is in none.
    pub fn hold(&self) -> Result<Hold, Fault> {
        self.request_hold(false)
    }

    /// `hold` for a holder that asks for the time itself or had it asked in an earlier stay.
    pub fn hold_quietly(&self) -> Result<Hold, Fault> {
        self.request_hold(true)
    }

    fn request_hold(&self, is_quiet: bool) -> Result<Hold, Fault> {
        let board = &self.0.shared.board;
        let number = board.lock().ask_hold();
        self.0.shared.nudge.raise();
        match board.wait(|state| state.take_answer(number)) {
            Ok(grant) => {
                let hold = Hold(Arc::new(Held { asker: self.0.clone(), stay: grant.stay, down: grant.down }));
                (self.0.hook)(&hold, is_quiet);
                Ok(hold)
            }
            Err(reason) => Err(Fault::Offline(reason)),
        }
    }

    /// `hold` into `slot`, keeping a live hold and dropping an ended one; returns the stay's end.
    pub fn keep(&self, slot: &mut Option<Hold>) -> Result<NetworkDown, Fault> {
        drop(slot.take_if(|hold| hold.down().is_raised()));
        let hold = match slot {
            Some(hold) => hold,
            None => slot.insert(self.hold()?),
        };
        Ok(hold.down().clone())
    }
}

/// The device's stay in a network, held while this or a share of it lives.
pub struct Hold(Arc<Held>);

// What the radio counts as one holder of a stay, however many jobs share it.
struct Held {
    asker: Asker,
    stay: u64,
    down: NetworkDown,
}

impl Hold {
    /// The stay's end; connections made during it lead nowhere after it.
    pub fn down(&self) -> &NetworkDown {
        &self.0.down
    }

    /// The hold for a job of another thread beside its holder's: the stay stands until both have let go.
    pub fn share(&self) -> Hold {
        Hold(Arc::clone(&self.0))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.asker.ask(Request::Release(self.stay));
    }
}

/// A stay's end, which holders and connections wait on and the radio's thread alone raises, with what the stay's connections need of it: its number, its interface, in a network of IPv6 alone the search for its NAT64, and of a routed stay what the router's forwarder took over.
#[derive(Clone)]
pub struct NetworkDown {
    end: Arc<Wake>,
    stay: u64,
    interface: Arc<str>,
    nat64: Option<Arc<Mutex<Search<Arc<Wake>>>>>,
    routed: Option<Routed>,
}

impl NetworkDown {
    // The end of the stay of that number on the interface of that name, in a network of IPv6 alone where the resolvers that tell its NAT64 are given, routed where what was taken off the kernel is.
    fn new(
        end: Wake,
        stay: u64,
        interface: String,
        resolvers: Option<Vec<IpAddr>>,
        routed: Option<Routed>,
    ) -> NetworkDown {
        let nat64 = resolvers.map(|resolvers| Arc::new(Mutex::new(Search::new(resolvers))));
        NetworkDown { end: Arc::new(end), stay, interface: interface.into(), nat64, routed }
    }

    /// What a routed stay took off the modem's interface for the router's forwarder; none of a stay that is not routed.
    pub fn routed(&self) -> Option<Routed> {
        self.routed
    }

    /// Readable once the stay has ended.
    pub fn fd(&self) -> RawFd {
        self.end.fd()
    }

    /// True once the stay has ended; does not wait.
    pub fn is_raised(&self) -> bool {
        self.end.is_raised()
    }

    /// The number of the stay.
    pub fn stay(&self) -> u64 {
        self.stay
    }

    /// The name of the interface the stay is on.
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// The search for the NAT64's prefix of a stay in a network of IPv6 alone, which its connections share; none in a network of IPv4.
    pub fn nat64(&self) -> Option<&Mutex<Search<Arc<Wake>>>> {
        self.nat64.as_deref()
    }
}

impl PartialEq for NetworkDown {
    fn eq(&self, other: &NetworkDown) -> bool {
        Arc::ptr_eq(&self.end, &other.end)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::storage::settings::Keeper;
    use r1_funcore::wifi::known::{self, TrialError, UNKNOWN};
    use r1_funcore::wifi::radio::ASLEEP;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    // The numbers of the tests' stays, each its own.
    static STAYS: AtomicU64 = AtomicU64::new(1);
    // The tests' stays are on the loopback interface.
    const LOOPBACK: &str = "lo";

    pub(crate) fn without_networks() -> Radio {
        let networks = Keeper::start(Data::unusable(), None).unwrap().networks();
        let modem = crate::cell::Modem::start(Data::unusable()).unwrap();
        Radio::start(networks, Arc::new(|_, _| {}), modem.cellular()).unwrap()
    }

    pub(crate) fn up() -> NetworkDown {
        NetworkDown::new(Wake::new().unwrap(), STAYS.fetch_add(1, Ordering::Relaxed), LOOPBACK.into(), None, None)
    }

    // A stay in a network of IPv6 alone, whose resolvers are those given.
    pub(crate) fn up_ipv6_alone(resolvers: Vec<IpAddr>) -> NetworkDown {
        let stay = STAYS.fetch_add(1, Ordering::Relaxed);
        NetworkDown::new(Wake::new().unwrap(), stay, LOOPBACK.into(), Some(resolvers), None)
    }

    impl NetworkDown {
        pub(crate) fn raise(&self) {
            self.end.raise();
        }
    }

    pub(crate) fn hold(online: &Online) -> Hold {
        Hold(Arc::new(Held { asker: online.0.clone(), stay: 9, down: up() }))
    }

    fn radio_and_epoll() -> (Radio, Epoll) {
        let radio = without_networks();
        let epoll = Epoll::with_fd(radio.fd());
        (radio, epoll)
    }

    fn run_until_off(radio: &Radio, epoll: &mut Epoll, mut check: impl FnMut()) {
        while !radio.is_off() {
            assert_eq!(epoll.wait(None).unwrap(), vec![7], "the thread tells when it has seen wishes through");
            radio.asker.shared.board.clear();
            check();
        }
        check();
    }

    #[test]
    fn off_until_wanted() {
        let (radio, mut epoll) = radio_and_epoll();
        assert!(radio.is_off());
        radio.want(None);
        assert!(radio.is_off(), "nothing is asked of the thread for a wish it already has");
        radio.want(Some(1));
        radio.want(None);
        radio.want(Some(2));
        assert!(!radio.is_off(), "wanted counts as not off from the moment of the wish");
        run_until_off(&radio, &mut epoll, || {});
    }

    #[test]
    fn shut_by_deadline() {
        let radio = without_networks();
        radio.want(Some(1));
        assert!(
            radio.shut(Instant::now() + r1_funcore::hw::power::SHUTDOWN_TIMEOUT).is_ok() && radio.is_off(),
            "here without the driver's nodes"
        );
        radio.asker.shared.board.lock().note_powering();
        let start = Instant::now();
        let result = radio.shut(start + Duration::from_millis(20));
        assert!(
            result.is_err_and(|e| e.kind() == io::ErrorKind::TimedOut)
                && start.elapsed() < r1_funcore::hw::power::SHUTDOWN_TIMEOUT,
            "a core that never goes off is waited for until the time given and no longer"
        );
    }

    #[test]
    fn failed_pass() {
        let (radio, mut epoll) = radio_and_epoll();
        assert!(radio.latest().scan.is_none());
        radio.want_scan(Some(3));
        radio.want_scan(None);
        radio.want_scan(Some(4));
        assert!(!radio.is_off(), "a pass that is asked for counts as not off");
        let mut ends = Vec::new();
        run_until_off(&radio, &mut epoll, || {
            ends.extend(radio.latest().scan.map(|scan| (scan.pass, scan.networks.is_none())))
        });
        assert_eq!(
            ends.last(),
            Some(&(4, true)),
            "the pass that is wanted last is the one that is answered, here without the driver's nodes"
        );
        assert!(radio.latest().scan.is_none(), "once");
    }

    #[test]
    fn failed_trial() {
        let (radio, mut epoll) = radio_and_epoll();
        assert!(radio.latest().trial.is_none());
        let change = Change::Remember(known::Network { name: "home".into(), password: "secret12".into() });
        radio.want_trial(Some((3, &change)));
        radio.want_trial(None);
        radio.want_trial(Some((4, &change)));
        assert!(!radio.is_off(), "a join that is asked for counts as not off");
        let mut ends = Vec::new();
        run_until_off(&radio, &mut epoll, || {
            ends.extend(radio.latest().trial.map(|trial| (trial.number, trial.outcome)))
        });
        assert_eq!(
            ends.last(),
            Some(&(4, Err(TrialError::NoRadio))),
            "the join that is wanted last is the one that is answered, here without the driver's nodes"
        );
        assert!(radio.latest().trial.is_none(), "once");
    }

    #[test]
    fn hold_refusal() {
        let (radio, mut epoll) = radio_and_epoll();
        let online = radio.online();
        let askers: Vec<_> = (0..3)
            .map(|_| {
                thread::spawn({
                    let online = online.clone();
                    move || online.hold().err()
                })
            })
            .collect();
        for asker in askers {
            assert_eq!(asker.join().unwrap(), Some(Fault::Offline(UNKNOWN.into())), "here no network is known");
        }
        radio.set_resting(true);
        assert_eq!(online.hold().err(), Some(Fault::Offline(ASLEEP.into())), "asleep the radio is not even tried");
        radio.set_resting(false);
        assert_eq!(online.hold().err(), Some(Fault::Offline(UNKNOWN.into())), "nor awake for a network nobody knows");
        run_until_off(&radio, &mut epoll, || {});
    }

    #[test]
    fn shared_hold_let_go_once() {
        let shared = Arc::new(Shared { board: Board::new(State::default()).unwrap(), nudge: Wake::new().unwrap() });
        let asker = Asker { shared: Arc::clone(&shared), hook: Arc::new(|_, _| {}) };
        let first = Hold(Arc::new(Held { asker, stay: 9, down: up() }));
        let share = first.share();
        drop(first);
        assert!(shared.board.lock().take_requests().is_empty(), "a share holds the stay on");
        assert!(!shared.nudge.is_raised(), "and the thread is told nothing");
        drop(share);
        assert_eq!(shared.board.lock().take_requests(), [Request::Release(9)], "the last to let go releases it, once");
        assert!(shared.nudge.is_raised());
    }

    #[test]
    fn keep_ended_hold() {
        let (radio, mut epoll) = radio_and_epoll();
        let online = radio.online();
        let down = up();
        let mut slot = Some(Hold(Arc::new(Held { asker: online.0.clone(), stay: 9, down: down.clone() })));
        assert!(online.keep(&mut slot).is_ok_and(|stay_end| stay_end == down), "a stay that stands serves again");
        assert!(radio.is_off() && slot.is_some(), "and the thread is asked nothing");
        down.raise();
        assert_eq!(
            online.keep(&mut slot).err(),
            Some(Fault::Offline(UNKNOWN.into())),
            "a stay that has ended is held anew, here with no network known"
        );
        assert!(slot.is_none(), "and its hold let go of");
        run_until_off(&radio, &mut epoll, || {});
    }
}
