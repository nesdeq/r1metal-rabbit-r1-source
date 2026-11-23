mod auto;
mod drafts;
mod files;
mod groups;
mod link;
mod names;
mod pictures;
mod socket;
mod stay;
mod transfers;
mod voices;

pub use drafts::Drafts;

use crate::clock;
use crate::crypto::Library;
use crate::log;
use crate::openai::Account;
use crate::power::alarm::Alarm;
use crate::storage::Data;
use crate::storage::settings::Uses;
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake};
use crate::wifi::radio::{Online, RadioGrant};
use files::Traces;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::domain::signal::auto::Limits;
use r1_funcore::domain::signal::robot::{self, Question};
use r1_funcore::domain::signal::state::{
    Await, CallReport, FETCH_LINE, LEFTOVERS_LINE, LinkStep, Need, Report, SignalView, State, Work,
};
use r1_funcore::gfx::palette::Roles;
use r1_funcore::hw::power::alarm::Reason;
use r1_funcore::signal::attachment::ATTACHMENTS;
use r1_funcore::signal::command::Command;
use r1_funcore::signal::device::{Device, Event, LinkState};
use r1_funcore::signal::{AUTO_THREAD, MEDIA_THREAD, PICTURES_THREAD, THREAD, VOICES_THREAD};
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;

const SOCKET: u64 = 1;
const POKE: u64 = 2;
const ALARM: u64 = 3;

struct Shared {
    board: Board<State>,
    poke: Wake,
}

struct Resources {
    online: Online,
    data: Data,
}

impl Resources {
    fn write(&self, device: &mut Device) {
        if let Err(e) = files::write(&self.data, device) {
            log!("signal: the files are not written: {e}");
        }
    }

    fn keep_sounds(&self, sounds: Vec<(String, Vec<u8>)>) {
        for (name, sound) in sounds {
            if let Err(e) = self.data.put(ATTACHMENTS, &name, &sound) {
                log!("signal: a voice note said is not kept: {e}");
            }
        }
    }
}

impl Shared {
    fn wake_thread(&self) {
        self.board.notify();
        self.poke.raise();
    }

    // Hands the state what a delivery, a fetch of groups or a stay's end told, and wakes the main loop where the call layer has news.
    fn take_events(&self, events: Vec<Event>) {
        if self.board.lock().take_events(events, sys::boottime_ms()) {
            self.board.raise();
        }
    }

    fn publish(&self, device: &Device) {
        self.board.lock().publish(device, sys::realtime_ms());
        self.board.raise();
        self.board.notify();
    }

    // Answers the robot's question of the device as it is, linked or not, without the network.
    fn answer(&self, device: Option<&Device>) {
        let Some(question) = self.board.lock().take_question() else {
            return;
        };
        let reply = robot::answer(device, &question, sys::realtime_ms(), clock::offset());
        self.board.lock().note_reply(reply);
        self.board.raise();
    }

    // Takes away the files of attachments nothing holds: not a message, and not a transfer.
    fn sweep(&self, resources: &Resources, device: &Device) {
        let mut held = device.held_files();
        held.extend(self.board.lock().claimed_files());
        files::sweep(&resources.data, &held);
    }

    fn set_link_step(&self, step: LinkStep) {
        self.board.lock().set_link_step(step);
        self.board.raise();
    }

    fn fail_and_wait(&self, fail: impl FnOnce(&mut State)) {
        fail(&mut self.board.lock());
        self.board.raise();
        self.wait_retry();
    }

    fn wait_retry(&self) {
        let wait = self.board.lock().retry_wait();
        if let Some(wait) = wait {
            self.board.wait_change(wait);
        }
    }

    fn keep_sends(&self, resources: &Resources, device: &mut Device) {
        let sends = self.board.lock().take_sends();
        let has_sends = !sends.is_empty();
        resources.keep_sounds(device.keep_sends(&Library, sends, sys::realtime_ms()));
        if has_sends {
            resources.write(device);
            self.publish(device);
        }
    }
}

fn await_need(shared: &Shared, alarm: &Alarm, epoll: &mut Epoll, device: Option<&Device>) -> io::Result<Need> {
    loop {
        shared.poke.clear();
        shared.answer(device);
        let next = shared.board.lock().next_wait();
        let at = match next {
            Await::Work(need) => return Ok(need),
            Await::Arm(at) => at,
        };
        let is_set = alarm.arm(at).unwrap_or_else(|e| {
            log!("signal: the alarm: {e}");
            true
        });
        if !is_set {
            continue;
        }
        let ready = epoll.wait(None)?;
        if ready.contains(&ALARM) {
            alarm.acknowledge();
            if shared.board.lock().take_alarm(sys::realtime_seconds()) {
                log!("{FETCH_LINE}");
                shared.board.raise();
            }
        }
    }
}

fn run(shared: &Shared, resources: &Resources, traces: &Traces, (alarm, epoll): (&Alarm, &mut Epoll)) -> io::Error {
    let result = if resources.data.wait_decided() { files::read(&resources.data) } else { Ok(LinkState::Unlinked) };
    let mut device = match result {
        Ok(LinkState::Linked(device)) => Some(*device),
        Ok(LinkState::Unlinked) => None,
        Ok(LinkState::Leftovers) => {
            log!("{LEFTOVERS_LINE}");
            files::forget(&resources.data, traces);
            None
        }
        Err(e) => {
            log!("signal: the files: {e}");
            None
        }
    };
    shared.board.lock().note_files_read(device.is_some(), sys::realtime_seconds());
    if let Some(device) = &device {
        shared.sweep(resources, device);
        shared.publish(device);
    }
    shared.board.raise();
    loop {
        let need = match await_need(shared, alarm, epoll, device.as_ref()) {
            Ok(need) => need,
            Err(e) => return e,
        };
        match need.to_work(device.as_mut()) {
            Work::Link => {
                device = link::link(shared, resources);
                if let Some(device) = &device {
                    shared.publish(device);
                }
            }
            Work::Stay(linked_device) => {
                if stay::run_stay(shared, resources, traces, linked_device) {
                    device = None;
                }
                shared.board.lock().note_stay_end(sys::realtime_seconds());
            }
            Work::Rest => shared.wait_retry(),
        }
    }
}

/// The main loop's handle of Signal's threads.
pub struct Signal {
    shared: Arc<Shared>,
}

impl Signal {
    /// Starts the Signal thread, with files on `data` and the service through `online`, and auto mode's writer through `account`; and the threads beside it that carry attachments, decode photos in `format` and play voice notes.
    pub fn start(
        data: Data,
        online: Online,
        account: Account,
        (uses, format): (Uses, PixelFormat),
    ) -> io::Result<Signal> {
        let shared = Arc::new(Shared { board: Board::new(State::default())?, poke: Wake::new()? });
        let (writer, traces) = (Arc::clone(&shared), Traces { account: account.clone(), uses });
        sys::spawn_background(AUTO_THREAD, move || auto::write_turns(&writer, &account))?;
        let alarm = Alarm::new(Reason::SignalMessages)?;
        let mut epoll = Epoll::new()?;
        epoll.add_wakeup(alarm.fd(), ALARM)?;
        epoll.add(shared.poke.fd(), POKE)?;
        let resources = Arc::new(Resources { online, data });
        let (carrier, decoder, player) = (Arc::clone(&shared), Arc::clone(&shared), Arc::clone(&shared));
        let (carrying, decoding, playing) = (Arc::clone(&resources), Arc::clone(&resources), Arc::clone(&resources));
        sys::spawn_background(MEDIA_THREAD, move || transfers::work(&carrier, &carrying))?;
        sys::spawn_background(PICTURES_THREAD, move || pictures::work(&decoder, &decoding, format))?;
        sys::spawn_background(VOICES_THREAD, move || voices::work(&player, &playing))?;
        let thread = Arc::clone(&shared);
        sys::spawn_background(THREAD, move || {
            log!("signal: waiting: {}", run(&thread, &resources, &traces, (&alarm, &mut epoll)));
            thread.board.lock().end_thread();
            thread.board.raise();
            thread.board.notify();
        })?;
        Ok(Signal { shared })
    }

    /// Readable when there is something new for the screens.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Hands on the screens' commands.
    pub fn ask(&self, commands: Vec<Command>) {
        if commands.is_empty() {
            return;
        }
        self.shared.board.lock().add_commands(commands);
        self.shared.wake_thread();
    }

    /// Sets the Signal screen that shows, whether the device is awake, the alarm's fetch period in minutes, nought for none, auto mode's limits, and whether photos and voice notes are fetched.
    pub fn want(&self, view: SignalView, is_awake: bool, period: u8, (limits, is_fetching): (Limits, bool)) {
        let is_changed = self.shared.board.lock().set_wanted(view, is_awake, period, limits, is_fetching);
        if is_changed {
            self.shared.wake_thread();
        }
    }

    /// Sets the voice note to play, by its file and the number of its playing, and the photos to decode with the width they stand at and the colours they are drawn over.
    pub fn want_media(&self, playing: Option<(&str, u32)>, photos: Option<(Vec<String>, usize)>, roles: Roles) {
        let mut state = self.shared.board.lock();
        let (is_playing, is_drawing) = (state.want_voice(playing), state.want_pictures(photos, roles));
        drop(state);
        if is_playing || is_drawing {
            self.shared.board.notify();
        }
    }

    /// Hands the thread the robot's question, which it answers of the device as it is.
    pub fn ask_question(&self, question: Question) {
        self.shared.board.lock().add_question(question);
        self.shared.board.raise();
        self.shared.wake_thread();
    }

    /// Hands the thread the radio's grant for what it does while the device sleeps.
    pub fn grant_radio(&self, _: &RadioGrant) {
        if self.shared.board.lock().grant_radio() {
            self.shared.wake_thread();
        }
    }

    /// Takes the report for the screens.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }

    /// Takes what the thread has for the call layer, and the call log for the screens.
    pub fn take_call_report(&self) -> CallReport {
        self.shared.board.lock().take_call_report()
    }

    /// Sets whether the calls hold the socket: a call stands, or their screen shows.
    pub fn set_calling(&self, is_calling: bool) {
        if self.shared.board.lock().set_calling(is_calling) {
            self.shared.wake_thread();
        }
    }

    /// True while nothing of Signal stands in the way of a sleep.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }
}
