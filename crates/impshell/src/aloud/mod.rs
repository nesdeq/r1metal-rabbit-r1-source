use crate::log;
use crate::openai::aloud::ReadingSession;
use crate::openai::{self, Account};
use crate::sound::{self, SoundStream};
use crate::sync::Board;
use crate::sys::{self, Wake, lock};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::domain::aloud::reading::{
    self, CLOSED_UNPAID_LINE, GONE_UNPAID_LINE, Lane, MessageOutcome, Reading, SESSION_ENDED_LINE, SETTLE_TIMEOUT,
    SessionStats, Settle, SettleWait, Step,
};
use r1_funcore::domain::aloud::state::{ReadingRequest, Report, State};
use r1_funcore::domain::aloud::{ReadingSource, Script};
use r1_funcore::domain::cause::Cause;
use r1_funcore::domain::voice::Phase;
use r1_funcore::openai::fault::{self, Fault};
use std::io;
use std::mem;
use std::ops::ControlFlow;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Shared {
    board: Board<State>,
    stop: Wake,
    account: Account,
}

impl Shared {
    fn set_phase(&self, reading: (u32, ReadingSource), phase: Phase) {
        if self.board.lock().set_phase(reading, phase) {
            self.board.raise();
        }
    }

    fn is_wanted(&self, reading: (u32, ReadingSource)) -> bool {
        self.board.lock().is_wanted(reading)
    }

    fn set_owing(&self, is_owing: bool) {
        if self.board.lock().set_owing(is_owing) {
            self.board.raise();
        }
    }
}

struct Connection {
    session: ReadingSession,
    hold: Hold,
    start: Instant,
}

impl Connection {
    fn end(self, sessions: &mut SessionStats) {
        sessions.note_ended(self.start.elapsed());
        self.session.close();
    }
}

fn connect(shared: &Shared, is_repeat: bool, task: &str) -> Result<Connection, Fault> {
    let start = Instant::now();
    let result = if is_repeat { shared.account.hold_quietly() } else { shared.account.hold() };
    let hold = result.inspect_err(|fault| log!("aloud: {fault}"))?;
    let session = ReadingSession::open(&shared.account, hold.down(), task)
        .inspect_err(|fault| log!("aloud: no session after {} ms: {fault}", start.elapsed().as_millis()))?;
    log!("{}", reading::to_session_line(start.elapsed().as_millis()));
    Ok(Connection { session, hold, start })
}

fn speak(lane: &Arc<Mutex<Lane>>, speaker_end: &Arc<Wake>) -> io::Result<SoundStream> {
    let (lane, speaker_end) = (Arc::clone(lane), Arc::clone(speaker_end));
    sound::play(move |block, _| lock(&lane).fill_block(block), move || speaker_end.raise())
}

fn take_message(reading: &mut Reading, message: Option<&str>, lane: Option<&Mutex<Lane>>) -> MessageOutcome {
    let (outcome, line) = reading.take_message(message, sys::monotonic(), lane.is_some());
    if let Some(lane) = lane {
        lock(lane).add_voice(reading.voice());
    }
    if let Some(line) = line {
        log!("{line}");
    }
    outcome
}

fn settle(reading: &mut Reading, session: &mut ReadingSession, down: &NetworkDown, limit: Duration) {
    let mut wait = SettleWait::new(sys::monotonic(), limit);
    while reading.is_in_flight() {
        loop {
            match session.read_message(|message| take_message(reading, message, None)) {
                Ok(Some(MessageOutcome::Closed)) => return log!("{CLOSED_UNPAID_LINE}"),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => return log!("aloud: the session broke before the service told what the passage cost: {e}"),
            }
        }
        match wait.next_step(reading, sys::monotonic()) {
            Settle::Done => break,
            Settle::Stop(response) => {
                if let Err(e) = session.stop(response) {
                    return log!("aloud: stopping the passage that was on its way: {e}");
                }
            }
            Settle::Wait(limit) => {
                let mut fds = [sys::readable(session.fd()), sys::readable(down.fd())];
                if let Err(e) = sys::poll(&mut fds, Some(Instant::now() + limit)) {
                    return log!("aloud: waiting for the service's word on the passage: {e}");
                }
                let [_, is_down] = fds.map(|fd| fd.revents != 0);
                if is_down {
                    return log!("{GONE_UNPAID_LINE}");
                }
            }
            Settle::Late(line) => return log!("{line}"),
        }
    }
}

// Asks for the reading's next passage over its session.
fn ask(shared: &Shared, reading: &mut Reading, connection: &mut Connection) -> ControlFlow<fault::Ending> {
    // Owed from before the asking, so that a sleep that comes between the two does not take the network from under it.
    shared.set_owing(true);
    let (number, words) = reading.next_passage();
    if let Err(e) = connection.session.ask(number, words) {
        log!("{}", reading.fail_asking(&e));
        return ControlFlow::Break(fault::Ending::Failed(e.into()));
    }
    reading.mark_asked(sys::monotonic());
    ControlFlow::Continue(())
}

struct ReadingRun<'a> {
    shared: &'a Shared,
    round: u32,
    source: ReadingSource,
    task: String,
    lane: Arc<Mutex<Lane>>,
    speaker_end: Arc<Wake>,
    reading: Reading,
    connection: Option<Connection>,
    speaker: Option<SoundStream>,
    sessions: SessionStats,
    start: Instant,
}

impl ReadingRun<'_> {
    fn step(&mut self) -> ControlFlow<fault::Ending> {
        let hold_state = self.shared.board.lock().follow((self.round, self.source), &mut self.reading);
        self.post_heard();
        let line = self.reading.begin_pass(hold_state, &mut lock(&self.lane));
        if let Some(line) = line {
            log!("{line}");
        }
        loop {
            let (is_playing, has_link) = (self.speaker.is_some(), self.connection.is_some());
            match self.reading.next_step(is_playing, has_link, sys::monotonic()) {
                Step::End(ending) => return ControlFlow::Break(ending),
                Step::Speak => match speak(&self.lane, &self.speaker_end) {
                    Ok(speaker) => self.speaker = Some(speaker),
                    Err(e) => {
                        log!("aloud: the speaker: {e}");
                        return ControlFlow::Break(fault::Ending::Failed(e.into()));
                    }
                },
                Step::Close(line) => {
                    if let Some(connection) = self.connection.take() {
                        connection.end(&mut self.sessions);
                    }
                    if let Some(line) = line {
                        log!("{line}");
                    }
                }
                Step::Connect(limit) => {
                    if let Err(fault) = self.connect()
                        && let Some(ending) = self.reading.fail_connecting(fault, limit, sys::monotonic())
                    {
                        return ControlFlow::Break(ending);
                    }
                }
                Step::Ask => {
                    // A passage is asked for only over a session that stands, which `has_link` told.
                    if let Some(connection) = self.connection.as_mut() {
                        ask(self.shared, &mut self.reading, connection)?;
                    }
                }
                Step::Wait(limit) => self.wait(limit)?,
                Step::Read => {
                    if let Some(ending) = self.read_messages() {
                        return ControlFlow::Break(ending);
                    }
                }
                Step::Pass => return ControlFlow::Continue(()),
            }
        }
    }

    fn post_heard(&self) {
        let count = self.reading.heard_count(lock(&self.lane).played_samples());
        if self.shared.board.lock().set_heard((self.round, self.source), count) {
            self.shared.board.raise();
        }
    }

    fn connect(&mut self) -> Result<(), Fault> {
        let start = Instant::now();
        let connection = connect(self.shared, self.sessions.is_repeat(), &self.task)?;
        self.reading.mark_connected(start.elapsed());
        self.sessions.note_opened();
        self.connection = Some(connection);
        Ok(())
    }

    fn wait(&mut self, limit: Option<Duration>) -> ControlFlow<fault::Ending> {
        let mut fds = [
            sys::readable(self.shared.stop.fd()),
            sys::readable(self.speaker_end.fd()),
            sys::readable(self.connection.as_ref().map_or(-1, |connection| connection.session.fd())),
            sys::readable(self.connection.as_ref().map_or(-1, |connection| connection.hold.down().fd())),
        ];
        // Nothing wakes the wait when the voice runs short, or when the service falls silent over a passage: those moments are waited for by the clock.
        let now = Instant::now();
        if let Err(e) = sys::poll(&mut fds, limit.map(|limit| now + limit)) {
            log!("aloud: waiting: {e}");
            return ControlFlow::Break(fault::Ending::Failed(e.into()));
        }
        let [is_stopped, is_speaker_end, _, is_down] = fds.map(|fd| fd.revents != 0);
        if is_stopped {
            self.shared.stop.clear();
        }
        if is_speaker_end {
            // The speaker has gone off: held, or with the last of the voice played.
            self.speaker_end.clear();
            self.speaker = None;
        }
        if is_down {
            let is_wanted = self.shared.is_wanted((self.round, self.source));
            log!("{}", reading::to_gone_line(is_wanted));
            return ControlFlow::Break(self.reading.end_offline(is_wanted));
        }
        ControlFlow::Continue(())
    }

    fn read_messages(&mut self) -> Option<fault::Ending> {
        let connection = self.connection.as_mut()?;
        let (reading, lane) = (&mut self.reading, &self.lane);
        loop {
            let outcome = match connection.session.read_message(|message| take_message(reading, message, Some(lane))) {
                Ok(Some(outcome)) => outcome,
                Ok(None) => break,
                Err(e) => {
                    log!("aloud: the session broke: {e}");
                    reading.lose_session();
                    return Some(fault::Ending::Failed(e.into()));
                }
            };
            match &outcome {
                MessageOutcome::Began => self.shared.set_phase((self.round, self.source), Phase::Speaking),
                MessageOutcome::Done => self.shared.set_owing(false),
                MessageOutcome::Closed => log!("{SESSION_ENDED_LINE}"),
                MessageOutcome::Refused(_) | MessageOutcome::Nothing => {}
            }
            if let Some(ending) = outcome.into_ending() {
                return Some(ending);
            }
        }
        let (ending, line) = reading.check_stall(sys::monotonic())?;
        log!("{line}");
        Some(ending)
    }

    fn finish(mut self, ending: fault::Ending) -> (fault::Ending, Option<u64>) {
        lock(&self.lane).is_held = true;
        drop(self.speaker.take());
        self.post_heard();
        if let Some(mut connection) = self.connection.take() {
            if self.reading.should_settle() {
                settle(&mut self.reading, &mut connection.session, connection.hold.down(), SETTLE_TIMEOUT);
            }
            connection.end(&mut self.sessions);
        }
        // Whatever was owed is paid for or given up: the network may go now, the sleep's included.
        self.shared.set_owing(false);
        let silence = lock(&self.lane).silence();
        let (line, micros) = self.reading.finish(self.round, self.start.elapsed(), silence, &self.sessions);
        log!("{line}");
        (ending, micros)
    }
}

fn run_reading(shared: &Shared, (round, source): (u32, ReadingSource), script: Script) -> (fault::Ending, Option<u64>) {
    // A reading without a key fails at once, before anything of it is begun.
    if let Err(fault) = shared.account.check_key() {
        log!("aloud: {fault}");
        return (fault::Ending::Failed(fault), None);
    }
    let (lane, speaker_end) = match Wake::new() {
        Ok(speaker_end) => (Arc::default(), Arc::new(speaker_end)),
        Err(e) => {
            log!("aloud: no flag for the speaker to raise: {e}");
            return (fault::Ending::Failed(e.into()), None);
        }
    };
    let Script { passages, is_growing, task } = script;
    let mut run = ReadingRun {
        shared,
        round,
        source,
        task,
        lane,
        speaker_end,
        reading: Reading::new(passages, is_growing, sys::monotonic()),
        connection: None,
        speaker: None,
        sessions: SessionStats::default(),
        start: Instant::now(),
    };
    let ending = loop {
        if let ControlFlow::Break(ending) = run.step() {
            break ending;
        }
    };
    run.finish(ending)
}

fn work(shared: &Shared) -> ! {
    loop {
        let ((round, source), script) = shared.board.wait(|state| state.next_reading());
        shared.stop.clear();
        shared.board.lock().request.begin((round, source));
        shared.set_phase((round, source), Phase::Connecting);
        log!("{}", source.to_begun_line(round, script.passages.len()));
        let (ending, micros) = run_reading(shared, (round, source), script);
        if let Some(micros) = micros {
            openai::aloud::book_reading(&shared.account, source.ledger_kind(), micros);
        }
        let is_wanted = shared.is_wanted((round, source));
        shared.set_phase((round, source), Phase::from_ending(Cause::from_ending(&ending), is_wanted));
    }
}

/// The main loop's handle of the reader's thread.
pub struct Reader {
    shared: Arc<Shared>,
}

impl Reader {
    /// Starts the thread, idle until a reading is wanted; `account` has the key, the ledger and the network.
    pub fn start(account: Account) -> io::Result<Reader> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, stop: Wake::new()?, account });
        let worker = Arc::clone(&shared);
        sys::spawn_background("aloud", move || work(&worker))?;
        Ok(Reader { shared })
    }

    /// Readable when a reading has got on.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets which reading is wanted, held or not, or that none is; a running one ends when another or none is wanted.
    pub fn want(&self, request: Option<ReadingRequest>) {
        if self.shared.board.lock().set_wanted(request) {
            self.shared.stop.raise();
            self.shared.board.notify();
        }
    }

    /// Hands over the reply of `round` as far as it is written.
    pub fn set_reply(&self, round: u32, script: Script) {
        if self.shared.board.lock().set_reply(round, script) {
            self.shared.stop.raise();
            self.shared.board.notify();
        }
    }

    /// True while no reading runs on the thread or waits to begin.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// True while a passage is on its way whose cost the service has not told: the network stays until it has, `SETTLE_TIMEOUT` at the most once the reading is let go of.
    pub fn is_owing(&self) -> bool {
        self.shared.board.lock().is_owing
    }

    /// Takes the report on the readings of a text and of the conversation.
    pub fn latest(&self) -> Report {
        self.shared.board.take(|state| mem::take(&mut state.report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Peer, peer};
    use crate::storage::Data;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::{up, without_networks};
    use r1_funcore::domain::aloud::Words;
    use r1_funcore::domain::shelf::Shelf;
    use r1_funcore::openai::aloud::TASK;
    use std::thread::{self, JoinHandle};

    const CREATED: &str = r#"{"type":"response.created","response":{"id":"resp_9","status":"in_progress"}}"#;
    const STOPPED: &str = r#"{"type":"response.done","response":{"id":"resp_9","status":"cancelled","status_details":{"type":"cancelled","reason":"client_cancelled"},"usage":{"input_token_details":{"text_tokens":1270,"cached_tokens_details":{"text_tokens":1152}},"output_token_details":{"text_tokens":20,"audio_tokens":310}}}}"#;

    #[test]
    fn failed_reading_phase() {
        let radio = without_networks();
        let reader = Reader::start(Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(reader.fd());
        assert!(reader.is_off() && reader.latest() == Report::default());
        reader.set_reply(4, Script::for_reply("Say it.".into()));
        let reply = || Some(ReadingRequest { round: 4, source: ReadingSource::Reply, is_held: false, words: None });
        reader.want(reply());
        reader.want(reply());
        let mut phases = || {
            let mut phases: Vec<(u32, ReadingSource, Phase)> = Vec::new();
            while phases.last().is_none_or(|(.., phase)| *phase == Phase::Connecting) {
                assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7]);
                let Report { text, reply, .. } = reader.latest();
                phases.extend([text, reply].into_iter().flatten());
            }
            phases
        };
        let reply_phases = phases();
        assert!(
            reply_phases.iter().all(|(_, source, _)| *source == ReadingSource::Reply)
                && reply_phases.last() == Some(&(4, ReadingSource::Reply, Phase::Failed(Cause::SessionFailed))),
            "without a partition there is no key to ask the service with: the reply is told so as a reply's, and no screen of a text hears of it"
        );
        let news = ReadingSource::Text(Shelf::News);
        let passages = [String::from("A passage.")];
        let words = Some(Words::Arrived(&passages));
        let text = |is_held, words| Some(ReadingRequest { round: 4, source: news, is_held, words });
        reader.want(text(false, words));
        reader.want(text(false, Some(Words::Arrived(&[]))));
        assert_eq!(
            phases().last(),
            Some(&(4, news, Phase::Failed(Cause::SessionFailed))),
            "a text of the number a reply had is another reading, told under its number"
        );
        reader.want(None);
        while !reader.is_off() {
            thread::yield_now();
        }
        reader.want(text(true, Some(Words::Arrived(&[]))));
        thread::yield_now();
        assert_eq!(reader.latest(), Report::default(), "a reading that was held and has ended is not begun again");
    }

    fn passages() -> Vec<String> {
        vec!["First.".into(), "Second.".into()]
    }

    #[test]
    fn voice_lane_while_read() {
        let (mut reading, lane) = (Reading::new(passages(), false, sys::monotonic()), Mutex::new(Lane::default()));
        reading.mark_asked(sys::monotonic());
        let voice = Some(r#"{"type":"response.output_audio.delta","delta":"AAABAA=="}"#);
        assert_eq!(take_message(&mut reading, voice, Some(&lane)), MessageOutcome::Began);
        assert_eq!(take_message(&mut reading, voice, Some(&lane)), MessageOutcome::Nothing);
        assert_eq!(lock(&lane).pending_samples(), 8, "four samples at the service's rate are eight at the card's");
        assert_eq!(take_message(&mut reading, voice, None), MessageOutcome::Nothing);
        assert_eq!(lock(&lane).pending_samples(), 8, "a reading that is left plays none of it");
    }

    #[test]
    fn growing_reply_raises_stop() {
        let radio = without_networks();
        let (board, stop) = (Board::new(State::default()).unwrap(), Wake::new().unwrap());
        let account = Account::new(Data::unusable(), radio.online());
        let reader = Reader { shared: Arc::new(Shared { board, stop, account }) };
        let mut epoll = Epoll::with_fd(reader.shared.stop.fd());
        reader.set_reply(4, Script::for_reply("Say it.".into()));
        assert!(epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(), "nothing reads it");
        reader.want(Some(ReadingRequest { round: 4, source: ReadingSource::Reply, is_held: false, words: None }));
        reader.shared.stop.clear();
        assert!(epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(), "nothing has grown");
        reader.set_reply(4, Script { passages: passages(), ..Script::for_reply("Say it.".into()) });
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), vec![7], "the reading is woken for what is new");
    }

    fn serve_session<T: Send + 'static>(
        service: impl FnOnce(&mut Peer) -> T + Send + 'static,
    ) -> (ReadingSession, JoinHandle<T>) {
        let (socket, server) = peer(service);
        (ReadingSession::with_socket(socket, TASK), server)
    }

    #[test]
    fn left_passage_stopped_and_booked() {
        let (mut session, server) = serve_session(|peer| {
            peer.write_text(CREATED);
            let request = peer.read_text();
            peer.write_text(r#"{"type":"response.output_audio.delta","delta":"AAABAA=="}"#);
            peer.write_text(STOPPED);
            request
        });
        let mut reading = Reading::new(passages(), false, sys::monotonic());
        reading.mark_asked(sys::monotonic());
        let start = Instant::now();
        settle(&mut reading, &mut session, &up(), Duration::from_secs(10));
        assert_eq!(
            (reading.finish(1, Duration::ZERO, (0, 0), &SessionStats::default()).1, reading.is_in_flight()),
            (Some(6_388), false),
            "what the service made of the passage is booked, 6388 millionths of a dollar"
        );
        assert!(start.elapsed() < Duration::from_secs(5), "it did not wait for more than it was told");
        assert_eq!(server.join().unwrap().unwrap(), "{\"type\":\"response.cancel\",\"response_id\":\"resp_9\"}");
    }

    #[test]
    fn settle_bounded_by_limit_and_network() {
        let (mut session, server) = serve_session(|peer| peer.read_text());
        let mut reading = Reading::new(passages(), false, sys::monotonic());
        reading.mark_asked(sys::monotonic());
        let start = Instant::now();
        settle(&mut reading, &mut session, &up(), Duration::from_millis(150));
        assert!(
            start.elapsed() >= Duration::from_millis(150) && start.elapsed() < Duration::from_secs(5),
            "waited for what it was given and no longer: {:?}",
            start.elapsed()
        );
        assert_eq!(
            (reading.finish(1, Duration::ZERO, (0, 0), &SessionStats::default()).1, reading.is_in_flight()),
            (None, true),
            "a passage the service never counted is not booked"
        );
        let down = up();
        down.raise();
        let start = Instant::now();
        settle(&mut reading, &mut session, &down, Duration::from_secs(60));
        assert!(start.elapsed() < Duration::from_secs(5), "with the network gone nothing is waited for");
        let mut idle = Reading::new(passages(), false, sys::monotonic());
        settle(&mut idle, &mut session, &down, Duration::from_secs(60));
        assert_eq!(
            idle.finish(1, Duration::ZERO, (0, 0), &SessionStats::default()).1,
            None,
            "with no passage on its way there is nothing to settle"
        );
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn paid_passage_wakes_main_loop() {
        let radio = without_networks();
        let reader = Reader::start(Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(reader.fd());
        assert!(!reader.is_owing());
        reader.shared.set_owing(true);
        assert!(reader.is_owing());
        assert_eq!(epoll.wait(Some(Duration::ZERO)).unwrap(), Vec::<u64>::new(), "an owing raises nothing");
        reader.shared.set_owing(false);
        assert!(!reader.is_owing());
        assert_eq!(
            epoll.wait(Some(Duration::ZERO)).unwrap(),
            vec![7],
            "what is paid for wakes the loop, whose radio may rest now"
        );
        reader.latest();
        reader.shared.set_owing(false);
        assert_eq!(
            epoll.wait(Some(Duration::ZERO)).unwrap(),
            Vec::<u64>::new(),
            "and nothing more is told of what is not owed"
        );
    }
}
