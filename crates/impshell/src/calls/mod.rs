mod far;
mod pictures;
mod stream;

use crate::crypto::Library;
use crate::log;
use crate::net::{dial, web};
use crate::sound::{self, Capture, Played, SoundStream};
use crate::storage::logs;
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake};
use crate::wifi::radio::{Hold, Online};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::domain::calls::state::{Answer, Report, State};
use r1_funcore::domain::calls::{Machine, Order, Step};
use r1_funcore::domain::camera::CallPicture;
use r1_funcore::domain::signal::state::CallReport;
use r1_funcore::gfx::Picture;
use r1_funcore::hw::GOVERNED;
use r1_funcore::hw::sound::call::{Microphone, Packet, RATE, Sound, Speaker};
use r1_funcore::logs::snapshot::{CALL_END, CALL_START};
use r1_funcore::net::MTU;
use r1_funcore::net::ice::Socket;
use r1_funcore::signal::service::Request;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use stream::Stream;

const NUDGE: u64 = 0;
const DOWN: u64 = 1;
const SOUND: u64 = 2;
// The first of the tokens the call's sockets are watched by, each socket the next.
const SOCKETS: u64 = 3;
const TAG: &str = "calls";
const FETCH_TAG: &str = "call requests";

struct Shared {
    board: Board<State>,
    nudge: Wake,
}

impl Shared {
    fn post(&self, answer: Answer) {
        self.board.lock().add_answer(answer);
        self.nudge.raise();
    }
}

// Tells the board as the calls' thread ends, by an error or a panic, that no call stands and no picture goes, and wakes the main loop and the pictures' threads for it.
struct Ending<'a>(&'a Shared);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.board.lock().end_thread();
        self.0.board.raise();
        self.0.board.notify();
    }
}

// The paced clock's time, and the wall clock's in milliseconds.
fn clocks() -> (Duration, u64) {
    (Duration::from_nanos(sys::boottime_ns()), sys::realtime_ms())
}

// Puts the calling thread of a call's sound above the drawing thread, so that its blocks keep their cadence whatever makes pictures.
fn prioritize(thread: &str) {
    if let Err(e) = sys::set_sound_priority() {
        log!("calls: {thread} thread's priority: {e}");
    }
}

enum Wire {
    Datagrams(UdpSocket),
    Stream(Stream),
}

// A socket of the call's agent, by the agent's name for it and the token it is watched by.
struct Open {
    socket: Socket,
    token: u64,
    wire: Wire,
}

impl Open {
    fn fd(&self) -> RawFd {
        match &self.wire {
            Wire::Datagrams(udp) => udp.as_raw_fd(),
            Wire::Stream(stream) => stream.fd(),
        }
    }
}

// A call's sound while it is talked in: what passes between the card's two threads and this one, under a lock that lends its holder a waiter's priority, as this thread runs under the drawing one and the card's threads above it; the packets taken from it, whose buffers go back; and the microphone and the speaker, which stop as it is let go of.
struct Live {
    sound: Arc<sys::PiMutex<Sound>>,
    packets: Vec<Packet>,
    _capture: SoundStream,
    _playback: SoundStream,
}

struct Worker<'a> {
    shared: &'a Arc<Shared>,
    online: &'a Online,
    epoll: Epoll,
    hold: Option<Hold>,
    opens: Vec<Open>,
    tokens: u64,
    // Raised by the microphone's thread where a packet of its sound waits.
    recorded: Arc<Wake>,
    live: Option<Live>,
}

impl Worker<'_> {
    // Starts the microphone and the speaker on a sound of the call's own: each thread works on its side outside the lock and reads its clock beside the card's delay, so that neither waits on the other's work and its lateness moves no stamp; what is recorded wakes this thread a packet at a time.
    fn start_sound(&mut self) -> io::Result<()> {
        let sound = Arc::new(sys::PiMutex::new(Sound::default())?);
        let (taken, recorded, mut microphone) = (Arc::clone(&sound), Arc::clone(&self.recorded), Microphone::default());
        let capture = sound::capture(RATE, move |capture| match capture {
            Capture::Samples(block, delay) => {
                let now = clocks().0;
                microphone.take(&mut taken.lock());
                microphone.record(block, now, delay);
                if microphone.give(&mut taken.lock()) {
                    recorded.raise();
                }
            }
            Capture::Gap(samples) => microphone.lose(samples),
            Capture::Start(_) => prioritize("the microphone's"),
            Capture::End => {}
        })?;
        let (played, heard) = (Arc::clone(&sound), Arc::clone(&sound));
        let (mut speaker, mut is_prioritized) = (Speaker::default(), false);
        let playback = sound::play_heard(
            move |block, delay| {
                let now = clocks().0;
                if !std::mem::replace(&mut is_prioritized, true) {
                    prioritize("the speaker's");
                }
                speaker.exchange(&mut played.lock(), now, delay);
                speaker.play(now, delay, block);
                true
            },
            move |played| match played {
                Played::Block(block) => heard.lock().hear(block),
                Played::Underrun => heard.lock().note_underrun(),
            },
        )?;
        self.live = Some(Live { sound, packets: Vec::new(), _capture: capture, _playback: playback });
        Ok(())
    }

    // Stops the call's sound, and logs what came of it and what the pictures cost as they stop with it.
    fn stop_sound(&mut self) {
        let lines = self.live.take().map(|live| live.sound.lock().to_lines()).unwrap_or_default();
        let figures = self.shared.board.lock().take_figures();
        lines.iter().chain(&figures.to_lines()).for_each(|line| log!("{line}"));
    }

    // Hands the machine a copy of the packets of the microphone's sound that wait, made outside the lock, and gives theirs back, so that the microphone's thread codes into their buffers and takes no memory.
    fn take_sound(&mut self, machine: &mut Machine) {
        self.recorded.clear();
        let packets = self.live.as_mut().map(|live| {
            live.sound.lock().take_packets(&mut live.packets);
            let copies = live.packets.clone();
            live.sound.lock().give_back(&mut live.packets);
            copies
        });
        machine.take_sound(&Library, packets.unwrap_or_default(), clocks().0);
    }

    fn release(&mut self) {
        if let Some(hold) = self.hold.take()
            && let Err(e) = self.epoll.remove(hold.down().fd())
        {
            log!("calls: ending the watch of the stay's end: {e}");
        }
    }

    // Holds the network for the call and watches for the stay's end; true where a stay stands.
    fn hold(&mut self) -> bool {
        self.release();
        match self.online.hold() {
            Ok(hold) => {
                if let Err(e) = self.epoll.add(hold.down().fd(), DOWN) {
                    log!("calls: watching the stay's end: {e}");
                }
                self.hold = Some(hold);
                true
            }
            Err(fault) => {
                log!("calls: no stay in a network: {fault}");
                false
            }
        }
    }

    // Asks the calling server a request on a thread beside this one under a share of the hold, so that the stay stands while it is asked and this thread waits on no answer; what came of it is told on the board.
    fn fetch(&self, number: u32, request: Request<'static>) {
        let Some(hold) = self.hold.as_ref().map(Hold::share) else {
            log!("calls: no stay for a request of the calling server");
            return self.shared.post(Answer::Fetched(number, None));
        };
        let shared = Arc::clone(self.shared);
        let asked = thread::Builder::new().name(FETCH_TAG.into()).spawn(move || {
            // A request waits on the network and is no real-time work, which its starter's priority would make it.
            if let Err(e) = sys::reset_scheduling() {
                log!("calls: a request's scheduling: {e}");
            }
            let answer = web::ask_request(&request, hold.down());
            let answer = answer.inspect_err(|e| log!("calls: a request of the calling server: {e}")).ok();
            shared.post(Answer::Fetched(number, answer));
        });
        if let Err(e) = asked {
            log!("calls: the thread of a request: {e}");
            self.shared.post(Answer::Fetched(number, None));
        }
    }

    // Watches a socket of the agent's under a token of its own, which it gives back.
    fn watch(&mut self, socket: Socket, wire: Wire) -> io::Result<u64> {
        self.tokens += 1;
        let open = Open { socket, token: SOCKETS + self.tokens, wire };
        self.epoll.add(open.fd(), open.token)?;
        self.opens.push(open);
        Ok(SOCKETS + self.tokens)
    }

    // Opens a socket of the agent's on an address of the device's, any port, whose calls never wait: where it is bound.
    fn bind(&mut self, socket: Socket, local: IpAddr) -> io::Result<SocketAddr> {
        let udp = UdpSocket::bind((local, 0))?;
        udp.set_nonblocking(true)?;
        let bound = udp.local_addr()?;
        self.watch(socket, Wire::Datagrams(udp))?;
        Ok(bound)
    }

    // Begins a stream of the agent's to an address, under TLS where the server is named, watched for the connection to come.
    fn connect(&mut self, socket: Socket, far: &SocketAddr, name: Option<&str>) -> io::Result<()> {
        let stream = Stream::begin(far, name)?;
        let fd = stream.fd();
        let token = self.watch(socket, Wire::Stream(stream))?;
        self.epoll.set_writable(fd, token, true)
    }

    fn close(&mut self, socket: Socket) {
        let Some(at) = self.opens.iter().position(|open| open.socket == socket) else { return };
        let open = self.opens.swap_remove(at);
        if let Err(e) = self.epoll.remove(open.fd()) {
            log!("calls: ending the watch of a socket: {e}");
        }
        if let Wire::Stream(stream) = open.wire {
            stream.close();
        }
    }

    // Sends a datagram from a socket; one there is no room for now is dropped, as a link drops it.
    fn send(&self, socket: Socket, to: SocketAddr, datagram: &[u8]) {
        let wire = self.opens.iter().find(|open| open.socket == socket).map(|open| &open.wire);
        let Some(Wire::Datagrams(udp)) = wire else { return };
        if let Err(e) = udp.send_to(datagram, to)
            && e.kind() != io::ErrorKind::WouldBlock
            && e.raw_os_error() != Some(libc::ENOBUFS)
        {
            log!("calls: a datagram not sent: {e}");
        }
    }

    // Writes bytes to a stream, which is watched for room while some wait; a stream that broke is over.
    fn write(&mut self, machine: &mut Machine, socket: Socket, bytes: &[u8]) {
        let Some(open) = self.opens.iter_mut().find(|open| open.socket == socket) else { return };
        let Wire::Stream(stream) = &mut open.wire else { return };
        let (fd, token) = (stream.fd(), open.token);
        let watched = stream.write(bytes).and_then(|is_waiting| self.epoll.set_writable(fd, token, is_waiting));
        if let Err(e) = watched {
            log!("calls: a stream broke: {e}");
            machine.take_closed(socket);
            self.close(socket);
        }
    }

    // Reads what a socket of datagrams holds, a datagram a call, and hands each to the machine.
    fn drain(&self, machine: &mut Machine, token: u64, now: Duration) {
        let Some(open) = self.opens.iter().find(|open| open.token == token) else { return };
        let Wire::Datagrams(udp) = &open.wire else { return };
        let mut datagram = [0u8; MTU];
        loop {
            match udp.recv_from(&mut datagram) {
                Ok((count, from)) => machine.take_datagram(&Library, open.socket, from, &datagram[..count], now),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => return log!("calls: reading a socket: {e}"),
            }
        }
    }

    // Serves a stream that is ready: its connection told the machine once it has come or failed, what it holds read and handed on, what waits for room sent; a stream that ended or broke is over.
    fn serve_stream(&mut self, machine: &mut Machine, token: u64, now: Duration) {
        let Some(open) = self.opens.iter_mut().find(|open| open.token == token) else { return };
        let Wire::Stream(stream) = &mut open.wire else { return };
        let (socket, fd) = (open.socket, stream.fd());
        match stream.take_connected() {
            Ok(Some(local)) => machine.take_connected(&Library, socket, Some(local), now),
            Ok(None) => {}
            Err(e) => {
                log!("calls: a stream not connected: {e}");
                machine.take_connected(&Library, socket, None, now);
                return self.close(socket);
            }
        }
        let mut bytes = Vec::new();
        let read = stream.read(&mut bytes);
        machine.take_stream(&Library, socket, &bytes, now);
        let kept = read.and_then(|is_open| if is_open { stream.flush().map(Some) } else { Ok(None) });
        let watched = kept.and_then(|waits| match waits {
            Some(is_waiting) => self.epoll.set_writable(fd, token, is_waiting).map(|()| true),
            None => Ok(false),
        });
        if let Err(e) = &watched {
            log!("calls: a stream broke: {e}");
        }
        if !watched.unwrap_or(false) {
            machine.take_closed(socket);
            self.close(socket);
        }
    }

    fn take_wishes(&self, machine: &mut Machine) {
        let (now, wall) = clocks();
        let wishes = self.shared.board.lock().take_wishes();
        machine.take_wishes(&Library, wishes, now, wall);
    }

    fn take_ready(&mut self, machine: &mut Machine, token: u64) {
        match token {
            NUDGE => {
                self.shared.nudge.clear();
                self.take_wishes(machine);
            }
            DOWN => {
                self.release();
                machine.take_down();
            }
            SOUND => self.take_sound(machine),
            token => {
                self.drain(machine, token, clocks().0);
                self.serve_stream(machine, token, clocks().0);
            }
        }
    }

    fn wait(&mut self, machine: &mut Machine, limit: Option<Duration>) -> io::Result<()> {
        let (has_picture_news, has_news) = machine.leave(&mut self.shared.board.lock());
        if has_picture_news {
            self.shared.board.notify();
        }
        if has_news {
            self.shared.board.raise();
        }
        for token in self.epoll.wait(limit)? {
            self.take_ready(machine, token);
        }
        Ok(())
    }

    fn act(&mut self, machine: &mut Machine, step: Step) -> io::Result<()> {
        let (now, wall) = clocks();
        match step {
            Step::Hold => {
                let is_held = self.hold();
                machine.take_hold(&Library, is_held, now, wall);
            }
            Step::Release => self.release(),
            Step::Addresses => {
                let addresses = self.hold.as_ref().map(|hold| dial::local_addresses(hold.down())).unwrap_or_default();
                self.shared.board.lock().add_answer(Answer::Addresses(addresses));
                self.take_wishes(machine);
            }
            Step::Bind { socket, local } => {
                let bound = self.bind(socket, local).inspect_err(|e| log!("calls: opening a socket: {e}")).ok();
                machine.take_bound(&Library, socket, bound, now);
            }
            Step::Connect { socket, far, name, .. } => {
                if let Err(e) = self.connect(socket, &far, name.as_deref()) {
                    log!("calls: beginning a stream: {e}");
                    self.close(socket);
                    machine.take_connected(&Library, socket, None, now);
                }
            }
            Step::Send { socket, to, datagram } => self.send(socket, to, &datagram),
            Step::Write { socket, bytes } => self.write(machine, socket, &bytes),
            Step::Close(socket) => self.close(socket),
            Step::Sound(true) => {
                logs::snapshot(CALL_START);
                if let Err(e) = self.start_sound() {
                    log!("calls: the microphone and the speaker: {e}");
                }
            }
            Step::Sound(false) => {
                self.stop_sound();
                logs::snapshot(CALL_END);
            }
            Step::Mute(is_muted) => {
                self.live.iter().for_each(|live| live.sound.lock().set_muted(is_muted));
            }
            Step::Hear { member, sequence, timestamp, payload } => {
                self.live.iter().for_each(|live| live.sound.lock().push(member, sequence, timestamp, now, &payload));
            }
            Step::Fetch { number, request } => self.fetch(number, request),
            Step::Log(line) => log!("{line}"),
            Step::Wait(limit) => self.wait(machine, limit)?,
        }
        Ok(())
    }
}

fn work(shared: &Arc<Shared>, online: &Online) -> io::Result<()> {
    let (mut epoll, recorded) = (Epoll::new()?, Arc::new(Wake::new()?));
    epoll.add(shared.nudge.fd(), NUDGE)?;
    epoll.add(recorded.fd(), SOUND)?;
    let mut worker = Worker { shared, online, epoll, hold: None, opens: Vec::new(), tokens: 0, recorded, live: None };
    // A call's packets wait behind no ordinary thread, the pictures' encoder among them; the governor clocks the core.
    if let Err(e) = sys::set_worker_priority(GOVERNED) {
        log!("calls: the thread's priority: {e}");
    }
    let mut machine = Machine::default();
    loop {
        let (now, wall) = clocks();
        let step = machine.next(&Library, now, wall);
        worker.act(&mut machine, step)?;
    }
}

/// The main loop's handle of the calls' thread, which carries Signal's calls.
pub struct Calls {
    shared: Arc<Shared>,
}

impl Calls {
    /// Starts the threads: the calls' thread, which holds the network through `online` while a call stands, the pictures' thread, and the far pictures' thread, which makes the far side's pictures in `format`.
    pub fn start(online: Online, format: PixelFormat) -> io::Result<Calls> {
        let shared = Arc::new(Shared { board: Board::new(State::new(format))?, nudge: Wake::new()? });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name(TAG.into()).spawn(move || {
            let _ending = Ending(&worker);
            if let Err(e) = work(&worker, &online) {
                log!("calls: the thread ends: {e}");
            }
        })?;
        let coder = Arc::clone(&shared);
        thread::Builder::new().name(pictures::TAG.into()).spawn(move || pictures::run(&coder))?;
        let decoder = Arc::clone(&shared);
        thread::Builder::new().name(far::TAG.into()).spawn(move || far::run(&decoder))?;
        Ok(Calls { shared })
    }

    /// Readable when the calls' report has news.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Hands the thread what the owner asks of a call.
    pub fn ask(&self, order: Order) {
        self.shared.board.lock().order(order);
        self.shared.nudge.raise();
    }

    /// Takes out of Signal's report what is the call layer's, and wakes the thread for it.
    pub fn take_news(&self, report: &mut CallReport) {
        if self.shared.board.lock().take_news(report) {
            self.shared.nudge.raise();
        }
    }

    /// Sets the room the far side's picture is fitted to.
    pub fn want(&self, room: (usize, usize)) {
        self.shared.board.lock().want(room);
    }

    /// True while the camera is to make a picture of each frame of its stream for the far side.
    pub fn wants_picture(&self) -> bool {
        self.shared.board.lock().wants_picture()
    }

    /// Swaps in the camera's newest picture for the far side, which the pictures' thread codes once free, in the place of one that still waits; `picture` holds the old one.
    pub fn add_picture(&self, picture: &mut CallPicture) {
        self.shared.board.lock().add_picture(picture);
        self.shared.board.notify();
    }

    /// Takes the thread's report, the far side's newest picture swapped into `picture`.
    pub fn latest(&self, picture: &mut Picture) -> Report {
        self.shared.board.take(|state| state.take_report(picture))
    }

    /// True while a call stands, which holds Signal's socket for its signals.
    pub fn is_standing(&self) -> bool {
        self.shared.board.lock().is_standing()
    }

    /// True while no call stands and nothing waits for the thread.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::calls::{Ending, Phase};
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::net::ice;
    use r1_funcore::signal::address::{Peer, ServiceId};
    use r1_funcore::signal::command::Command;
    use std::net::Ipv4Addr;

    #[test]
    fn no_network_no_call() {
        let radio = without_networks();
        let calls = Calls::start(radio.online(), PixelFormat::XRGB8888).unwrap();
        let mut epoll = Epoll::with_fd(calls.fd());
        assert!(calls.is_off() && !calls.is_standing());
        let peer = Peer::Contact(ServiceId::Aci([7; 16]));
        calls.ask(Order::Call { peer, name: None, is_video: false });
        let (mut phase, mut commands) = (None, Vec::new());
        while phase != Some(Phase::Ended(Ending::Failed(Cause::NoNetwork))) {
            epoll.wait(Some(Duration::from_secs(10))).unwrap();
            let report = calls.latest(&mut Picture::default());
            commands.extend(report.commands);
            phase = report.view.flatten().map(|view| view.phase).or(phase);
        }
        assert!(
            commands.contains(&Command::Relays) && calls.is_off(),
            "a call begun without a network asks Signal's thread for the relays, ends for want of a stay, and holds nothing"
        );
    }

    // Has `run` work with a worker that holds no stay and watches its nudge.
    fn with_worker(run: impl FnOnce(&mut Worker<'_>)) {
        let shared = Arc::new(Shared {
            board: Board::new(State::new(PixelFormat::XRGB8888)).unwrap(),
            nudge: Wake::new().unwrap(),
        });
        let radio = without_networks();
        let online = radio.online();
        let (mut epoll, recorded) = (Epoll::new().unwrap(), Arc::new(Wake::new().unwrap()));
        epoll.add(shared.nudge.fd(), NUDGE).unwrap();
        run(&mut Worker {
            shared: &shared,
            online: &online,
            epoll,
            hold: None,
            opens: Vec::new(),
            tokens: 0,
            recorded,
            live: None,
        });
    }

    #[test]
    fn request_without_stay() {
        with_worker(|worker| {
            let request = Request::new("GET", "https://sfu.example/".into(), Vec::new(), Vec::new().into());
            worker.fetch(1, request);
            assert_eq!(
                worker.epoll.wait(Some(Duration::from_secs(10))).unwrap(),
                [NUDGE],
                "a request of the calling server without a stay is answered at once, and wakes the thread for it"
            );
        });
    }

    #[test]
    fn sockets_carry_datagrams() {
        with_worker(carry_datagrams);
    }

    fn carry_datagrams(worker: &mut Worker<'_>) {
        let far = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut agent = ice::Agent::new(&Library, ice::Role::Controlling, false).unwrap();
        agent.local(Ipv4Addr::LOCALHOST.into());
        let Some(ice::Step::Bind { socket, .. }) = agent.next(&Library, Duration::ZERO) else {
            panic!("an agent given an address asks for a socket on it")
        };
        let bound = worker.bind(socket, Ipv4Addr::LOCALHOST.into()).unwrap();
        worker.send(socket, far.local_addr().unwrap(), b"word");
        let mut word = [0u8; 8];
        let (count, from) = far.recv_from(&mut word).unwrap();
        assert_eq!(
            (&word[..count], from),
            (&b"word"[..], bound),
            "a datagram leaves from the address the socket is bound to"
        );
        far.send_to(b"answer", bound).unwrap();
        assert_eq!(
            worker.epoll.wait(Some(Duration::from_secs(10))).unwrap(),
            [SOCKETS + 1],
            "and one for it wakes the thread"
        );
        worker.close(socket);
        assert!(worker.opens.is_empty(), "closed, the socket is watched no more");
    }
}
