mod data;
mod files;
pub mod logs;
pub mod nvram;
mod restore;
mod safe;
pub mod settings;
pub mod slots;
mod volume;

pub use data::{Data, InDirectory};

use crate::clock;
use crate::sync::Board;
use r1_funcore::base::file_names::{self, Names};
use r1_funcore::base::time::stamp::to_stamp;
use r1_funcore::storage::progress::{Progress, Report, State};
use r1_funcore::storage::safe::{Order, Refusal, Request};
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;

/// A name from the local time, with a count where `names` holds it.
pub fn stem_now(names: impl Names<Error = io::Error>) -> io::Result<String> {
    let stamp = to_stamp(clock::local());
    file_names::first_free(|count| file_names::numbered_stem(&stamp, count), names)
}

struct Shared {
    board: Board<State>,
    #[cfg(test)]
    scratch: std::sync::OnceLock<&'static std::path::Path>,
}

impl Shared {
    fn new() -> io::Result<Shared> {
        Ok(Shared {
            board: Board::new(State::default())?,
            #[cfg(test)]
            scratch: std::sync::OnceLock::new(),
        })
    }

    fn enter(&self, progress: Progress) {
        self.board.lock().enter(progress);
        self.board.notify();
    }

    fn report(&self, progress: Progress) {
        self.board.lock().report_progress(progress);
        self.board.raise();
        self.board.notify();
    }

    fn begin_encrypting(&self) {
        if self.board.lock().begin_encrypting() {
            self.board.raise();
            self.board.notify();
        }
    }

    fn report_change(&self, number: u32, outcome: Result<(), Refusal>) {
        self.board.lock().report.change = Some((number, outcome));
        self.board.raise();
    }

    fn await_device(&self) {
        self.board.wait(|state| mem::take(&mut state.has_new_device).then_some(()));
    }

    fn await_order(&self) -> (u32, Order) {
        self.board.wait(|state| state.order.take())
    }
}

#[cfg(test)]
pub(crate) fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("r1-storage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// The data partition and its thread.
pub struct Storage {
    shared: Arc<Shared>,
}

impl Storage {
    /// Starts bringing the partition up.
    pub fn start() -> io::Result<Storage> {
        let shared = Arc::new(Shared::new()?);
        let worker = Arc::clone(&shared);
        thread::Builder::new().name("storage".into()).spawn(move || volume::work(&worker))?;
        Ok(Storage { shared })
    }

    /// The whole partition, which the drive (`Data::export_root`) and the debug shell alone hold.
    pub fn data(&self) -> Data {
        Data::new(Arc::clone(&self.shared), None)
    }

    /// The partition narrowed to the directories of `scope`; any other is refused (`paths::check_scope`).
    pub fn within(&self, scope: &'static [&'static str]) -> Data {
        Data::new(Arc::clone(&self.shared), Some(scope))
    }

    /// Readable while a new step or a passphrase change's outcome waits.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes the thread's report.
    pub fn latest(&self) -> Report {
        self.shared.board.take(|state| mem::take(&mut state.report))
    }

    /// True while the thread does no work of its own that a sleep would hold up: it formats nothing and moves nothing under the safe's key.
    pub fn is_off(&self) -> bool {
        !self.shared.board.lock().progress().is_at_work()
    }

    /// Hands the safe the window's request, once per number.
    pub fn ask_safe(&self, request: Option<(u32, Request)>) {
        if self.shared.board.lock().ask_safe(request) {
            self.shared.board.notify();
        }
    }

    /// Notes a new block device.
    pub fn device_added(&self) {
        self.shared.board.lock().has_new_device = true;
        self.shared.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_added_once() {
        let storage = Storage { shared: Arc::new(Shared::new().unwrap()) };
        storage.device_added();
        storage.device_added();
        storage.shared.await_device();
        assert!(!storage.shared.board.lock().has_new_device, "devices that appeared are waited for once");
    }
}
