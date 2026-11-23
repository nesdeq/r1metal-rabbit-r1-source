use crate::sync;
use crate::sys::lock;
use crate::{log, sys};
use r1_funcore::base::crew::{Part, Progress, Roster, Row, Rows, Step, Wait, Wave, Work};
use std::hint;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::Instant;

type PartFn<'a> = dyn Fn(usize) + Sync + 'a;

#[derive(Clone, Copy)]
struct PartCall(*const PartFn<'static>);

// What a job points to is shared between threads by its nature, and outlives the job: the call that hands it over waits for every part.
unsafe impl Send for PartCall {}

impl PartCall {
    fn run(self, number: usize) {
        // The job's call has not returned, since this part of it is not done: what it points to is there.
        unsafe { (*self.0)(number) }
    }
}

struct Shared {
    state: Mutex<Roster<PartCall>>,
    job_ready: Vec<Condvar>,
    job_done: Condvar,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, Roster<PartCall>> {
        lock(&self.state)
    }

    fn run_parts(&self) {
        loop {
            let Some((part, number)) = self.state().claim() else {
                return;
            };
            let _guard = PartGuard(self);
            part.run(number);
        }
    }
}

struct PartGuard<'a>(&'a Shared);

impl Drop for PartGuard<'_> {
    fn drop(&mut self) {
        if self.0.state().end_part() {
            self.0.job_done.notify_all();
        }
    }
}

struct JobGuard<'a>(&'a Shared);

impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.end_job();
        drop(sync::wait_while(&self.0.job_done, state, |state| state.is_running()));
    }
}

fn run_thread(shared: &Shared, index: usize, core: Option<usize>) {
    if let Some(core) = core
        && let Err(e) = sys::pin_to_cores(&[core])
    {
        log!("crew: thread {index} is not kept on core {core}: {e}");
    }
    loop {
        let (part, number) = {
            let mut state = shared.state();
            loop {
                match state.next_step(index) {
                    Step::Run(part, number) => break (part, number),
                    Step::End => return,
                    Step::Sleep => state = sync::wait(&shared.job_ready[index], state),
                }
            }
        };
        let _guard = PartGuard(shared);
        part.run(number);
    }
}

// A part beside a wave, or one of its rows.
enum Piece<'a> {
    Part(Part<'a>),
    Row(Row<'a>),
}

// The row that waits on a row: whether it has parked, and its thread, kept as it first parks.
#[derive(Default)]
struct Sleeper {
    is_parked: AtomicBool,
    thread: OnceLock<Thread>,
}

impl Sleeper {
    fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}

// A wave's progress, and the row parked on each row; a row that notes takes no lock, and wakes the row below only where it parked.
struct Record {
    progress: Progress,
    sleepers: Vec<Sleeper>,
}

impl Record {
    // Parks `row` on the row above until place `place` is ready: the flag set before the progress is read again, the row above's count stored before the flag is read, so that one of the two sees the other.
    fn park(&self, row: usize, place: usize) {
        let Some(sleeper) = row.checked_sub(1).and_then(|above| self.sleepers.get(above)) else {
            return;
        };
        sleeper.thread.get_or_init(thread::current);
        sleeper.is_parked.store(true, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        while !self.progress.is_ready(row, place) {
            thread::park();
        }
        sleeper.is_parked.store(false, Ordering::Relaxed);
    }
}

impl Rows for Record {
    fn note(&self, row: usize, places: usize) {
        self.progress.note(row, places);
        fence(Ordering::SeqCst);
        if let Some(sleeper) = self.sleepers.get(row).filter(|sleeper| sleeper.is_parked.load(Ordering::Relaxed)) {
            sleeper.wake();
        }
    }

    fn wait(&self, row: usize, place: usize) {
        let begun = Instant::now();
        loop {
            match self.progress.next_wait(row, place, begun.elapsed()) {
                Wait::Ready => return,
                Wait::Spin => hint::spin_loop(),
                Wait::Park => return self.park(row, place),
            }
        }
    }
}

// Releases the rows that wait on a row that panics.
struct RowGuard<'a>(&'a Record);

impl Drop for RowGuard<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.progress.break_off();
            fence(Ordering::SeqCst);
            self.0.sleepers.iter().for_each(Sleeper::wake);
        }
    }
}

/// A crew of threads; dropping it ends them.
pub struct Crew {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    caller: Mutex<()>,
}

impl Crew {
    /// Starts `threads` unpinned threads, for tests.
    #[cfg(test)]
    pub fn new(name: &str, threads: usize) -> io::Result<Crew> {
        Crew::start(name, (0..threads).map(|_| None))
    }

    /// Starts a thread pinned to each of `cores`, a small job going to the first; if they fail, the caller runs every part.
    pub fn with_cores(name: &str, cores: &[usize]) -> Crew {
        Crew::start(name, cores.iter().copied().map(Some)).unwrap_or_else(|e| {
            log!("{name}: no threads beside its own: {e}");
            Crew::start(name, std::iter::empty()).expect("a crew without threads starts none")
        })
    }

    /// Keeps the calling thread on the first of `cores` and starts a thread pinned to each of the others: a call's picture is coded on the large cores, the camera's pictures made on every core.
    pub fn on_cores(name: &str, cores: &[usize]) -> Crew {
        if let Err(e) = sys::pin_to_cores(&cores[..1]) {
            log!("{name}: the calling thread is not kept on its core: {e}");
        }
        Crew::with_cores(name, &cores[1..])
    }

    fn start(name: &str, cores: impl ExactSizeIterator<Item = Option<usize>>) -> io::Result<Crew> {
        let shared = Arc::new(Shared {
            state: Mutex::new(Roster::new(cores.len())),
            job_ready: (0..cores.len()).map(|_| Condvar::new()).collect(),
            job_done: Condvar::new(),
        });
        let mut crew = Crew { shared, threads: Vec::new(), caller: Mutex::new(()) };
        for (index, core) in cores.enumerate() {
            let shared = Arc::clone(&crew.shared);
            let handle = thread::Builder::new().name(name.into()).spawn(move || run_thread(&shared, index, core))?;
            crew.threads.push(handle);
        }
        Ok(crew)
    }

    /// Runs a step's work: its parts at once, or its wave.
    pub fn run(&self, work: Work<'_>) {
        match work {
            Work::Parts(parts) => self.work(parts),
            Work::Wave(wave) => self.wave(wave),
        }
    }

    // Runs the parts beside a wave and its rows as `for_each` runs its items, each row noting and waiting through a progress of the wave's own.
    fn wave(&self, Wave { rows, places, lead, beside }: Wave<'_>) {
        let sleepers = rows.iter().map(|_| Sleeper::default()).collect();
        let record = Record { progress: Progress::new(rows.len(), places, lead), sleepers };
        let pieces = beside.into_iter().map(Piece::Part).chain(rows.into_iter().map(Piece::Row));
        let mut pieces: Vec<Option<Piece<'_>>> = pieces.map(Some).collect();
        self.for_each(&mut pieces, |_, piece| match piece.take() {
            Some(Piece::Part(part)) => part(),
            Some(Piece::Row(row)) => {
                let _guard = RowGuard(&record);
                row(&record);
            }
            None => {}
        });
    }

    /// Runs every part of a job as `for_each` runs its items.
    pub fn work<'a>(&self, parts: impl IntoIterator<Item = Part<'a>>) {
        let mut parts: Vec<Option<Part<'a>>> = parts.into_iter().map(Some).collect();
        self.for_each(&mut parts, |_, part| {
            if let Some(part) = part.take() {
                part();
            }
        });
    }

    /// Runs `work` on every item with its number, the first on the caller, the others on the crew; takes no memory.
    pub fn for_each<T: Send>(&self, items: &mut [T], work: impl Fn(usize, &mut T) + Sync) {
        struct Items<T>(*mut T);
        // Every number is taken once, so every item has one borrower at a time.
        unsafe impl<T: Send> Sync for Items<T> {}
        let (count, items) = (items.len(), Items(items.as_mut_ptr()));
        if count == 0 {
            return;
        }
        let part = |number: usize| {
            let items = &items;
            work(number, unsafe { &mut *items.0.add(number) })
        };
        let part: &PartFn<'_> = &part;
        let _caller_guard = lock(&self.caller);
        let _job_guard = JobGuard(&self.shared);
        let mut state = self.shared.state();
        // The parts are done or dropped before this call returns, which `JobGuard` sees to, so none outlives what the job points to.
        let call = PartCall(unsafe { std::mem::transmute::<&PartFn<'_>, &PartFn<'static>>(part) });
        for index in state.begin(call, count) {
            self.shared.job_ready[index].notify_one();
        }
        drop(state);
        part(0);
        self.shared.run_parts();
    }
}

impl Drop for Crew {
    fn drop(&mut self) {
        self.shared.state().close();
        self.shared.job_ready.iter().for_each(Condvar::notify_one);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_count;
    use r1_funcore::base::crew::Handoff;
    use std::collections::{HashMap, HashSet};

    fn add_one<'a>(numbers: &'a mut [u32], chunk: usize) -> impl Iterator<Item = Part<'a>> {
        numbers
            .chunks_mut(chunk)
            .map(|chunk| Box::new(move || chunk.iter_mut().for_each(|number| *number += 1)) as Part<'a>)
    }

    fn barrier_part<'a>((count, ready): (&'a Mutex<usize>, &'a Condvar), parts: usize) -> Part<'a> {
        Box::new(move || {
            let mut arrivals = count.lock().unwrap();
            *arrivals += 1;
            ready.notify_all();
            while *arrivals < parts {
                arrivals = ready.wait(arrivals).unwrap();
            }
        })
    }

    #[test]
    fn small_job_uses_first_threads() {
        let crew = Crew::new("test", 6).unwrap();
        while crew.shared.state().idle().len() < 6 {
            thread::yield_now();
        }
        let (workers, idle_lists) = (Mutex::new(HashSet::new()), Mutex::new(HashSet::new()));
        for _ in 0..50 {
            let (count, ready) = (Mutex::new(0), Condvar::new());
            let parts = (0..3).map(|_| {
                let barrier = barrier_part((&count, &ready), 3);
                let (workers, idle_lists, shared) = (&workers, &idle_lists, &crew.shared);
                Box::new(move || {
                    workers.lock().unwrap().insert(thread::current().id());
                    // No part ends before all three have begun: whoever sleeps now has no part of the job.
                    idle_lists.lock().unwrap().insert(shared.state().idle().to_vec());
                    barrier();
                }) as Part<'_>
            });
            crew.work(parts);
            // The threads that served are asleep again before the next job asks, as they are between two frames.
            while crew.shared.state().idle().len() < 6 {
                thread::yield_now();
            }
        }
        let (workers, idle_lists) = (workers.into_inner().unwrap(), idle_lists.into_inner().unwrap());
        assert_eq!(workers.len(), 3, "the caller and two of the six, the same two for all fifty jobs");
        assert_eq!(idle_lists, HashSet::from([vec![5, 4, 3, 2]]), "and those two are the crew's first");
    }

    #[test]
    fn caller_alone_and_drop_ends_threads() {
        let empty = Crew::new("test", 0).unwrap();
        let mut numbers = vec![0u32; 12];
        empty.work(add_one(&mut numbers, 5));
        assert_eq!(numbers, [1; 12]);
        let mut slices: Vec<&mut [u32]> = numbers.chunks_mut(5).collect();
        empty.for_each(&mut slices, |_, slice| slice.iter_mut().for_each(|number| *number += 1));
        assert_eq!(numbers, [2; 12]);
        let crew = Crew::new("test", 4).unwrap();
        let shared = Arc::clone(&crew.shared);
        crew.work(add_one(&mut numbers, 3));
        drop(crew);
        assert_eq!(
            (Arc::strong_count(&shared), numbers),
            (1, vec![3; 12]),
            "every thread has ended and let go of what it shared"
        );
    }

    #[test]
    fn for_each_takes_no_memory() {
        let crew = Crew::new("test", 3).unwrap();
        let mut numbers = vec![0u32; 1000];
        let mut slices: Vec<&mut [u32]> = numbers.chunks_mut(250).collect();
        let parts = slices.len();
        // What each thread had allocated when it last ended a part: a thread that allocates between two parts, or in one, shows.
        let last_counts = Mutex::new(HashMap::new());
        last_counts.lock().unwrap().reserve(64);
        let threads = Mutex::new(HashSet::new());
        threads.lock().unwrap().reserve(64);
        // The first round's parts wait for each other, so that each is done on a thread of its own: the caller's and the crew's three.
        let (count, ready) = (Mutex::new(0), Condvar::new());
        for round in 1..=200u32 {
            let start_count = alloc_count::allocations();
            crew.for_each(&mut slices, |number, slice| {
                let part_count = alloc_count::allocations();
                if round == 1 {
                    let mut arrivals = count.lock().unwrap();
                    *arrivals += 1;
                    ready.notify_all();
                    while *arrivals < parts {
                        arrivals = ready.wait(arrivals).unwrap();
                    }
                }
                slice.iter_mut().for_each(|value| *value += 1 + number as u32);
                let (thread, mut last_counts) = (thread::current().id(), last_counts.lock().unwrap());
                if round > 1
                    && let Some(&last) = last_counts.get(&thread)
                {
                    assert_eq!(part_count, last, "a thread of the crew allocated between two parts");
                }
                threads.lock().unwrap().insert(thread);
                last_counts.insert(thread, alloc_count::allocations());
            });
            assert_eq!(alloc_count::allocations(), start_count, "round {round}: the call allocated");
        }
        drop(slices);
        for (number, slice) in numbers.chunks(250).enumerate() {
            assert!(slice.iter().all(|&value| value == 200 * (1 + number as u32)), "part {number}");
        }
        assert_eq!(threads.into_inner().unwrap().len(), parts, "the caller and each thread of the crew took part");
        let mut empty: [u32; 0] = [];
        crew.for_each(&mut empty, |_, _| unreachable!("a job of nothing has no part"));
        let mut values = [0u32; 64];
        crew.for_each(&mut values, |number, value| *value = number as u32);
        assert!(values.iter().enumerate().all(|(number, &value)| value == number as u32), "more parts than threads");
    }

    #[test]
    fn two_callers() {
        let crew = Crew::new("test", 3).unwrap();
        thread::scope(|scope| {
            for step in [1u32, 2] {
                let crew = &crew;
                scope.spawn(move || {
                    let mut numbers = vec![0u32; 600];
                    for round in 1..=100 {
                        let mut slices: Vec<&mut [u32]> = numbers.chunks_mut(100).collect();
                        crew.for_each(&mut slices, |_, slice| slice.iter_mut().for_each(|number| *number += step));
                        crew.work(add_one(&mut numbers, 150));
                        assert!(numbers.iter().all(|&number| number == round * (step + 1)), "round {round}");
                    }
                });
            }
        });
    }

    // A wave of `rows` rows of `places`, each value the one above and after it plus the one before it plus one, which only rows run in the wave's order give, each row's values in a room of its own; and a part beside it.
    fn wave_of<'a>(
        values: &'a Handoff,
        beside: &'a mut u32,
        (rows, places, fail): (usize, usize, Option<usize>),
    ) -> Wave<'a> {
        let rows = (0..rows)
            .map(|row| {
                Box::new(move |progress: &dyn Rows| {
                    let room = Handoff::room(places);
                    let mut before = 0;
                    for place in 0..places {
                        progress.wait(row, place);
                        assert!(Some(row * places + place) != fail, "a row fails at a place");
                        let mut above = [0];
                        if row > 0 {
                            values.take((row - 1) * room + (place + 1).min(places - 1), &mut above);
                        }
                        before = above[0].wrapping_add(before).wrapping_add(1);
                        values.put(row * room + place, &[before]);
                        progress.note(row, place + 1);
                    }
                }) as Row<'a>
            })
            .collect();
        Wave { rows, places, lead: 2, beside: vec![Box::new(move || *beside += 1) as Part<'a>] }
    }

    #[test]
    fn wave_as_in_order() {
        let crew = Crew::new("test", 3).unwrap();
        let (rows, places) = (9, 20);
        let bytes = rows * Handoff::room(places);
        let (mut alone, mut crewed) = (Handoff::default(), Handoff::default());
        alone.resize(bytes);
        crewed.resize(bytes);
        let (mut beside_alone, mut beside_crewed) = (0, 0);
        Work::Wave(wave_of(&alone, &mut beside_alone, (rows, places, None))).in_order();
        for _ in 0..100 {
            crew.run(Work::Wave(wave_of(&crewed, &mut beside_crewed, (rows, places, None))));
        }
        let (mut expected, mut got) = (vec![0; bytes], vec![0; bytes]);
        alone.take(0, &mut expected);
        crewed.take(0, &mut got);
        assert_eq!(
            (&got, beside_crewed),
            (&expected, 100),
            "the rows on the crew as one after another, the part beside each time"
        );
        // Without a part beside it, the first row is the caller's.
        let mut failing = wave_of(&crewed, &mut beside_crewed, (rows, places, Some(3)));
        failing.beside.clear();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crew.run(Work::Wave(failing))));
        assert!(result.is_err(), "a row that fails on the caller fails the call, the rows waiting on it let go");
        crew.run(Work::Wave(wave_of(&crewed, &mut beside_crewed, (rows, places, None))));
        crewed.take(0, &mut got);
        assert_eq!(got, expected, "and the crew takes the next wave");
    }

    #[test]
    fn panic_drops_parts() {
        let crew = Crew::new("test", 0).unwrap();
        let mut numbers = [0u32; 4];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crew.for_each(&mut numbers, |number, value| {
                assert!(number > 0, "the caller's own part fails");
                *value = 1;
            });
        }));
        assert!(result.is_err());
        assert_eq!(numbers, [0; 4], "the parts no thread had begun were dropped with the call");
        crew.for_each(&mut numbers, |_, value| *value = 2);
        assert_eq!(numbers, [2; 4], "and the crew takes the next job");
    }
}
