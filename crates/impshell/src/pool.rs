use crate::sync;
use crate::sys::lock;
use crate::{log, sys};
use r1_funcore::base::crew::Queue;
use std::cell::OnceCell;
use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Condvar, Mutex};
use std::thread;

type Task = Box<dyn FnOnce() + Send>;

/// Threads that take whole jobs and stay for the next; a new one starts only while every one is busy.
pub struct ThreadPool {
    name: &'static str,
    queue: Mutex<Queue<Task>>,
    job_ready: Condvar,
}

impl ThreadPool {
    pub const fn new(name: &'static str) -> ThreadPool {
        ThreadPool { name, queue: Mutex::new(Queue::new()), job_ready: Condvar::new() }
    }

    /// Runs `job` on an idle thread or a new one; its handle gives the result.
    pub fn run<T: Send + 'static>(&'static self, job: impl FnOnce() -> T + Send + 'static) -> io::Result<JobHandle<T>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let job: Task = Box::new(move || {
            let _ = sender.send(job());
        });
        let mut queue = lock(&self.queue);
        if queue.add(job) {
            self.job_ready.notify_one();
        } else if let Err(e) = thread::Builder::new().name(self.name.into()).spawn(move || self.run_thread()) {
            queue.drop_last();
            return Err(e);
        }
        Ok(JobHandle { receiver, result: OnceCell::new() })
    }

    // A starter may be pinned or real-time, and a job may make its thread so, which the next job must not inherit.
    fn reset(&self) {
        if let Err(e) = sys::reset_scheduling() {
            log!("{}: the thread keeps the place it had: {e}", self.name);
        }
    }

    fn run_thread(&self) -> ! {
        self.reset();
        let mut queue = lock(&self.queue);
        loop {
            match queue.take() {
                Some(job) => {
                    drop(queue);
                    job();
                    self.reset();
                    queue = lock(&self.queue);
                }
                None => {
                    queue.rest();
                    queue = sync::wait(&self.job_ready, queue);
                    queue.wake();
                }
            }
        }
    }
}

/// The end of a pool's job.
pub struct JobHandle<T> {
    receiver: Receiver<T>,
    result: OnceCell<Option<T>>,
}

impl<T> JobHandle<T> {
    /// True once the job has returned or died.
    pub fn is_finished(&self) -> bool {
        if self.result.get().is_none() {
            let result = match self.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => None,
                Err(TryRecvError::Empty) => return false,
            };
            let _ = self.result.set(result);
        }
        true
    }

    /// Waits for the job's end; none if it died.
    pub fn join(self) -> Option<T> {
        let JobHandle { receiver, result } = self;
        result.into_inner().unwrap_or_else(|| receiver.recv().ok())
    }
}

/// Runs every job on a thread named `name`; each result in order: an error if its thread did not start, none if it died.
pub fn run_all<'a, T: Send + 'a>(
    name: &str,
    jobs: impl IntoIterator<Item = impl FnOnce() -> T + Send + 'a>,
) -> Vec<io::Result<Option<T>>> {
    thread::scope(|scope| {
        let threads: Vec<_> =
            jobs.into_iter().map(|job| thread::Builder::new().name(name.into()).spawn_scoped(scope, job)).collect();
        threads.into_iter().map(|thread| thread.map(|handle| handle.join().ok())).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::hw::CORES;

    #[test]
    fn pool_reuses_idle_threads() {
        static POOL: ThreadPool = ThreadPool::new("test");
        let is_idle = || lock(&POOL.queue).has_idle();
        let first = POOL.run(|| thread::current().id()).unwrap().join();
        while !is_idle() {
            thread::yield_now();
        }
        assert_eq!(POOL.run(|| thread::current().id()).unwrap().join(), first, "the thread that sleeps takes the next");
        let (go, waiting) = mpsc::channel::<()>();
        let blocked = POOL.run(move || waiting.recv().is_ok()).unwrap();
        let second = POOL.run(|| 7).unwrap();
        assert!(second.join() == Some(7) && !blocked.is_finished(), "a job is done while another is at work");
        go.send(()).unwrap();
        while !blocked.is_finished() {
            thread::yield_now();
        }
        assert_eq!(blocked.join(), Some(true), "what a job returned is kept once its end is seen");
        let failed = POOL.run(|| -> u32 { panic!("the job fails") }).unwrap();
        assert_eq!(failed.join(), None, "a job that died returned nothing");
    }

    fn core_count() -> libc::c_int {
        let mut cores = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
        unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut cores) };
        unsafe { libc::CPU_COUNT(&cores) }
    }

    #[test]
    fn pool_thread_unpinned() {
        static POOL: ThreadPool = ThreadPool::new("test");
        let free = thread::spawn(|| sys::pin_to_cores(&CORES).map(|()| core_count())).join().unwrap().unwrap();
        let pin = || sys::pin_to_cores(&[0]);
        let started = thread::spawn(move || {
            pin().unwrap();
            let job = move || (thread::current().id(), core_count(), pin().map(|()| core_count()));
            POOL.run(job).unwrap().join()
        });
        let (thread, count, pinned) = started.join().unwrap().unwrap();
        assert_eq!((count, pinned.unwrap()), (free, 1), "a thread one core's thread started, then kept on one itself");
        while !lock(&POOL.queue).has_idle() {
            thread::yield_now();
        }
        assert_eq!(
            POOL.run(|| (thread::current().id(), core_count())).unwrap().join(),
            Some((thread, free)),
            "the same thread takes the next job on any core, whatever started it and whatever its last job made of it"
        );
    }
}
