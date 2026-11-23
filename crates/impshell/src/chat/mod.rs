mod files;
mod read;
mod recollect;
mod turn;

use crate::openai::Account;
use crate::storage::Data;
use crate::voiced::{Part, Shared};
use crate::{clock, crypto, log, sys};
use r1_funcore::base::time::Moment;
use r1_funcore::domain::camera::Shot;
use r1_funcore::domain::chat::onboarding::SEED_BYTES;
use r1_funcore::domain::chat::state::{Chore, Lapse, Load, Milestone, Store};
use r1_funcore::domain::chat::thread::{self, RobotState};
use r1_funcore::domain::chat::tools::Act;
use r1_funcore::domain::chat::transcript::{self, Entry};
use r1_funcore::domain::chat::{
    NEARLY_FULL_LINE, NO_PARTITION_LINE, WAKE_LINE, previous_day, start_day, to_day_entry, to_fact_entries,
    to_forgotten_line, to_listed_days,
};
use r1_funcore::domain::settings::Settings;
use r1_funcore::domain::shelf::Shelf;
use r1_funcore::domain::signal::robot::Reply;
use r1_funcore::domain::voice::{Phase, Voice};
use r1_funcore::storage::{SaveFault, has_room};
use std::io;
use std::sync::Arc;

/// The chat job's worker, with the partition of its files.
pub struct Robot {
    data: Data,
}

/// The main loop's handle of the chat job.
pub type Chat = Part<Robot>;

impl Robot {
    fn read_files(&self) -> Option<Store> {
        if !self.data.wait_decided() {
            log!("{NO_PARTITION_LINE}");
            return None;
        }
        match files::read(&self.data) {
            Ok(store) => {
                log!("{}", store.to_read_line());
                Some(store)
            }
            Err(e) => {
                log!("chat: what the conversation keeps cannot be read, and nothing of it is written: {e}");
                None
            }
        }
    }

    fn make_state(&self) -> Option<RobotState> {
        let state = crypto::random::<SEED_BYTES>().map(RobotState::new);
        state.iter().for_each(|state| files::write_state(&self.data, state));
        state
    }

    fn check_ready(shared: &Shared<Robot>, is_recording: bool) -> Option<SaveFault> {
        if !shared.is_ready() {
            return Some(SaveFault::NoStorage);
        }
        if !is_recording {
            return None;
        }
        match shared.job.data.free() {
            Ok(free) if has_room(free) => None,
            Ok(_) => {
                log!("{NEARLY_FULL_LINE}");
                Some(SaveFault::Full)
            }
            Err(e) => {
                log!("chat: no recording: {e}");
                Some(SaveFault::NotRecorded)
            }
        }
    }

    fn run_chore(shared: &Shared<Robot>, chore: Chore) {
        match chore {
            Chore::Open => shared.open(),
            Chore::Hush => {
                shared.drop_photo();
                shared.hush();
            }
            Chore::Mute(is_muted) => shared.mute(is_muted),
            Chore::PreviousDay(day) => shared.show_previous_day(&day),
            Chore::List(shelf) => shared.list(shelf),
            Chore::Forget(lapse) => shared.forget(&lapse),
            Chore::Show(shot) => shared.show_photo(shot),
        }
    }
}

impl Shared<Robot> {
    fn read_store<R>(&self, read: impl FnMut(&Store) -> R) -> R {
        self.board.read_loan(|state| &state.job.store, read)
    }

    fn change_store<R>(&self, change: impl FnOnce(&mut Store) -> R) -> R {
        self.board.change_loan(|state| &mut state.job.store, change)
    }

    fn is_ready(&self) -> bool {
        let is_loaded = match self.board.wait(|state| state.job.next_load()) {
            Load::Read => {
                let store = self.job.read_files();
                let is_loaded = store.is_some();
                self.board.lock().job.end_load(store);
                self.board.notify();
                is_loaded
            }
            Load::Done(is_loaded) => is_loaded,
        };
        if is_loaded {
            self.change_store(|store| {
                if store.lacks_robot() {
                    store.robot = self.job.make_state();
                }
            });
        }
        is_loaded
    }

    fn set_idle(&self) {
        self.tell(|state| state.set_idle());
    }

    fn set_thinking(&self) {
        self.tell(|state| state.set_thinking());
    }

    fn set_current(&self, entry: Option<Entry>) {
        self.board.lock().job.report.current = Some(entry);
        self.board.raise();
    }

    fn show_day(&self, day: String, entries: Vec<Entry>) {
        let previous = self.board.lock().job.show_day(day, entries);
        drop(previous);
        self.board.raise();
    }

    fn add_milestone(&self, milestone: Milestone) {
        self.board.lock().job.report.milestones.push(milestone);
        self.board.raise();
    }

    fn add_acts(&self, acts: Vec<Act>) {
        self.tell(|state| state.job.add_acts(acts));
    }

    fn post_identity(&self) {
        let identity = self.read_store(Store::identity);
        self.board.lock().job.report.identity = Some(identity);
        self.board.raise();
    }

    fn set_recalling(&self, is_recalling: bool) -> bool {
        let is_set = self.board.lock().set_recalling(is_recalling);
        if is_set {
            self.wake_all();
        }
        is_set
    }

    fn hush(&self) {
        self.tell(Voice::hush);
    }

    fn show_photo(&self, shot: Shot) {
        let previous = self.board.lock().job.show_photo(shot);
        drop(previous);
        self.board.raise();
    }

    fn drop_photo(&self) {
        let photo = self.board.lock().job.drop_photo();
        if photo.is_some() {
            self.board.raise();
        }
    }

    fn open(&self) {
        if !self.is_ready() {
            return self.fail_saving(SaveFault::NoStorage);
        }
        let today = Moment::from_seconds(clock::local()).to_compact_date();
        let days = files::talk_days(&self.job.data);
        if let Some(day) = start_day(&days, &today) {
            let entries = files::entries(&self.job.data, day);
            self.show_day(day.clone(), entries);
        }
        let is_never_held = self.read_store(|store| thread::is_never_held(store.robot.as_ref(), &store.thread));
        self.post_identity();
        let is_waking = self.board.lock().open(is_never_held);
        if is_waking {
            log!("{WAKE_LINE}");
        }
        self.wake_all();
    }

    fn mute(&self, is_muted: bool) {
        if !self.is_ready() {
            return;
        }
        self.tell(|state| state.job.mute(is_muted));
        self.change_store(|store| {
            if let Some(state) = store.robot.as_mut() {
                state.is_muted = is_muted;
                files::write_state(&self.job.data, state);
            }
        });
        self.post_identity();
    }

    fn show_previous_day(&self, day: &str) {
        match previous_day(files::talk_days(&self.job.data), day) {
            Some(previous) => {
                let entries = files::entries(&self.job.data, &previous);
                self.show_day(previous, entries);
            }
            None => {
                self.board.lock().job.report.has_reached_oldest = true;
                self.board.raise();
            }
        }
    }

    fn list(&self, shelf: Shelf) {
        if !self.is_ready() {
            return;
        }
        let (memory, data) = (self.read_store(|store| store.memory.clone()), &self.job.data);
        let memos = match shelf {
            Shelf::Facts => to_fact_entries(&memory),
            Shelf::Days => {
                let robot = memory.robot_name();
                let day_entry = |day: String| {
                    let recollection = files::recollection(data, &day);
                    let words = transcript::to_reading(&files::entries(data, &day), robot);
                    to_day_entry(day, recollection, words)
                };
                to_listed_days(files::talk_days(data)).into_iter().map(day_entry).collect()
            }
            Shelf::Notes | Shelf::Talks | Shelf::News => return,
        };
        let previous = self.board.lock().job.report.listing.replace((shelf, memos));
        drop(previous);
        self.board.raise();
    }

    fn forget(&self, lapse: &Lapse) {
        if !self.is_ready() {
            return;
        }
        let data = &self.job.data;
        // What is forgotten is what left its file: a fact no file held, or a write that failed, is none.
        let is_forgotten = match lapse {
            Lapse::Fact(line) => {
                self.change_store(|store| store.memory.forget(line) && files::write_memory(data, &store.memory))
            }
            // Under the store's loan, as a day's settling writes under it: a day forgotten while it settles keeps nothing of the settling.
            Lapse::Day(day) => {
                let removed = self.change_store(|_| files::forget_day(data, day));
                let removed = removed.inspect_err(|e| log!("chat: what was to be forgotten is not: {e}"));
                if removed.is_ok() {
                    self.board.lock().job.report.forgotten.push(day.clone());
                }
                removed.is_ok()
            }
            Lapse::Thread => {
                let change = self.change_store(|store| store.thread.clear());
                files::write_item(data, &change)
            }
        };
        if is_forgotten {
            log!("{}", to_forgotten_line(lapse));
        }
        if let Err(e) = data.trim() {
            log!("chat: the free blocks are not handed back to the medium: {e}");
        }
        self.board.lock().job.mark_stale();
        self.board.raise();
    }
}

impl Part<Robot> {
    /// Starts the threads, idle until the screen opens, with files on `data` and the model services through `account`.
    pub fn start(data: Data, account: Account) -> io::Result<Chat> {
        let chat = Part::spawn(Robot { data }, true, account)?;
        let shared = Arc::clone(&chat.shared);
        sys::spawn_background("recall", move || recollect::work(&shared))?;
        Ok(chat)
    }

    /// The number of the reply to read aloud.
    pub fn voice(&self) -> Option<u32> {
        self.shared.board.lock().voice()
    }

    /// Sets how many passages of reply `round` were heard to their end.
    pub fn set_heard(&self, round: u32, count: usize) {
        self.shared.board.lock().job.set_heard(round, count);
    }

    /// Takes the phase of the reading of reply `round`.
    pub fn set_voice_phase(&self, round: u32, phase: Phase) {
        self.shared.tell(|state| state.job.take_voice_phase(round, phase));
    }

    /// Hands the answer to the readout the turn thread asked for to it.
    pub fn answer_readout(&self, answer: Reply) {
        self.shared.board.lock().job.readout_answer = Some(answer);
        self.shared.board.notify();
    }

    /// Takes what the owner has set, which tells the turns their tools.
    pub fn set_settings(&self, settings: Settings) {
        self.shared.board.lock().job.settings = settings;
    }

    /// True while a turn or a settling needs the network.
    pub fn is_owing(&self) -> bool {
        self.shared.board.lock().is_owing()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use crate::wifi::radio::Radio;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::voice::Bid;
    use std::thread;
    use std::time::Duration;

    fn chat() -> (Chat, Radio) {
        let radio = without_networks();
        (Chat::start(Data::unusable(), Account::new(Data::unusable(), radio.online())).unwrap(), radio)
    }

    #[test]
    fn no_partition_fails_input() {
        let (chat, _radio) = chat();
        let mut epoll = Epoll::with_fd(chat.fd());
        assert!(chat.is_off() && !chat.is_owing() && chat.voice().is_none());
        chat.ask(vec![Bid::Job(Chore::Open), Bid::Text("Hallo".into()), Bid::Listen, Bid::Send]);
        let mut phase = Some(Phase::Idle);
        while phase == Some(Phase::Idle) {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7]);
            phase = chat.latest().phase;
        }
        assert_eq!(phase, Some(Phase::Failed(Cause::NoStorage)));
        while !chat.is_off() {
            thread::yield_now();
        }
        assert!(!chat.shared.board.lock().has_inputs(), "nothing waits for a turn that could not be kept");
    }
}
