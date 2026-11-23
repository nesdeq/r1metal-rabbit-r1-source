mod archive;
mod crashes;
mod folder;
mod scrub;
mod snapshot;
mod writer;

use crate::storage::Data;
use crate::sync::Board;
use crate::sys::{self, Wake, lock, try_lock};
use crate::{clock, log};
use r1_funcore::base::handoff::Latch;
use r1_funcore::logs::writer::Status;
use r1_funcore::logs::{LineFile, Logging, Queue, State, UNSETTLED_LINE, lines};
use std::cell::RefCell;
use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Instant;

std::thread_local! {
    static THREAD_QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
}

struct Shared {
    board: Board<State<PathBuf>>,
    wake: Wake,
    // Beside the board because a thread that draws hands a line over without ever waiting for it.
    is_on: AtomicBool,
    period: AtomicU64,
    overflow: Mutex<Queue>,
}

impl Shared {
    fn new() -> io::Result<Shared> {
        Ok(Shared {
            board: Board::new(State::default())?,
            wake: Wake::new()?,
            is_on: AtomicBool::new(false),
            period: AtomicU64::new(0),
            overflow: Mutex::new(Queue::default()),
        })
    }

    fn scrub(&self) -> io::Result<()> {
        let number = self.board.lock().ask_scrub();
        self.wake.raise();
        self.board.wait(|state| state.scrub_outcome(number)).map_err(io::Error::other)
    }

    fn scrub_due(&self) -> Option<u64> {
        self.board.lock().scrub_due()
    }

    fn end_scrub(&self, number: u64, failure: Option<String>) {
        self.board.lock().end_scrub(number, failure);
        self.board.notify();
    }

    fn begin_period(&self) {
        let period = self.period.fetch_add(1, Ordering::AcqRel) + 1;
        let (mut state, mut overflow) = (self.board.lock(), lock(&self.overflow));
        state.queue.begin(period);
        overflow.set_period(period);
        drop((state, overflow));
        self.is_on.store(true, Ordering::Release);
    }

    fn take_overflow(&self) -> MutexGuard<'_, State<PathBuf>> {
        let mut state = self.board.lock();
        state.queue.merge(&mut lock(&self.overflow));
        state
    }

    fn open_day(&self, day: &str) {
        self.board.wait(|state| state.days.try_open(day));
    }

    fn pack_past(&self, root: &Path, today: &str) {
        self.board.lock().days.pack_past(root.into(), today);
        self.board.notify();
    }

    fn await_due_day(&self) -> (PathBuf, String) {
        self.board.wait(|state| state.days.take_pack_job())
    }

    fn end_packing(&self) {
        self.board.lock().days.end_packing();
        self.board.notify();
    }
}

static WRITER: OnceLock<Arc<Shared>> = OnceLock::new();

fn now() -> (u64, u32) {
    lines::to_stamp_time(sys::realtime_ms(), clock::offset())
}

fn active_writer() -> Option<&'static Arc<Shared>> {
    WRITER.get().filter(|shared| shared.is_on.load(Ordering::Acquire))
}

/// True while logs are written, so a caller can skip costly text.
pub fn is_on() -> bool {
    active_writer().is_some()
}

fn hand_over(shared: &Shared, thread_queue: &mut Queue) {
    if let Some(mut state) = shared.board.try_lock() {
        if let Some(mut overflow) = try_lock(&shared.overflow) {
            state.queue.merge(&mut overflow);
        }
        state.queue.merge(thread_queue);
    } else if let Some(mut overflow) = try_lock(&shared.overflow) {
        overflow.merge(thread_queue);
    }
}

fn add_line_to(shared: &Shared, file: LineFile, text: &str) {
    let now = now();
    THREAD_QUEUE.with_borrow_mut(|thread_queue| {
        thread_queue.set_period(shared.period.load(Ordering::Acquire));
        lines::add_line(thread_queue.lines.of_mut(file), now, text);
        hand_over(shared, thread_queue);
    });
}

/// Queues a line for a day's file: a shown frame's record, an event of the touch screen, the wheel or the button, or a battery reading.
pub fn add_line(file: LineFile, text: &str) {
    match active_writer() {
        Some(shared) => add_line_to(shared, file, text),
        // Nothing is written while the logs are off, and what this thread held for them goes with the rest.
        None => THREAD_QUEUE.with_borrow_mut(Queue::clear),
    }
}

fn snapshot_to(shared: &Shared, reason: &str) {
    let record = lines::to_stamped(now(), &lines::note(reason));
    THREAD_QUEUE.with_borrow_mut(|thread_queue| {
        thread_queue.set_period(shared.period.load(Ordering::Acquire));
        thread_queue.snapshots.push(record);
        hand_over(shared, thread_queue);
    });
    shared.wake.raise();
}

/// Queues a system snapshot; `reason` says what for.
pub fn snapshot(reason: &str) {
    if let Some(shared) = active_writer() {
        snapshot_to(shared, reason);
    }
}

fn settle_on(shared: &Shared, deadline: Instant) -> bool {
    let mut state = shared.board.lock();
    THREAD_QUEUE.with_borrow_mut(|thread_queue| {
        if shared.is_on.load(Ordering::Relaxed) {
            state.queue.merge(thread_queue);
        } else {
            thread_queue.clear();
        }
    });
    let wait = state.queue.begin_wait();
    shared.wake.raise();
    let limit = deadline.saturating_duration_since(Instant::now());
    shared.board.wait_while_within(state, limit, |state| !state.queue.is_settled(wait))
}

/// Removes Signal's traces from the logs and the kernel's records, and waits for the outcome.
pub fn scrub() -> io::Result<()> {
    WRITER.get().map_or(Ok(()), |shared| shared.scrub())
}

/// Waits until every line so far is written, or until `deadline`.
pub fn settle(deadline: Instant) {
    if let Some(shared) = WRITER.get()
        && !settle_on(shared, deadline)
    {
        log!("{UNSETTLED_LINE}");
    }
}

/// The main loop's handle of the logs.
pub struct Logs {
    shared: Arc<Shared>,
    logging: RefCell<Latch<Option<Logging>>>,
}

impl Logs {
    fn new(shared: Arc<Shared>) -> Logs {
        Logs { shared, logging: RefCell::default() }
    }

    /// Starts the writing and packing threads; nothing is written before `want`.
    pub fn start(data: Data) -> io::Result<Logs> {
        let shared = Arc::new(Shared::new()?);
        let _ = WRITER.set(Arc::clone(&shared));
        let (worker, packer) = (Arc::clone(&shared), Arc::clone(&shared));
        thread::Builder::new().name("logs".into()).spawn(move || writer::work(&worker, &data))?;
        sys::spawn_background("logs pack", move || archive::work(&packer))?;
        Ok(Logs::new(shared))
    }

    /// Readable while a new status waits.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes the newest status.
    pub fn latest(&self) -> Option<Status> {
        self.shared.board.take(|state| state.report.take())
    }

    /// Takes the logging setting; tells the writer only of a change.
    pub fn want(&self, logging: Option<Logging>) {
        if let Some(logging) = self.logging.borrow_mut().change(logging) {
            self.shared.board.lock().logging = logging;
            self.shared.wake.raise();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn on_writer() -> Arc<Shared> {
        let shared = Arc::new(Shared::new().unwrap());
        shared.is_on.store(true, Ordering::Relaxed);
        shared
    }

    fn words(lines: &str) -> Vec<&str> {
        lines.lines().filter_map(|line| line.rsplit(' ').next()).collect()
    }

    fn add_on_thread(shared: &Arc<Shared>, file: LineFile, word: &'static str) {
        let shared = Arc::clone(shared);
        thread::spawn(move || add_line_to(&shared, file, word)).join().unwrap();
    }

    fn add_while_locked(shared: &Shared, file: LineFile, word: &str) {
        let guards = (shared.board.lock(), lock(&shared.overflow));
        add_line_to(shared, file, word);
        drop(guards);
    }

    #[test]
    fn draw_thread_never_waits() {
        let shared = on_writer();
        let guard = shared.board.lock();
        let (sender, receiver) = mpsc::channel();
        let draw_thread = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for word in ["one", "two", "three"] {
                    add_line_to(&shared, LineFile::Frames, word);
                }
                add_line_to(&shared, LineFile::Input, "touch");
                snapshot_to(&shared, "meanwhile");
                sender.send(()).unwrap();
            })
        };
        assert!(receiver.recv_timeout(TIMEOUT).is_ok(), "the thread that draws waited for the writer");
        draw_thread.join().unwrap();
        assert!(
            LineFile::ALL.iter().all(|file| guard.queue.lines.of(*file).is_empty()) && guard.queue.snapshots.is_empty(),
            "nothing comes through while the writer has the queue"
        );
        assert_eq!(
            words(lock(&shared.overflow).lines.of(LineFile::Frames)),
            ["one", "two", "three"],
            "the thread has ended, so only the overflow can hold what it could not give the queue"
        );
        drop(guard);
        add_on_thread(&shared, LineFile::Frames, "four");
        let state = shared.board.lock();
        assert_eq!(
            words(state.queue.lines.of(LineFile::Frames)),
            ["one", "two", "three", "four"],
            "another thread's hand-over takes it along: every line, in the order it was made"
        );
        assert_eq!((words(state.queue.lines.of(LineFile::Input)), state.queue.snapshots.len()), (vec!["touch"], 1));
        drop(state);
        let guard = shared.board.lock();
        add_on_thread(&shared, LineFile::Power, "again");
        drop(guard);
        assert_eq!(
            words(shared.take_overflow().queue.lines.of(LineFile::Power)),
            ["again"],
            "the writer's own pass takes in the overflow"
        );
        assert!(lock(&shared.overflow).lines.of(LineFile::Power).is_empty());
    }

    #[test]
    fn settle_takes_thread_queue() {
        let (on, off) = (on_writer(), Arc::new(Shared::new().unwrap()));
        add_while_locked(&on, LineFile::Power, "held");
        assert!(settle_on(&on, Instant::now() + TIMEOUT));
        assert_eq!(
            words(on.board.lock().queue.lines.of(LineFile::Power)),
            ["held"],
            "settling takes in what the thread held"
        );
        add_while_locked(&on, LineFile::Frames, "old");
        settle_on(&off, Instant::now() + TIMEOUT);
        settle_on(&on, Instant::now() + TIMEOUT);
        assert!(
            on.board.lock().queue.lines.of(LineFile::Frames).is_empty()
                && off.board.lock().queue.lines.of(LineFile::Frames).is_empty(),
            "what was held for logs that have gone off is let go with the rest"
        );
    }

    #[test]
    fn settle_deadline() {
        let shared = on_writer();
        shared.begin_period();
        let start = Instant::now();
        assert!(!settle_on(&shared, start + Duration::from_millis(20)), "nothing was written");
        assert!(start.elapsed() < TIMEOUT, "and the wait ended at its time");
    }

    #[test]
    fn thread_queue_keeps_period() {
        let shared = on_writer();
        add_while_locked(&shared, LineFile::Frames, "held");
        add_line_to(&shared, LineFile::Frames, "next");
        assert_eq!(
            words(shared.board.lock().queue.lines.of(LineFile::Frames)),
            ["held", "next"],
            "what a thread kept back goes in with the next it hands over"
        );
        add_while_locked(&shared, LineFile::Frames, "stale");
        shared.begin_period();
        add_line_to(&shared, LineFile::Frames, "fresh");
        assert_eq!(
            words(shared.board.lock().queue.lines.of(LineFile::Frames)),
            ["fresh"],
            "the logs went off and on between: what was held for the period before is dropped, and so is what had been queued"
        );
    }

    #[test]
    fn scrub_waits_and_reports() {
        let shared = Arc::new(Shared::new().unwrap());
        assert_eq!(shared.scrub_due(), None, "none asked");
        let asker = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.scrub().map_err(|e| e.to_string()))
        };
        while shared.scrub_due().is_none() {
            thread::yield_now();
        }
        assert_eq!(shared.scrub_due(), Some(1));
        assert!(!asker.is_finished(), "the asker waits");
        shared.end_scrub(1, Some("no room".into()));
        assert_eq!(asker.join().unwrap(), Err("no room".into()), "and hears why it failed");
        assert_eq!(shared.scrub_due(), None, "done");
        let asker = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.scrub().is_ok())
        };
        while shared.scrub_due().is_none() {
            thread::yield_now();
        }
        shared.end_scrub(2, None);
        assert!(asker.join().unwrap());
    }

    #[test]
    fn want_tells_changes() {
        let shared = on_writer();
        let handle = Logs::new(Arc::clone(&shared));
        handle.want(Some(Logging::On));
        assert_eq!(shared.board.lock().logging, Some(Logging::On));
        let guard = shared.board.lock();
        let (sender, receiver) = mpsc::channel();
        let asker = thread::spawn(move || {
            handle.want(Some(Logging::On));
            sender.send(()).unwrap();
        });
        assert!(
            receiver.recv_timeout(TIMEOUT).is_ok(),
            "the thread that draws waited for the writer to say the same again"
        );
        drop(guard);
        asker.join().unwrap();
    }
}
