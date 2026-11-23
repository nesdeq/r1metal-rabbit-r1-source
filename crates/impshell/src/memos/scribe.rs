use crate::backlog::Worker;
use crate::media::recording::SoundFile;
use crate::openai::Account;
use crate::openai::{transcript, write};
use crate::storage::Data;
use crate::sync::Board;
use crate::sys;
use crate::wifi::radio::NetworkDown;
use crate::{clock, log};
use r1_funcore::codec::aac;
use r1_funcore::domain::scribe::state::{Model, Report, State};
use r1_funcore::domain::scribe::{self, Desk, Entry, Extent};
use r1_funcore::gfx::palette::Roles;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::ledger::SummaryKind;
use r1_funcore::openai::summary::Summary;
use r1_funcore::openai::transcript::Subject;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Instant;

struct Shared {
    board: Board<State>,
    data: Data,
    account: Account,
}

struct EntryDesk<'a, 'b> {
    shared: &'a Shared,
    worker: &'a mut Worker<'b, State>,
    entry: &'a Entry,
}

impl Desk for EntryDesk<'_, '_> {
    type Sound = SoundFile;
    type Stay = NetworkDown;

    fn open_sound(&mut self, stem: &str) -> Result<(SoundFile, Extent), Fault> {
        let sound = super::sound_file(&self.shared.data, stem)?;
        let track = &sound.track;
        let extent = Extent {
            packets: track.packets.len(),
            rate: track.sound.rate,
            bytes: sound.length,
            seconds: aac::track_seconds(track),
        };
        Ok((sound, extent))
    }

    fn hold(&mut self) -> Result<NetworkDown, Fault> {
        self.worker.ask(|slot| self.shared.account.keep_hold(slot))
    }

    fn transcribe_whole(&mut self, sound: &SoundFile, down: &NetworkDown, seconds: u64) -> Result<String, Fault> {
        let recording = (&sound.read_all()?[..], seconds);
        transcript::transcribe(&self.shared.account, down, recording, (Subject::Note, ""), None)
    }

    fn transcribe_piece(
        &mut self,
        sound: &SoundFile,
        down: &NetworkDown,
        first: usize,
        count: usize,
    ) -> Result<String, Fault> {
        let seconds = aac::cut_seconds(&sound.track, first, count);
        let piece = sound.cut(first, count, sys::realtime_seconds())?;
        transcript::transcribe(&self.shared.account, down, (&piece, seconds), (Subject::Note, ""), None)
    }

    fn book_note(&mut self) {
        transcript::book_note(&self.shared.account);
    }

    fn note_written(&mut self) {
        self.shared.board.lock().report.has_changed = true;
        self.shared.board.raise();
    }

    fn set_model(&mut self, model: Model) {
        self.shared.board.lock().backlog.set_busy(self.entry, model);
        self.shared.board.raise();
    }

    fn summarize(&mut self, kind: SummaryKind, words: &str) -> Result<Summary, Fault> {
        self.worker.ask_last(|slot| write::write_summary(&self.shared.account, slot, kind, words))
    }

    fn to_page_marks(&mut self) -> ((u64, i16), Roles) {
        ((clock::local(), clock::offset()), self.shared.board.lock().roles)
    }
}

impl Shared {
    fn take_entry(&self, worker: &mut Worker<'_, State>) -> (Entry, bool) {
        let scan = || {
            let recording = self.board.lock().recording.clone();
            scribe::scan(&self.data, recording.as_deref())
                .inspect_err(|e| log!("scribe: looking for what is not written yet: {e}"))
                .unwrap_or_default()
        };
        worker.take(scan, || Model::Writing)
    }

    fn run_entry(&self, worker: &mut Worker<'_, State>, entry: &Entry) -> Result<bool, Fault> {
        let start = Instant::now();
        let result = scribe::write_entry(&self.data, &mut EntryDesk { shared: self, worker, entry }, entry);
        if let Some(line) = scribe::to_log_line(&entry.1, &result, start.elapsed().as_millis()) {
            log!("{line}");
        }
        result
    }
}

fn run(shared: &Shared) -> ! {
    let mut worker = Worker::new(&shared.board);
    loop {
        let (entry, is_new) = shared.take_entry(&mut worker);
        let result = shared.run_entry(&mut worker, &entry);
        shared.board.lock().take_outcome(&entry, &result, is_new);
        shared.board.raise();
        shared.board.notify();
    }
}

/// The main loop's handle of the scribe's thread.
pub struct Scribe(Arc<Shared>);

impl Scribe {
    /// Starts the thread, idle until an entry needs writing, with entries on `data` and the model services through `account`.
    pub fn start(data: Data, account: Account) -> io::Result<Scribe> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, data, account });
        let worker = Arc::clone(&shared);
        sys::spawn_background("scribe", move || run(&worker))?;
        Ok(Scribe(shared))
    }

    /// Readable when the work on an entry moves on or ends.
    pub fn fd(&self) -> RawFd {
        self.0.board.fd()
    }

    /// Queues a new entry to be written at once.
    pub fn add_new(&self, kind: SummaryKind, stem: String) {
        self.0.board.lock().backlog.add_new((kind, stem));
        self.0.board.notify();
    }

    /// Drops a deleted entry.
    pub fn forget(&self, kind: SummaryKind, stem: &str) {
        self.0.board.lock().backlog.forget(&(kind, stem.to_string()));
    }

    /// Sets the stem of the note being recorded.
    pub fn set_recording(&self, stem: Option<String>) {
        self.0.board.lock().recording = stem;
    }

    /// Sets whether a notes or translator screen shows; one that opens retries every entry that lacks something.
    pub fn want(&self, is_wanted: bool) {
        if self.0.board.lock().backlog.set_wanted(is_wanted) {
            self.0.board.notify();
        }
    }

    /// Sets the colours of a summary's pages.
    pub fn set_roles(&self, roles: Roles) {
        self.0.board.lock().roles = roles;
    }

    /// Takes the report for the screen.
    pub fn latest(&self) -> Report {
        self.0.board.take(State::take_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::cause::Cause;
    use std::time::Duration;

    #[test]
    fn no_partition_no_write_no_hang() {
        let radio = without_networks();
        let scribe = Scribe::start(Data::unusable(), Account::new(Data::unusable(), radio.online())).unwrap();
        let mut epoll = Epoll::with_fd(scribe.fd());
        scribe.want(true);
        scribe.add_new(SummaryKind::Talk, "20261012_190328".into());
        let mut cause = None;
        while cause.is_none() {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7]);
            cause = scribe.latest().cause;
        }
        assert_eq!(
            cause,
            Some(Cause::NotWritten),
            "a conversation that cannot be read is not summed up, and the screen is told"
        );
        assert_eq!(scribe.latest(), Report::default());
    }
}
