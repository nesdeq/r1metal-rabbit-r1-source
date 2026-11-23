mod ccci;
mod link;
mod seed;

use crate::log;
use crate::power::alarm::Alarm;
use crate::storage::{Data, nvram};
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake, klog};
use r1_funcore::cell::at::{Command, Lines};
use r1_funcore::cell::attaches::{self, Attaches};
use r1_funcore::cell::ccci::{Answer, BOOT_WORDS, SBP_FILES, answer_message, is_enabled, parse_event};
use r1_funcore::cell::files::{self, Service};
use r1_funcore::cell::link::Routed;
use r1_funcore::cell::modem::{self, Modem as Machine, NO_CHANNEL, NO_MODEM, NOT_STOPPED, Report, Step};
use r1_funcore::cell::sim::{FILE, Pin, Sim};
use r1_funcore::cell::stream::{PACKET_BYTES, Request, Slots, to_drop_line};
use r1_funcore::cell::{DIRECTORY, rpc};
use r1_funcore::domain::device::NVRAM;
use r1_funcore::hw::power::alarm::Reason;
use r1_funcore::storage::NOT_MOUNTED;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const NUDGE: u64 = 0;
const STATUS: u64 = 1;
const FILES: u64 = 2;
const AT: u64 = 3;
const ALARM: u64 = 4;
const ANSWERED: u64 = 0;
const PREEMPT: u64 = 1;
// The kernel starts the modem only for a thread of this name (`ccci_fsm_ioctl.c`).
const THREAD: &str = "ccci_mdinit";
const MONITOR_THREAD: &str = "modem monitor";
const RPC_THREAD: &str = "modem rpc";

/// A data connection the modem granted; readable once it is lost.
pub struct Link {
    lost: Arc<Wake>,
    interface: String,
    nat64: Option<Vec<IpAddr>>,
}

impl Link {
    pub fn fd(&self) -> RawFd {
        self.lost.fd()
    }

    /// True once the connection is lost; does not wait.
    pub fn is_lost(&self) -> bool {
        self.lost.is_raised()
    }

    /// The name of the network interface the connection runs on.
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// The network's DNS servers, which tell its NAT64's prefix, where the connection is IPv6 alone.
    pub fn nat64(&self) -> Option<&[IpAddr]> {
        self.nat64.as_deref()
    }

    /// Hands the connection's IPv4 over to the router's forwarder, and tells what was taken off the kernel for it.
    pub fn route(&self) -> io::Result<Routed> {
        link::route(&self.interface)
    }

    /// Takes the device's own stack off the forwarder's way again, as the routed stay ends.
    pub fn unroute(&self) -> io::Result<()> {
        link::unroute(&self.interface)
    }
}

type State = modem::State<Link>;

struct Shared {
    board: Board<State>,
    nudge: Wake,
    answered: Wake,
}

impl Shared {
    fn is_usable(&self) -> bool {
        self.board.lock().is_usable(Duration::from_nanos(sys::boottime_ns()))
    }
}

fn keep(data: &Data, name: &str, text: &str) {
    if let Err(e) = data.put(DIRECTORY, name, text.as_bytes()) {
        log!("cell: keeping {name}: {e}");
    }
}

fn read_text(data: &Data, name: &str) -> String {
    data.text(DIRECTORY, name).inspect_err(|e| log!("cell: reading {name}: {e}")).ok().flatten().unwrap_or_default()
}

// Each of the SBP code's files in `nvdata`: its bytes, none where it is not there, or why it was not read.
fn read_sbp() -> [Result<Option<Vec<u8>>, String>; 2] {
    let mut partition = nvram::files(NVRAM).map_err(|e| format!("{NVRAM}: {e}"));
    SBP_FILES.map(|path| {
        let files = partition.as_mut().map_err(|e| e.clone())?;
        match files.read(path) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    })
}

fn watch_monitor(monitor: &File) {
    loop {
        let record = match ccci::read_message(monitor) {
            Ok(record) => record,
            Err(e) => return log!("cell: reading the kernel's monitor: {e}"),
        };
        match answer_message(&record) {
            Some(Answer::Battery) => {
                if let Err(e) = ccci::send_battery(monitor) {
                    log!("cell: handing the modem the battery's voltage: {e}");
                }
            }
            Some(Answer::Log(line)) => log!("{line}"),
            None => {}
        }
    }
}

// Reads a packet of a port of the file service or RPC: the request once it is whole.
fn read_request(port: &mut File, slots: &mut Slots, channel: u32, capacity: usize) -> io::Result<Option<Request>> {
    let mut packet = [0u8; PACKET_BYTES];
    let count = port.read(&mut packet)?;
    match slots.take(&packet[..count], channel, capacity) {
        Ok(request) => Ok(request),
        Err(flaw) => {
            log!("{}", to_drop_line(channel, flaw));
            Ok(None)
        }
    }
}

fn reply(port: &mut File, request: &Request, params: &[Vec<u8>]) -> io::Result<()> {
    let params: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
    request.reply(&params).iter().try_for_each(|fragment| port.write_all(fragment))
}

// Answers the modem's RPC requests for the app's life; the kernel's RPC node has no poll (`rpc_dev_fops` in `port_rpc.c`), so its reads block on this thread alone, as MediaTek's `ccci_rpcd` reads it.
fn serve_rpc(mut port: File) {
    let mut slots = Slots::default();
    loop {
        let request = match read_request(&mut port, &mut slots, rpc::CHANNEL, rpc::CAPACITY) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(e) => return log!("cell: reading {}: {e}", ccci::RPC),
        };
        let (params, line) = rpc::answer(&request);
        log!("{line}");
        if let Err(e) = reply(&mut port, &request, &params) {
            log!("cell: answering on {}: {e}", ccci::RPC);
        }
    }
}

// The RPC node, held for the app's life, and the thread that answers it.
fn open_rpc() {
    let port = match ccci::open_port(ccci::RPC) {
        Ok(port) => port,
        Err(e) => return log!("cell: {}: {e}", ccci::RPC),
    };
    if let Err(e) = thread::Builder::new().name(RPC_THREAD.into()).spawn(move || serve_rpc(port)) {
        log!("cell: the RPC thread: {e}");
    }
}

struct Port {
    file: File,
    path: &'static str,
}

struct Worker<'a> {
    shared: &'a Shared,
    data: &'a Data,
    epoll: Epoll,
    monitor: Option<Arc<File>>,
    status: Option<File>,
    files: Option<Port>,
    at: Option<Port>,
    // Ends the attach kept after a stay, a sleeping device woken for it.
    alarm: Alarm,
    file_slots: Slots,
    lines: Lines,
    service: Service,
    lost: Option<Arc<Wake>>,
}

impl Worker<'_> {
    fn open(&mut self, path: &'static str, token: u64) -> io::Result<Port> {
        let file = ccci::open_port(path)?;
        self.epoll.add(file.as_raw_fd(), token)?;
        Ok(Port { file, path })
    }

    fn close(&mut self, port: Option<Port>) {
        if let Some(port) = port
            && let Err(e) = self.epoll.remove(port.file.as_raw_fd())
        {
            log!("cell: ending the watch of {}: {e}", port.path);
        }
    }

    fn monitor(&self) -> io::Result<&File> {
        self.monitor.as_deref().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, NO_MODEM))
    }

    fn start(&mut self, words: &[u32; BOOT_WORDS]) -> io::Result<()> {
        let files = self.files.take();
        self.close(files);
        ccci::init_ccb()?;
        ccci::set_boot_data(self.monitor()?, words)?;
        self.files = Some(self.open(ccci::FILES, FILES)?);
        ccci::start(self.monitor()?)
    }

    fn unload(&mut self) {
        if let Some(line) = self.service.close_all(self.data) {
            log!("{line}");
        }
        let files = self.files.take();
        self.close(files);
        self.file_slots = Slots::default();
    }

    fn open_at(&mut self) -> io::Result<()> {
        self.lines = Lines::default();
        self.at = Some(self.open(ccci::AT, AT)?);
        Ok(())
    }

    fn close_at(&mut self) {
        let at = self.at.take();
        self.close(at);
    }

    fn send(&mut self, command: &Command) -> io::Result<()> {
        let port = self.at.as_mut().ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, NO_CHANNEL))?;
        port.file.write_all(command.to_wire().as_bytes())
    }

    fn answer(&self, number: u64, answer: Result<Link, String>) {
        self.shared.board.lock().answer(number, answer);
        self.shared.answered.raise();
    }

    // Hands the radio its link on the `ccmni` device of that index; why it could not, which the state machine takes.
    fn grant(&mut self, number: u64, index: u32, nat64: Option<Vec<IpAddr>>) -> Result<(), String> {
        let lost = match Wake::new() {
            Ok(lost) => Arc::new(lost),
            Err(e) => {
                let reason = format!("watching the connection: {e}");
                self.answer(number, Err(reason.clone()));
                return Err(reason);
            }
        };
        self.lost = Some(Arc::clone(&lost));
        self.answer(number, Ok(Link { lost, interface: link::to_name(index), nat64 }));
        Ok(())
    }

    // Sets the alarm to that second of UTC, or clears it; clocks set meanwhile have the state machine set it again.
    fn arm(&self, machine: &mut Machine, at: Option<u64>) {
        match self.alarm.arm(at) {
            Ok(true) => {}
            Ok(false) => {
                self.alarm.acknowledge();
                machine.arm_again();
            }
            Err(e) => log!("cell: the alarm that ends the kept attach: {e}"),
        }
    }

    fn serve_files(&mut self) -> io::Result<()> {
        let Some(port) = &mut self.files else {
            return Ok(());
        };
        let named = |effect: &str, e: io::Error| io::Error::new(e.kind(), format!("{effect} {}: {e}", port.path));
        let request = read_request(&mut port.file, &mut self.file_slots, files::CHANNEL, files::CAPACITY);
        if let Some(request) = request.map_err(|e| named("reading", e))? {
            let (params, lines, trace) = self.service.serve(self.data, &request);
            lines.iter().for_each(|line| log!("{line}"));
            klog::write_debug(&trace);
            reply(&mut port.file, &request, &params).map_err(|e| named("answering on", e))?;
        }
        Ok(())
    }

    fn read_lines(&mut self) -> io::Result<Vec<String>> {
        let Some(port) = &mut self.at else {
            return Ok(Vec::new());
        };
        let mut bytes = [0u8; PACKET_BYTES];
        let read = port.file.read(&mut bytes);
        let count = read.map_err(|e| io::Error::new(e.kind(), format!("reading {}: {e}", port.path)))?;
        Ok(self.lines.take(&bytes[..count]))
    }

    fn read_event(&mut self, machine: &mut Machine) -> io::Result<()> {
        let Some(status) = &self.status else {
            return Ok(());
        };
        let record =
            ccci::read_event(status).map_err(|e| io::Error::new(e.kind(), format!("reading {}: {e}", ccci::STATUS)))?;
        if let Some((event, reason)) = parse_event(&record) {
            machine.take_event(event, &reason);
        }
        Ok(())
    }

    // A port whose read or reply failed is watched no more, and the state machine takes the failure as its call named it.
    fn drop_port(&mut self, machine: &mut Machine, token: u64, error: &io::Error) {
        let port = if token == FILES { self.files.take() } else { self.at.take() };
        if port.is_some() {
            machine.take_port_error(&error.to_string());
        }
        self.close(port);
    }

    // The status node, held for the thread's life, ends the thread where a read of it fails: the modem's start and stop are heard nowhere else.
    fn take_ready(&mut self, machine: &mut Machine, token: u64) -> io::Result<()> {
        let (now, utc) = (Duration::from_nanos(sys::boottime_ns()), sys::realtime_seconds());
        let result = match token {
            NUDGE => {
                self.shared.nudge.clear();
                let wishes = self.shared.board.lock().take_wishes();
                machine.take_wishes(wishes, now, utc);
                Ok(())
            }
            STATUS => return self.read_event(machine),
            FILES => self.serve_files(),
            // Its time come, the state machine ends the keep; the clocks set, it sets the alarm again to the same end.
            ALARM => {
                self.alarm.acknowledge();
                machine.arm_again();
                Ok(())
            }
            _ => self.read_lines().map(|lines| lines.iter().for_each(|line| machine.take_line(line, now, utc))),
        };
        if let Err(e) = result {
            self.drop_port(machine, token, &e);
        }
        Ok(())
    }

    fn wait(&mut self, machine: &mut Machine, limit: Option<Duration>) -> io::Result<()> {
        if self.shared.board.lock().note(machine) {
            self.shared.board.raise();
        }
        self.shared.board.notify();
        for token in self.epoll.wait(limit)? {
            self.take_ready(machine, token)?;
        }
        Ok(())
    }

    fn act(&mut self, machine: &mut Machine, step: Step, (now, utc): (Duration, u64)) -> io::Result<()> {
        match step {
            Step::Seed => machine.take_seed(seed::seed(self.data).map_err(|e| e.to_string())),
            Step::ReadSbp => machine.take_sbp(read_sbp()),
            Step::Start(words) => machine.take_start(self.start(&words).map_err(|e| e.to_string())),
            Step::Stop(is_flight) => {
                let result = self.monitor().and_then(|monitor| ccci::stop(monitor, is_flight));
                machine.take_stop(result.map_err(|e| e.to_string()));
            }
            Step::Unload => self.unload(),
            Step::Open => machine.take_open(self.open_at().map_err(|e| e.to_string())),
            Step::Close => self.close_at(),
            Step::Send(command) => {
                machine.take_sent(self.send(&command).map_err(|e| format!("writing to {}: {e}", ccci::AT)));
            }
            Step::Configure(interface) => {
                machine.take_configured(link::configure(&interface).map_err(|e| e.to_string()), now, utc);
            }
            Step::Unconfigure(interface) => {
                if let Err(e) = link::unconfigure(&interface) {
                    log!("cell: taking the connection off {}: {e}", link::to_name(interface.index()));
                }
            }
            Step::Grant(number, index, nat64) => {
                let result = self.grant(number, index, nat64);
                machine.take_grant(result);
            }
            Step::Refuse(number, reason) => self.answer(number, Err(reason)),
            Step::Lose => self.lost.take().iter().for_each(|lost| lost.raise()),
            Step::KeepSim(sim) => keep(self.data, FILE, &sim.to_text()),
            Step::KeepAttaches(attaches) => keep(self.data, attaches::FILE, &attaches.to_text()),
            Step::Arm(at) => self.arm(machine, at),
            Step::Log(line) => log!("{line}"),
            Step::Wait(limit) => self.wait(machine, limit)?,
        }
        Ok(())
    }

    // Runs the state machine's steps; a wait that fails ends the thread once the state machine's end has been carried out: the link lost, the modem stopped, its files written.
    fn run(&mut self, machine: &mut Machine) -> io::Result<()> {
        loop {
            let clocks = (Duration::from_nanos(sys::boottime_ns()), sys::realtime_seconds());
            let step = machine.next(clocks.0, clocks.1);
            let Err(e) = self.act(machine, step, clocks) else {
                continue;
            };
            machine.end(&e.to_string());
            loop {
                match machine.next(clocks.0, clocks.1) {
                    Step::Wait(_) => return Err(e),
                    step => self.act(machine, step, clocks)?,
                }
            }
        }
    }
}

// The monitor of a modem the bootloader loaded, held for the app's life, and the thread that answers it; beside it the RPC node's.
fn open_monitor() -> Option<Arc<File>> {
    let boot = fs::read_to_string(ccci::BOOT_FILE).inspect_err(|e| log!("cell: {}: {e}", ccci::BOOT_FILE)).ok()?;
    if !is_enabled(&boot) {
        return None;
    }
    let monitor = Arc::new(ccci::open_monitor().inspect_err(|e| log!("cell: the kernel's monitor: {e}")).ok()?);
    let watched = Arc::clone(&monitor);
    if let Err(e) = thread::Builder::new().name(MONITOR_THREAD.into()).spawn(move || watch_monitor(&watched)) {
        log!("cell: the monitor's thread: {e}");
    }
    open_rpc();
    Some(monitor)
}

fn work(shared: &Shared, data: &Data) -> io::Result<()> {
    if !data.wait_decided() {
        return Err(io::Error::new(io::ErrorKind::NotFound, NOT_MOUNTED));
    }
    let monitor = open_monitor();
    let status = monitor.as_ref().map(|_| File::open(ccci::STATUS)).transpose()?;
    let sim = Sim::parse(&read_text(data, FILE));
    let attaches = Attaches::parse(&read_text(data, attaches::FILE));
    let mut machine = Machine::new(monitor.is_some(), sim, attaches);
    let mut epoll = Epoll::new()?;
    epoll.add(shared.nudge.fd(), NUDGE)?;
    if let Some(status) = &status {
        epoll.add(status.as_raw_fd(), STATUS)?;
    }
    let alarm = Alarm::new(Reason::KeptAttach)?;
    epoll.add_wakeup(alarm.fd(), ALARM)?;
    let mut worker = Worker {
        shared,
        data,
        epoll,
        monitor,
        status,
        files: None,
        at: None,
        alarm,
        file_slots: Slots::default(),
        lines: Lines::default(),
        service: Service::default(),
        lost: None,
    };
    worker.run(&mut machine)
}

/// The main loop's handle of the modem's thread, which starts the modem while it is wanted and serves its files and AT channel.
pub struct Modem {
    shared: Arc<Shared>,
}

impl Modem {
    /// Starts the thread, which waits for the data partition its files are on.
    pub fn start(data: Data) -> io::Result<Modem> {
        let shared =
            Arc::new(Shared { board: Board::new(State::default())?, nudge: Wake::new()?, answered: Wake::new()? });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name(THREAD.into()).spawn(move || {
            if let Err(e) = work(&worker, &data) {
                log!("cell: the modem's thread ends: {e}");
            }
            worker.board.lock().end_thread();
            worker.answered.raise();
            worker.board.notify();
            worker.board.raise();
        })?;
        Ok(Modem { shared })
    }

    /// Readable when the modem's status changed.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes the thread's report.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }

    /// Sets whether the modem is wanted; the thread follows.
    pub fn want(&self, is_wanted: bool) {
        if self.shared.board.lock().want(is_wanted) {
            self.shared.nudge.raise();
        }
    }

    /// Sets whether a PIN the SIM takes is kept; the thread follows.
    pub fn keep_pin(&self, keeps_pin: bool) {
        if self.shared.board.lock().keep_pin(keeps_pin) {
            self.shared.nudge.raise();
        }
    }

    pub fn enter_pin(&self, pin: Pin) {
        self.shared.board.lock().enter_pin(pin);
        self.shared.nudge.raise();
    }

    /// Sets whether the attach outlasts the stays; the thread follows.
    pub fn hold_attach(&self, holds_attach: bool) {
        if self.shared.board.lock().hold_attach(holds_attach) {
            self.shared.nudge.raise();
        }
    }

    /// Sets whether the device is awake: a stay begun awake keeps its attach after it.
    pub fn set_awake(&self, is_awake: bool) {
        if self.shared.board.lock().set_awake(is_awake) {
            self.shared.nudge.raise();
        }
    }

    /// Hands the modem an AT command, its answer told in the log.
    pub fn send(&self, command: String) {
        self.shared.board.lock().send(command);
        self.shared.nudge.raise();
    }

    /// True while the thread works on nothing and nothing waits for it.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// True while the modem takes asks for a data connection.
    pub fn is_usable(&self) -> bool {
        self.shared.is_usable()
    }

    /// The radio's side of the modem.
    pub fn cellular(&self) -> Cellular {
        Cellular(Arc::clone(&self.shared))
    }

    /// Stops the modem and waits until the kernel has, or until `deadline`.
    pub fn shut(&self, deadline: Instant) -> io::Result<()> {
        self.want(false);
        let limit = deadline.saturating_duration_since(Instant::now());
        let guard = self.shared.board.lock();
        if self.shared.board.wait_while_within(guard, limit, |state| state.is_running()) {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::TimedOut, NOT_STOPPED))
        }
    }
}

/// The radio's handle of the modem: whether it takes asks, a data connection asked for, and its letting go.
#[derive(Clone)]
pub struct Cellular(Arc<Shared>);

impl Cellular {
    /// True while the modem takes asks for a data connection.
    pub fn is_usable(&self) -> bool {
        self.0.is_usable()
    }

    /// Asks for a data connection and waits for the answer, unless `should_yield` says to give way once `preempt` is readable: then the answer if it came meanwhile, else none.
    pub fn join(&self, preempt: RawFd, should_yield: impl Fn() -> bool) -> io::Result<Option<Result<Link, String>>> {
        let mut epoll = Epoll::new()?;
        epoll.add(self.0.answered.fd(), ANSWERED)?;
        epoll.add(preempt, PREEMPT)?;
        let number = self.0.board.lock().ask_link();
        self.0.nudge.raise();
        loop {
            if let Some(answer) = self.0.board.lock().take_answer(number) {
                return Ok(Some(answer));
            }
            let ready = epoll.wait(None)?;
            self.0.answered.clear();
            if ready.contains(&PREEMPT) && should_yield() {
                let (answer, is_news) = self.0.board.lock().take_back(number);
                if is_news {
                    self.0.nudge.raise();
                }
                return Ok(answer);
            }
        }
    }

    /// Lets go of the data connection.
    pub fn release(&self) {
        self.0.board.lock().release_link();
        self.0.nudge.raise();
    }
}
