use crate::log;
use crate::openai::live::Interpreter;
use crate::openai::{self, Account};
use crate::sound::{self, Capture, SoundStream};
use crate::storage::{Data, InDirectory, stem_now};
use crate::sync::Board;
use crate::sys::{self, Wake, lock};
use crate::wifi::radio::Hold;
use r1_funcore::domain::dialogue::Turn;
use r1_funcore::domain::paper::names::{self, folder};
use r1_funcore::domain::paper::sheet;
use r1_funcore::domain::translator::Uplink;
use r1_funcore::domain::translator::session::{self, Action, CLOSING_TIMEOUT, GONE_LINE, Lanes, Session, UNCLOSED};
use r1_funcore::domain::translator::state::{Report, State};
use r1_funcore::domain::voice::Phase;
use r1_funcore::hw::sound::to_lost_line;
use r1_funcore::openai::fault::{Ending, Fault};
use r1_funcore::openai::ledger::SummaryKind;
use r1_funcore::openai::models::interpreter;
use r1_funcore::storage::TALKS;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Shared {
    board: Board<State>,
    stop: Wake,
    data: Data,
    account: Account,
}

impl Shared {
    fn set_phase(&self, round: u32, phase: Phase) {
        if self.board.lock().set_session(round, phase) {
            self.board.raise();
        }
    }
}

fn microphone(lanes: &Arc<Mutex<Lanes>>, capture_flag: &Arc<Wake>) -> io::Result<SoundStream> {
    let (lanes, flag, mut uplink) = (Arc::clone(lanes), Arc::clone(capture_flag), Uplink::default());
    sound::capture(interpreter::RATE, move |capture| match capture {
        Capture::Samples(samples, _) => {
            uplink.add_samples(samples, &mut lock(&lanes).up);
            flag.raise();
        }
        Capture::Gap(samples) => {
            log!("{}", to_lost_line(samples as u64));
            Uplink::add_lost(samples, &mut lock(&lanes).up);
        }
        Capture::Start(_) | Capture::End => {}
    })
}

fn speaker(lanes: &Arc<Mutex<Lanes>>) -> io::Result<SoundStream> {
    let lanes = Arc::clone(lanes);
    sound::play(
        move |block, ahead| {
            lock(&lanes).fill_block(block, ahead, sys::monotonic_ns());
            true
        },
        || {},
    )
}

struct SessionRun<'a> {
    shared: &'a Shared,
    round: u32,
    interpreter: Interpreter,
    hold: Hold,
    microphone: SoundStream,
    speaker: SoundStream,
    lanes: Arc<Mutex<Lanes>>,
    capture_flag: Arc<Wake>,
    session: Session,
    start: Instant,
}

impl SessionRun<'_> {
    fn run(&mut self) -> Ending {
        loop {
            if let ControlFlow::Break(ending) = self.step() {
                return ending;
            }
        }
    }

    fn step(&mut self) -> ControlFlow<Ending> {
        self.wait()?;
        self.read_messages()
    }

    fn wait(&mut self) -> ControlFlow<Ending> {
        let mut fds = [
            sys::readable(self.interpreter.fd()),
            sys::readable(self.capture_flag.fd()),
            sys::readable(self.shared.stop.fd()),
            sys::readable(self.hold.down().fd()),
        ];
        // One wait for all of it, bounded only once the session's close is asked: until then the microphone's blocks wake it often enough for what goes by the time.
        let now = Instant::now();
        match sys::poll(&mut fds, self.session.wait_limit(sys::monotonic()).map(|limit| now + limit)) {
            Ok(0) => {
                log!("{}", session::to_unanswered_line());
                return ControlFlow::Break(UNCLOSED);
            }
            Ok(_) => {}
            Err(e) => {
                log!("translator: waiting: {e}");
                return ControlFlow::Break(Ending::Failed(e.into()));
            }
        }
        let [_, has_sound, should_stop, is_down] = fds.map(|fd| fd.revents != 0);
        if is_down {
            log!("{GONE_LINE}");
            return ControlFlow::Break(session::to_network_gone());
        }
        if should_stop {
            // The flag is taken down whenever it is up: left up while a session closes, it would end every wait at once.
            self.shared.stop.clear();
            let is_wanted = self.shared.board.lock().is_wanted(self.round);
            // The service is asked to end the session, and answers with what is paid for.
            if self.session.should_close(is_wanted, sys::monotonic())
                && let Err(e) = self.interpreter.end()
            {
                log!("translator: asking the service to end the session: {e}");
                return ControlFlow::Break(UNCLOSED);
            }
        }
        if has_sound {
            self.send_sound()?;
        }
        ControlFlow::Continue(())
    }

    fn send_sound(&mut self) -> ControlFlow<Ending> {
        self.capture_flag.clear();
        // While the speaker sounds the model's voice, and a moment after, silence goes up in the microphone's place: the model must not hear itself.
        self.session.set_echo(sys::monotonic_ns(), lock(&self.lanes).echo_until_ns);
        while self.session.take_piece(&mut lock(&self.lanes).up) {
            if let Some(piece) = self.session.piece()
                && let Err(e) = self.interpreter.send_samples(piece)
            {
                log!("translator: sending the microphone's sound: {e}");
                return ControlFlow::Break(Ending::Failed(e.into()));
            }
            self.session.count_piece();
        }
        if let Some(phase) = self.session.take_phase_change() {
            self.shared.set_phase(self.round, phase);
        }
        ControlFlow::Continue(())
    }

    fn read_messages(&mut self) -> ControlFlow<Ending> {
        loop {
            let (action, line) = match self.interpreter.read_message(|message| self.session.take_message(message)) {
                Ok(Some(step)) => step,
                Ok(None) => return ControlFlow::Continue(()),
                Err(e) => {
                    log!("translator: the session broke: {e}");
                    return ControlFlow::Break(Ending::Failed(e.into()));
                }
            };
            if let Some(line) = line {
                log!("{line}");
            }
            match action {
                Action::Nothing => {}
                Action::PlayVoice => self.session.queue_voice(&mut lock(&self.lanes).down),
                Action::AddSource(words, start_ms) => {
                    self.shared.board.lock().add_source(&words, start_ms);
                    self.shared.board.raise();
                }
                Action::AddTranslation(words, reply_ms) => {
                    self.shared.board.lock().add_translation(&words, reply_ms);
                    self.shared.board.raise();
                }
                // The session is held with nothing behind it: work the model hands over goes back to it, so that it does not wait for an answer that never comes.
                Action::HandBack(work) => {
                    if let Err(e) = self.interpreter.hand_back(&work) {
                        log!("translator: handing the model's work back: {e}");
                        return ControlFlow::Break(Ending::Failed(e.into()));
                    }
                }
                Action::End(ending) => return ControlFlow::Break(ending),
            }
        }
    }

    fn finish(self, ending: Ending) -> (Ending, Option<u64>) {
        drop((self.microphone, self.speaker));
        self.interpreter.close();
        drop(self.hold);
        let playout = lock(&self.lanes).playout;
        let (lines, seconds) = self.session.finish(self.round, self.start.elapsed(), playout);
        lines.iter().for_each(|line| log!("{line}"));
        if let Some(line) = self.session.to_unhandled_line() {
            log!("{line}");
        }
        (ending, seconds)
    }
}

fn run_session(shared: &Shared, round: u32) -> (Ending, Option<u64>) {
    let start = Instant::now();
    let to_failure = |line: String, fault: Fault| {
        log!("translator: {line}");
        (Ending::Failed(fault), None)
    };
    let hold = match shared.account.hold() {
        Ok(hold) => hold,
        Err(fault) => return to_failure(fault.to_string(), fault),
    };
    // The network may take long to come up: a screen left meanwhile has no session opened, which is paid by its seconds.
    if !shared.board.lock().is_wanted(round) {
        return (Ending::Done, None);
    }
    let mut interpreter = match Interpreter::open(&shared.account, hold.down()) {
        Ok(interpreter) => interpreter,
        Err(fault) => {
            return to_failure(format!("no session after {} ms: {fault}", start.elapsed().as_millis()), fault);
        }
    };
    let connect_time = start.elapsed();
    let lanes = Arc::new(Mutex::new(Lanes::default()));
    let capture_flag = match Wake::new() {
        Ok(flag) => Arc::new(flag),
        Err(e) => {
            interpreter.close();
            return to_failure(format!("no flag for the microphone to raise: {e}"), e.into());
        }
    };
    let (microphone, speaker) = match (microphone(&lanes, &capture_flag), speaker(&lanes)) {
        (Ok(microphone), Ok(speaker)) => (microphone, speaker),
        (microphone, speaker) => {
            interpreter.close();
            let reason = microphone.err().or(speaker.err()).map_or_else(String::new, |e| e.to_string());
            return to_failure(format!("no sound for the session: {reason}"), Fault::Temporary(reason));
        }
    };
    interpreter.set_timeout(CLOSING_TIMEOUT);
    let result = interpreter.greet().inspect_err(|e| log!("translator: asking the model to introduce itself: {e}"));
    shared.set_phase(round, Phase::Listening);
    log!("{}", session::to_stood_line(round, connect_time.as_millis()));
    let session = Session::new(connect_time);
    let mut run =
        SessionRun { shared, round, interpreter, hold, microphone, speaker, lanes, capture_flag, session, start };
    let ending = match result {
        Ok(()) => run.run(),
        Err(e) => Ending::Failed(e.into()),
    };
    run.finish(ending)
}

fn write_talk(shared: &Shared, seconds: u64, turns: &[Turn]) -> io::Result<String> {
    let stem = stem_now(InDirectory(&shared.data, TALKS))?;
    let (place, name) = (folder(SummaryKind::Talk, &stem), names::words_file(SummaryKind::Talk, &stem));
    shared.data.put(&place, &name, sheet::to_remarks_file(&stem, seconds, turns).as_bytes())?;
    Ok(stem)
}

fn work(shared: &Shared) -> ! {
    loop {
        let round = shared.board.wait(|state| state.request.next_new());
        shared.stop.clear();
        shared.board.lock().begin(round);
        shared.set_phase(round, Phase::Connecting);
        let (ending, paid_seconds) = run_session(shared, round);
        let (seconds, micros) =
            paid_seconds.map_or((0, 0), |seconds| (seconds, openai::live::book_session(&shared.account, seconds)));
        let (turns, has_speech) = {
            let state = shared.board.lock();
            (state.dialogue().turns.clone(), state.dialogue().has_speech())
        };
        log!("{}", session::to_session_line(round, seconds, micros, turns.len()));
        if has_speech {
            match write_talk(shared, seconds, &turns) {
                Ok(stem) => {
                    log!("{}", session::to_kept_line(&stem));
                    shared.board.lock().note_kept(stem);
                }
                Err(e) => log!("translator: the conversation is not kept: {e}"),
            }
        }
        // A session the screen let go of, a sleep's stop included, is no failure, whether its stop or the network's end came first.
        let phase = shared.board.lock().to_phase(round, &ending);
        shared.set_phase(round, phase);
        shared.board.raise();
    }
}

/// The main loop's handle of the translator's thread.
pub struct Translator {
    shared: Arc<Shared>,
}

impl Translator {
    /// Starts the thread, idle until a session is wanted, with conversations on `data` and the model service through `account`.
    pub fn start(data: Data, account: Account) -> io::Result<Translator> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, stop: Wake::new()?, data, account });
        let worker = Arc::clone(&shared);
        sys::spawn_background("translator", move || work(&worker))?;
        Ok(Translator { shared })
    }

    /// Readable when a session or its conversation moves on.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets the number of the session wanted, none for no session; one that is held ends when another or none is wanted.
    pub fn want(&self, round: Option<u32>) {
        if self.shared.board.lock().request.claim(round) {
            self.shared.stop.raise();
            self.shared.board.notify();
        }
    }

    /// True while no session runs on the thread or waits to begin.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// Takes the report for the screen.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::cause::Cause;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn failed_session_reported_thread_rests() {
        let radio = without_networks();
        let translator = Translator::start(Data::unusable(), Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(translator.fd());
        assert!(translator.is_off() && translator.latest() == Report::default(), "no conversation yet");
        translator.want(Some(4));
        let mut phases = Vec::new();
        while phases.last().is_none_or(|(_, session)| *session == Phase::Connecting) {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7]);
            let report = translator.latest();
            assert_eq!(report.remarks, None, "nobody has spoken");
            phases.extend(report.session);
        }
        assert_eq!(
            phases.last(),
            Some(&(4, Phase::Failed(Cause::SessionFailed))),
            "without a partition there is no key to ask the service with, and the screen is told so under the session's number"
        );
        translator.want(None);
        while !translator.is_off() {
            thread::yield_now();
        }
        translator.want(Some(4));
        thread::yield_now();
        assert_eq!(translator.latest().session, None, "a session that was held and has ended is not begun again");
    }
}
