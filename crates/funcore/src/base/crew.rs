//! Which thread does which job of a set of threads.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;

/// A part of a job, which a crew's thread may run beside the job's other parts; it may borrow from its caller, who waits until every part is done.
pub type Part<'a> = Box<dyn FnOnce() + Send + 'a>;

// Runs a job's parts one after another on the caller's thread.
pub(crate) fn in_order(parts: Vec<Part<'_>>) {
    parts.into_iter().for_each(|part| part());
}

/// A row of a wave: a part that takes its row's places in order, noting how far it has come and waiting for the row above through what it is handed.
pub type Row<'a> = Box<dyn FnOnce(&dyn Rows) + Send + 'a>;

/// What a row of a wave notes its progress to and waits through: the crew that runs the rows at once, or nothing where they run one after another; the one wait of funcore's beside `Hardware::pause`.
pub trait Rows: Sync {
    /// Notes that `row` has finished its first `places` places.
    fn note(&self, row: usize, places: usize);

    /// Returns once place `place` of `row` may be taken.
    fn wait(&self, row: usize, place: usize);
}

// Rows run one after another on one thread, where a row never waits: the row above has ended before it begins.
struct InOrder;

impl Rows for InOrder {
    fn note(&self, _: usize, _: usize) {}

    fn wait(&self, _: usize, _: usize) {}
}

/// Rows of places that run at once, each taking a place only once the row above has finished `lead` places past it or its whole row, and parts that run beside them and never wait.
pub struct Wave<'a> {
    pub rows: Vec<Row<'a>>,
    pub places: usize,
    pub lead: usize,
    pub beside: Vec<Part<'a>>,
}

/// A step's work: parts that may run at once, or a wave.
pub enum Work<'a> {
    Parts(Vec<Part<'a>>),
    Wave(Wave<'a>),
}

impl Work<'_> {
    /// Runs the work on the caller's thread: the parts, or a wave's parts beside it and then its rows in order.
    pub fn in_order(self) {
        match self {
            Work::Parts(parts) => in_order(parts),
            Work::Wave(wave) => {
                in_order(wave.beside);
                wave.rows.into_iter().for_each(|row| row(&InOrder));
            }
        }
    }
}

// Runs each part and each row of the work on a thread of its own, the last first, as a crew may run them in any order, each row waiting for the row above as a crew's do.
#[cfg(test)]
pub(crate) fn on_threads(work: Work<'_>) {
    extern crate std;
    struct Threaded(Progress);
    impl Rows for Threaded {
        fn note(&self, row: usize, places: usize) {
            self.0.note(row, places);
        }

        fn wait(&self, row: usize, place: usize) {
            while !self.0.is_ready(row, place) {
                std::thread::yield_now();
            }
        }
    }
    let (parts, Wave { rows, places, lead, .. }) = match work {
        Work::Parts(parts) => (parts, Wave { rows: Vec::new(), places: 0, lead: 0, beside: Vec::new() }),
        Work::Wave(Wave { rows, places, lead, beside }) => (beside, Wave { rows, places, lead, beside: Vec::new() }),
    };
    let threaded = Threaded(Progress::new(rows.len(), places, lead));
    std::thread::scope(|scope| {
        for row in rows.into_iter().rev() {
            let threaded = &threaded;
            scope.spawn(move || row(threaded));
        }
        for part in parts.into_iter().rev() {
            scope.spawn(part);
        }
    });
}

/// What a row waiting for a place does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    Ready,
    Spin,
    Park,
}

// How long a row spins before it parks, a row's work (the bench's code step on one large core takes 1.5 ms for the 30 rows of 640x480): a row above that runs notes within a macroblock or two, and one that has not in a row's work has lost its core.
const SPIN: Duration = Duration::from_micros(50);

// A row's count of places done, alone in a line of the A53's data cache (64 bytes, Cortex-A53 TRM), so that a row noting its own moves no line the rows beside it read.
#[derive(Default)]
#[repr(align(64))]
struct Done(AtomicUsize);

/// How far each row of a wave has come, which its rows note and read at once without a lock: when a place may be taken.
pub struct Progress {
    done: Vec<Done>,
    places: usize,
    lead: usize,
    is_broken: AtomicBool,
}

impl Progress {
    /// The progress of a wave of `rows` rows of `places` places, each `lead` places behind the row above.
    pub fn new(rows: usize, places: usize, lead: usize) -> Progress {
        let done = (0..rows).map(|_| Done::default()).collect();
        Progress { done, places, lead, is_broken: AtomicBool::new(false) }
    }

    /// Notes that `row` has finished its first `places` places: what it handed on before is seen by a row that finds a place ready by it.
    pub fn note(&self, row: usize, places: usize) {
        if let Some(done) = self.done.get(row) {
            done.0.store(places, Ordering::Release);
        }
    }

    /// Whether place `place` of `row` may be taken.
    pub fn is_ready(&self, row: usize, place: usize) -> bool {
        let reach = (place + self.lead).min(self.places);
        let above = row.checked_sub(1).and_then(|above| self.done.get(above));
        self.is_broken.load(Ordering::Acquire) || above.is_none_or(|done| done.0.load(Ordering::Acquire) >= reach)
    }

    /// What `row` waiting for place `place` does next, having waited `spun` so far.
    pub fn next_wait(&self, row: usize, place: usize, spun: Duration) -> Wait {
        match (self.is_ready(row, place), spun < SPIN) {
            (true, _) => Wait::Ready,
            (false, true) => Wait::Spin,
            (false, false) => Wait::Park,
        }
    }

    /// A row ended without finishing, as one that panics does: no row waits for another from then on.
    pub fn break_off(&self) {
        self.is_broken.store(true, Ordering::Release);
    }
}

// The bytes a handoff moves at once.
const WORD: usize = size_of::<u64>();

/// Bytes the rows of a wave hand on to the rows below them, each put before its row notes it and taken after the row below found its place ready, so that the progress orders them; a word at a time, each word written by one row at a time (`room`).
#[derive(Default)]
pub struct Handoff {
    words: Vec<AtomicU64>,
}

impl Handoff {
    /// The bytes a run of `bytes` takes so that the run after it shares no word with it.
    pub const fn room(bytes: usize) -> usize {
        bytes.next_multiple_of(WORD)
    }

    /// Holds `count` bytes from now on, each nought.
    pub fn resize(&mut self, count: usize) {
        self.words.clear();
        self.words.resize_with(count.div_ceil(WORD), AtomicU64::default);
    }

    // Visits the words from byte `at` on that hold `count` bytes: each word, the first byte of it the visit takes, and where in the run that byte stands.
    fn visit(&self, at: usize, count: usize, mut visit: impl FnMut(&AtomicU64, usize, usize)) {
        let mut done = 0;
        while done < count {
            let (word, offset) = ((at + done) / WORD, (at + done) % WORD);
            visit(&self.words[word], offset, done);
            done += WORD - offset;
        }
    }

    /// Puts `bytes` from place `at` on.
    pub fn put(&self, at: usize, bytes: &[u8]) {
        self.visit(at, bytes.len(), |cell, offset, done| {
            let count = (WORD - offset).min(bytes.len() - done);
            let mut word = if count == WORD { [0; WORD] } else { cell.load(Ordering::Relaxed).to_le_bytes() };
            word[offset..offset + count].copy_from_slice(&bytes[done..done + count]);
            cell.store(u64::from_le_bytes(word), Ordering::Relaxed);
        });
    }

    /// Takes the bytes from place `at` on into `out`.
    pub fn take(&self, at: usize, out: &mut [u8]) {
        let length = out.len();
        self.visit(at, length, |cell, offset, done| {
            let count = (WORD - offset).min(length - done);
            out[done..done + count]
                .copy_from_slice(&cell.load(Ordering::Relaxed).to_le_bytes()[offset..offset + count]);
        });
    }
}

/// What a thread of a crew does next, `P` what calls a part.
pub enum Step<P> {
    Run(P, usize),
    Sleep,
    End,
}

struct Job<P> {
    part: P,
    count: usize,
    next: usize,
}

/// A crew's threads and the job they work on, `P` what calls a part.
pub struct Roster<P> {
    job: Option<Job<P>>,
    running: usize,
    is_closed: bool,
    idle: Vec<usize>,
}

impl<P: Copy> Roster<P> {
    /// The roster of that many threads, which takes no memory from then on.
    pub fn new(threads: usize) -> Roster<P> {
        Roster { job: None, running: 0, is_closed: false, idle: Vec::with_capacity(threads) }
    }

    /// The next part of the job for whoever helps with it, with its number.
    pub fn claim(&mut self) -> Option<(P, usize)> {
        let job = self.job.as_mut().filter(|job| job.next < job.count)?;
        job.next += 1;
        self.running += 1;
        Some((job.part, job.next - 1))
    }

    /// What the crew's thread of that number does next.
    pub fn next_step(&mut self, thread: usize) -> Step<P> {
        if let Some((part, number)) = self.claim() {
            self.idle.retain(|idle| *idle != thread);
            return Step::Run(part, number);
        }
        if self.is_closed {
            return Step::End;
        }
        if !self.idle.contains(&thread) {
            let place = self.idle.partition_point(|idle| *idle > thread);
            self.idle.insert(place, thread);
        }
        Step::Sleep
    }

    /// A part is done; true where none runs any more.
    pub const fn end_part(&mut self) -> bool {
        self.running -= 1;
        self.running == 0
    }

    /// No more parts of the job are claimed.
    pub fn end_job(&mut self) {
        self.job = None;
    }

    /// True while a part runs.
    pub const fn is_running(&self) -> bool {
        self.running > 0
    }

    /// Begins a job of `count` parts, the first of them the caller's: the threads to wake for the others while one sleeps, the crew's first ones first.
    pub fn begin(&mut self, part: P, count: usize) -> impl Iterator<Item = usize> {
        self.job = Some(Job { part, count, next: 1 });
        (1..count).map_while(|_| self.idle.pop())
    }

    /// Ends the threads once they have no part.
    pub const fn close(&mut self) {
        self.is_closed = true;
    }

    /// The threads that sleep, the next to be woken last.
    pub fn idle(&self) -> &[usize] {
        &self.idle
    }
}

/// Whole jobs that wait for a pool's threads, and how many of them sleep.
pub struct Queue<J> {
    jobs: VecDeque<J>,
    idle: usize,
}

impl<J> Queue<J> {
    pub const fn new() -> Queue<J> {
        Queue { jobs: VecDeque::new(), idle: 0 }
    }

    /// Takes a job; true where a thread that sleeps takes it, else one more is started.
    pub fn add(&mut self, job: J) -> bool {
        self.jobs.push_back(job);
        self.jobs.len() <= self.idle
    }

    /// Gives up the job added last, which no thread could be started for.
    pub fn drop_last(&mut self) -> Option<J> {
        self.jobs.pop_back()
    }

    /// The next job.
    pub fn take(&mut self) -> Option<J> {
        self.jobs.pop_front()
    }

    /// A thread goes to sleep.
    pub const fn rest(&mut self) {
        self.idle += 1;
    }

    /// A thread woke.
    pub const fn wake(&mut self) {
        self.idle -= 1;
    }

    /// True while a thread sleeps.
    pub const fn has_idle(&self) -> bool {
        self.idle > 0
    }
}

impl<J> Default for Queue<J> {
    fn default() -> Queue<J> {
        Queue::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_parts_and_idle() {
        let mut roster = Roster::new(3);
        assert!(roster.claim().is_none(), "no job, no part");
        for thread in [0, 2, 1, 2] {
            assert!(matches!(roster.next_step(thread), Step::Sleep));
        }
        assert_eq!(roster.idle(), [2, 1, 0], "each thread once, the next to be woken last");
        assert_eq!(roster.begin('p', 3).collect::<Vec<_>>(), [0, 1], "a thread for each part but the caller's");
        assert_eq!(roster.idle(), [2]);
        assert!(matches!(roster.next_step(0), Step::Run('p', 1)) && matches!(roster.next_step(1), Step::Run('p', 2)));
        assert!(roster.claim().is_none() && matches!(roster.next_step(2), Step::Sleep), "every part is claimed");
        assert!(roster.is_running() && !roster.end_part() && roster.end_part() && !roster.is_running());
        roster.end_job();
        assert!(matches!(roster.next_step(0), Step::Sleep));
        assert_eq!(roster.idle(), [2, 0]);
        assert_eq!(roster.begin('q', 1).count(), 0, "a job of one part is the caller's alone");
        assert!(roster.claim().is_none(), "whose first part is not handed out again");
        assert_eq!(roster.begin('r', 2).collect::<Vec<_>>(), [0]);
        assert_eq!(roster.claim(), Some(('r', 1)), "whoever helps takes a part by its number");
        roster.end_job();
        roster.close();
        assert!(matches!(roster.next_step(1), Step::End), "closed, a thread without a part ends");
    }

    #[test]
    fn progress_by_lead() {
        let progress = Progress::new(3, 5, 2);
        assert!(progress.is_ready(0, 4), "the first row waits for none");
        assert!(!progress.is_ready(1, 0), "a row waits for the row above to be its lead ahead");
        progress.note(0, 2);
        assert!(progress.is_ready(1, 0) && !progress.is_ready(1, 1), "two places done above: the first below");
        progress.note(0, 4);
        assert!(
            progress.is_ready(1, 2) && !progress.is_ready(1, 3),
            "a place its lead from the end waits for the whole row"
        );
        progress.note(0, 5);
        assert!(progress.is_ready(1, 4) && !progress.is_ready(2, 0), "and the row after waits for its own above");
        let waits = [Duration::ZERO, SPIN].map(|spun| progress.next_wait(2, 0, spun));
        assert_eq!(waits, [Wait::Spin, Wait::Park], "a row spins a row's work, then parks");
        assert_eq!(progress.next_wait(1, 4, SPIN), Wait::Ready, "a place ready is taken however long the wait");
        progress.break_off();
        assert!(progress.is_ready(2, 4), "a row that ended unfinished keeps none waiting");
    }

    #[test]
    fn handoff_and_work_in_order() {
        let mut handoff = Handoff::default();
        handoff.resize(Handoff::room(20));
        let run: Vec<u8> = (1..=13).collect();
        handoff.put(3, &run);
        let mut taken = [0; 20];
        handoff.take(0, &mut taken);
        assert_eq!(taken[..3], [0; 3], "the bytes before the run nought");
        assert_eq!((&taken[3..16], &taken[16..]), (&run[..], &[0; 4][..]), "a run across words where it was put");
        let mut middle = [0; 5];
        handoff.take(9, &mut middle);
        assert_eq!(middle, [7, 8, 9, 10, 11], "taken from inside a word");
        assert_eq!(Handoff::room(13), 16, "a run's room a whole number of words");
        let next = core::sync::atomic::AtomicUsize::new(0);
        let mark = |number: u8| handoff.put(next.fetch_add(1, Ordering::Relaxed), &[number]);
        let row = |number: u8| {
            Box::new(move |rows: &dyn Rows| {
                rows.wait(usize::from(number), 0);
                mark(number);
                rows.note(usize::from(number), 1);
            }) as Row<'_>
        };
        let beside = Box::new(move || mark(9)) as Part<'_>;
        Work::Wave(Wave { rows: alloc::vec![row(0), row(1)], places: 1, lead: 1, beside: alloc::vec![beside] })
            .in_order();
        handoff.take(0, &mut taken[..3]);
        assert_eq!(taken[..3], [9, 0, 1], "the parts beside first, then the rows one after another");
    }

    #[test]
    fn queue_wakes_or_starts() {
        let mut queue = Queue::new();
        assert!(!queue.add(1), "no thread sleeps: one more is started");
        assert_eq!(queue.drop_last(), Some(1), "which a job no thread could be started for is taken back from");
        queue.rest();
        assert!(queue.has_idle() && queue.add(2), "a thread that sleeps takes it");
        assert!(!queue.add(3), "a second job finds none left");
        queue.wake();
        assert!(!queue.has_idle());
        assert_eq!([queue.take(), queue.take(), queue.take()], [Some(2), Some(3), None], "in the order they came");
    }
}
