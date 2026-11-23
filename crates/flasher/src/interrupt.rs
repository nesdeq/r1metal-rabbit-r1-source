//! Ctrl-C taken from its default for as long as what takes it is held: each one is heard on a thread of its own, which does what it was told for it.

use crate::Result;
use std::io::{self, Read};
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

/// What `r1` ends with when a Ctrl-C ends it, as shells have it: 128 and the signal's number.
pub const ENDED: i32 = 128 + libc::SIGINT;

type Heard = Box<dyn FnMut() + Send>;
// What a Ctrl-C does while it is taken.
static HEARD: Mutex<Option<Heard>> = Mutex::new(None);
// The pipe a Ctrl-C is written into once the thread that reads it runs, whichever thread the kernel hands the signal to.
static PIPE: AtomicI32 = AtomicI32::new(-1);

// One byte into the pipe, which is all a signal handler may safely do.
extern "C" fn written(_: libc::c_int) {
    let byte = 0u8;
    unsafe { libc::write(PIPE.load(Ordering::Relaxed), (&raw const byte).cast(), 1) };
}

/// Ctrl-C taken from its default: let go of, it gives Ctrl-C back, as `give_back` does where how that went is wanted.
#[must_use]
pub struct Taken(());

impl Taken {
    /// Gives Ctrl-C back its default: it ends `r1`.
    pub fn give_back(self) -> Result<()> {
        let given = given_back();
        std::mem::forget(self);
        given
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        let _ = given_back();
    }
}

/// Has every Ctrl-C from now on call `heard` instead of ending `r1`, until what is returned gives it back.
pub fn take(heard: impl FnMut() + Send + 'static) -> Result<Taken> {
    let mut held = HEARD.lock().unwrap_or_else(PoisonError::into_inner);
    *held = Some(Box::new(heard));
    if PIPE.load(Ordering::Relaxed) < 0 {
        let (mut reader, writer) = io::pipe()?;
        thread::Builder::new().name("interrupts".into()).spawn(move || {
            let mut byte = [0u8; 1];
            while reader.read_exact(&mut byte).is_ok() {
                if let Some(heard) = HEARD.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                    heard();
                }
            }
        })?;
        PIPE.store(writer.into_raw_fd(), Ordering::Relaxed);
    }
    drop(held);
    handle(written as extern "C" fn(libc::c_int) as libc::sighandler_t)?;
    Ok(Taken(()))
}

// Gives Ctrl-C back its default: it ends `r1`.
fn given_back() -> Result<()> {
    *HEARD.lock().unwrap_or_else(PoisonError::into_inner) = None;
    handle(libc::SIG_DFL)
}

// Has the kernel do that on a Ctrl-C, a read or write it cuts into carried on.
fn handle(action: libc::sighandler_t) -> Result<()> {
    let mut taken = unsafe { std::mem::zeroed::<libc::sigaction>() };
    taken.sa_sigaction = action;
    taken.sa_flags = libc::SA_RESTART;
    unsafe { libc::sigemptyset(&mut taken.sa_mask) };
    if unsafe { libc::sigaction(libc::SIGINT, &taken, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}
