pub mod scribe;

use crate::media::recording::SoundFile;
use crate::sound::{self, SoundStream, VoiceRecording, record_voice};
use crate::storage::{Data, InDirectory, stem_now};
use crate::sync::Board;
use crate::sys;
use crate::{clock, log};
use r1_funcore::base::file_names::stem;
use r1_funcore::base::time::Moment;
use r1_funcore::codec::aac;
use r1_funcore::codec::mp4::NO_SOUND_TRACK;
use r1_funcore::domain::memos::state::{Report, State};
use r1_funcore::domain::memos::work::{Action, Work};
use r1_funcore::domain::memos::{self, FULL_LINE, Lengths, Request};
use r1_funcore::domain::paper::names::{self, directory, folder};
use r1_funcore::domain::shelf::Shelf;
use r1_funcore::domain::tally::Tally;
use r1_funcore::openai::ledger::SummaryKind;
use r1_funcore::storage::paths::Listed;
use r1_funcore::storage::{NOTES, SaveFault, has_room};
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;

fn sound_file(data: &Data, stem: &str) -> io::Result<SoundFile> {
    let file = data.open(&folder(SummaryKind::Note, stem), &names::sound_file(stem))?;
    SoundFile::open(file)?.ok_or_else(|| sys::invalid(NO_SOUND_TRACK.to_string()))
}

struct Shared {
    board: Board<State>,
    data: Data,
}

struct NoteRecording {
    stem: String,
    recording: VoiceRecording,
}

fn begin_note(shared: &Shared) -> Result<NoteRecording, SaveFault> {
    let data = &shared.data;
    let to_fault = |e: io::Error| {
        log!("notes: no recording: {e}");
        SaveFault::NotRecorded
    };
    if !has_room(data.free().map_err(to_fault)?) {
        log!("{FULL_LINE}");
        return Err(SaveFault::Full);
    }
    let stem = stem_now(InDirectory(data, NOTES)).map_err(to_fault)?;
    let (place, name) = (data.clone(), stem.clone());
    let create = move || place.create(&folder(SummaryKind::Note, &name), &name, names::SOUND_EXTENSION);
    let recording = record_voice(create).map_err(to_fault)?;
    log!("{}", memos::to_recording_line(&stem));
    Ok(NoteRecording { stem, recording })
}

fn end_note(note: NoteRecording) -> Result<String, SaveFault> {
    let NoteRecording { stem: name, recording } = note;
    match recording.end() {
        Ok(Some(file)) => {
            log!("{}", memos::to_recorded_line(&file.name, file.packets, file.bytes));
            return Ok(stem(&file.name).to_string());
        }
        Ok(None) => log!("{}", memos::to_unheard_line(&name)),
        Err(e) => log!("notes: {name}: {e}"),
    }
    Err(SaveFault::NotRecorded)
}

fn note_seconds(data: &Data, stem: &str) -> Option<u64> {
    sound_file(data, stem)
        .as_ref()
        .map(|file| aac::track_seconds(&file.track))
        .inspect_err(|e| log!("notes: {stem}: {e}"))
        .ok()
}

fn count(data: &Data, lengths: &mut Lengths) -> Tally {
    if !data.wait_settled() {
        return Tally::default();
    }
    let notes = data.names(directory(SummaryKind::Note), Listed::Folders);
    let notes = notes.inspect_err(|e| log!("notes: counting them: {e}"));
    let day = Moment::from_seconds(clock::local()).to_compact_date();
    let is_ready = memos::has_rundown(data, &day).inspect_err(|e| log!("notes: today's edition: {e}")).unwrap_or(false);
    memos::count_shelves(&notes.unwrap_or_default(), lengths, |stem| note_seconds(data, stem), is_ready)
}

fn delete(shared: &Shared, kind: SummaryKind, stem: &str) -> io::Result<()> {
    shared.data.discard(directory(kind), stem)?;
    log!("{}", memos::to_deleted_line(stem, kind));
    Ok(())
}

fn run_action(
    shared: &Arc<Shared>,
    work: &mut Work,
    action: Action,
    note: &mut Option<NoteRecording>,
    speaker: &mut Option<SoundStream>,
) {
    match action {
        Action::End => {
            if let Some(recording) = note.take() {
                work.note_end(end_note(recording));
            }
        }
        Action::Begin => {
            *speaker = None;
            work.note_start(begin_note(shared).map(|recording| note.insert(recording).stem.clone()));
        }
        Action::Stop => *speaker = None,
        Action::Play(number, stem) => {
            let worker = Arc::clone(shared);
            let on_end = move || {
                worker.board.lock().note_play_end(number);
                worker.board.notify();
            };
            match sound_file(&shared.data, &stem).and_then(|file| sound::play_file(file, on_end)) {
                Ok(stream) => *speaker = Some(stream),
                Err(e) => {
                    log!("notes: {stem} is not played: {e}");
                    *speaker = None;
                    work.fail_playing();
                }
            }
        }
        Action::Delete(kind, stem, should_stop) => {
            if should_stop {
                *speaker = None;
            }
            if let Err(e) = delete(shared, kind, &stem) {
                log!("notes: {stem} is not deleted: {e}");
                work.fail_deleting();
            }
        }
    }
}

fn work(shared: &Arc<Shared>) -> ! {
    let (mut note, mut speaker): (Option<NoteRecording>, Option<SoundStream>) = (None, None);
    let (mut work, mut lengths) = (Work::default(), Lengths::default());
    loop {
        let round = shared.board.wait(|state| state.take_round(&work));
        let (actions, requests) = work.begin_round(round);
        for action in actions {
            run_action(shared, &mut work, action, &mut note, &mut speaker);
        }
        for request in requests {
            let action = work.take_request(request);
            run_action(shared, &mut work, action, &mut note, &mut speaker);
        }
        let listing = work.take_relist().map(|shelf| {
            let recording = note.as_ref().map(|note| note.stem.as_str());
            let data = &shared.data;
            let memos = memos::list(shelf, data, recording, &mut lengths, |stem| note_seconds(data, stem));
            (shelf, memos.inspect_err(|e| log!("notes: listing them: {e}")).unwrap_or_default())
        });
        let tally = work.take_recount().then(|| count(&shared.data, &mut lengths));
        // A listing the screens have not taken is replaced, and goes outside the lock they take.
        let previous = shared.board.lock().add_report(work.take_report(listing, tally));
        drop(previous);
        shared.board.raise();
    }
}

/// The main loop's handle of the thread that keeps notes and conversations.
pub struct Memos {
    shared: Arc<Shared>,
}

impl Memos {
    /// Starts the thread, idle until the notes, the translations, the editions or home show, with its files on `data`.
    pub fn start(data: Data) -> io::Result<Memos> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, data });
        let worker = Arc::clone(&shared);
        sys::spawn_background("notes", move || work(&worker))?;
        Ok(Memos { shared })
    }

    /// Readable when there is something new for the screens.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets the shelf that shows, the change count of the files the screens know, and whether home shows on which day.
    pub fn want(&self, shelf: Option<Shelf>, files: u32, home: (bool, u64)) {
        if self.shared.board.lock().set_wanted(shelf, files, home) {
            self.shared.board.notify();
        }
    }

    /// Hands on the screens' requests.
    pub fn ask(&self, requests: Vec<Request>) {
        if self.shared.board.lock().add_requests(requests) {
            self.shared.board.notify();
        }
    }

    /// True while the thread holds neither microphone nor speaker and has done all it was asked.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// Takes the report for the screens.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use r1_funcore::domain::cause::Cause;
    use std::time::Duration;

    fn next_report(memos: &Memos, epoll: &mut Epoll) -> Report {
        assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the thread tells when it has done");
        memos.latest()
    }

    #[test]
    fn no_partition_empty_lists_and_causes() {
        let memos = Memos::start(Data::unusable()).unwrap();
        let mut epoll = Epoll::with_fd(memos.fd());
        assert!(memos.is_off() && memos.latest() == Report::default());
        memos.want(Some(Shelf::Notes), 0, (false, 0));
        assert_eq!(
            next_report(&memos, &mut epoll).listing,
            Some((Shelf::Notes, Vec::new())),
            "a list that cannot be read is empty"
        );
        memos.ask(vec![Request::Record]);
        let report = next_report(&memos, &mut epoll);
        assert_eq!((report.recording, report.cause), (None, Some(Cause::NotRecorded)), "no room to record into");
        assert!(memos.is_off());
        memos.ask(vec![
            Request::Play("20261012_190328".into()),
            Request::Delete(SummaryKind::Note, "20261012_190328".into()),
        ]);
        let report = next_report(&memos, &mut epoll);
        assert_eq!(
            (report.playing, report.cause, report.listing),
            (None, Some(Cause::NotDeleted), Some((Shelf::Notes, Vec::new()))),
            "the last thing that came to nothing is the one the screen is told of, and the list is read again"
        );
        memos.want(Some(Shelf::Talks), 0, (false, 0));
        assert_eq!(
            next_report(&memos, &mut epoll).listing,
            Some((Shelf::Talks, Vec::new())),
            "the other kind has its own list"
        );
        memos.want(Some(Shelf::Talks), 1, (false, 0));
        assert_eq!(
            next_report(&memos, &mut epoll).listing,
            Some((Shelf::Talks, Vec::new())),
            "read again when its files change"
        );
        memos.want(None, 1, (false, 0));
        next_report(&memos, &mut epoll);
        assert!(memos.is_off(), "with no screen showing the thread holds nothing");
    }
}
