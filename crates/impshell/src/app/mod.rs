mod console;
mod frame;
mod serve;
#[cfg(feature = "debug-shell")]
mod shell;
mod start;
mod steer;

use crate::about::About;
use crate::aloud::Reader;
use crate::bell::Setter;
use crate::calls::Calls;
use crate::camera::Camera;
use crate::cell::Modem;
use crate::chat::Chat;
use crate::cost::Cost;
use crate::display::backlight;
use crate::display::fb::Framebuffer;
use crate::display::recorder::Recorder;
use crate::gallery::Gallery;
use crate::input::Inputs;
use crate::input::tilt::Tilt;
use crate::magic::Magic;
use crate::memos::Memos;
use crate::memos::scribe::Scribe;
use crate::news::Editor;
use crate::news::request::Curator;
use crate::openai::Account;
use crate::openai::keys::Keys;
use crate::pokedex::Pokedex;
use crate::power::alarm::Alarm;
use crate::power::battery::Meter;
use crate::power::sleep::Gate;
use crate::router::Router;
use crate::rundown::Rundown;
use crate::signal::{Drafts, Signal};
use crate::storage::logs::{self, Logs};
use crate::storage::settings::Keeper;
use crate::storage::{Storage, slots};
use crate::sys::uevent::Monitor;
use crate::sys::{self, Epoll, klog};
use crate::timekeeper::Timing;
use crate::translator::Translator;
use crate::usb::drive::Drive;
use crate::web::Web;
use crate::wifi::radio::{Online, Radio};
use crate::{log, sound};
use console::Host;
use r1_funcore::base::handoff::{Attempt, Latch};
use r1_funcore::domain::app::cycle::{Cycle, Pass};
use r1_funcore::domain::app::light::Light;
use r1_funcore::domain::app::{self, Boost};
use r1_funcore::domain::camera::CallPicture;
use r1_funcore::domain::settings::Settings;
use r1_funcore::gfx::palette::Roles;
use r1_funcore::hw::power::doze::SCREEN_OFF_LINE;
use r1_funcore::hw::slots::{MARKED_LINE, Mark, Marking};
use r1_funcore::input::Event;
use r1_funcore::input::pose::Pose;
use r1_funcore::logs::Logging;
use r1_funcore::logs::kernel::are_lines_read;
use r1_funcore::logs::snapshot::{SLEEP, WAKE};
use r1_funcore::ui::Ui;
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::sync::Arc;

macro_rules! sources {
    ($($(#[$attribute:meta])* $source:ident),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Source {
            $($(#[$attribute])* $source),*
        }

        impl Source {
            const ALL: &[Source] = &[$($(#[$attribute])* Source::$source),*];
        }
    };
}

sources! {
    Console, Uevent, Battery, Storage, Camera, Tilt, Gallery, Settings, Radio, Magic, Cost, Time, Logs, Suspend, Memos,
    Scribe, Talk, Editor, Aloud, Rundown, #[cfg(feature = "debug-shell")] Shell, About, Frames, Chat, Keys,
    Pokedex, Signal, Drafts, Bell, Cell, Microphone, Web, Setter, Curator, Router, Calls,
}

fn mark_slot(cmdline: &str) -> bool {
    match slots::mark_booted(cmdline) {
        Ok(Mark::Dirty) => log!("{MARKED_LINE}"),
        Ok(Mark::Clean) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return false,
        Err(e) => log!("marking the boot slot successful: {e}"),
    }
    if let Err(e) = slots::tell_booted() {
        log!("log store: the boot is not told up: {e}");
    }
    true
}

fn turn(pose: Pose, ui: &mut Ui, camera: &Option<Camera>, gallery: &Option<Gallery>) -> bool {
    let needs_redraw = ui.set_pose(pose);
    camera.iter().for_each(|camera| camera.set_pose(pose));
    gallery.iter().for_each(|gallery| gallery.set_pose(pose));
    needs_redraw
}

// How a part's descriptor is watched: `Epoll::add`, or `Epoll::add_wakeup` for an alarm.
type Add = fn(&mut Epoll, RawFd, u64) -> io::Result<()>;

fn add_watch<T>(
    epoll: &mut Epoll,
    add: Add,
    source: Source,
    result: io::Result<T>,
    fd: fn(&T) -> RawFd,
) -> io::Result<T> {
    result.and_then(|part| add(epoll, fd(&part), app::to_token(source as usize)).map(|()| part))
}

fn watch<T>(epoll: &mut Epoll, source: Source, name: &str, result: io::Result<T>, fd: fn(&T) -> RawFd) -> Option<T> {
    add_watch(epoll, Epoll::add, source, result, fd).inspect_err(|e| log!("{name}: {e}")).ok()
}

fn require<T>(epoll: &mut Epoll, add: Add, source: Source, result: io::Result<T>, fd: fn(&T) -> RawFd) -> T {
    add_watch(epoll, add, source, result, fd).unwrap_or_else(|e| panic!("{source:?}: {e}"))
}

struct Parts {
    monitor: Option<Monitor>,
    inputs: Inputs,
    fb: Option<Framebuffer>,
    recorder: Option<Recorder>,
    storage: Storage,
    drive: Option<Drive>,
    keeper: Keeper,
    logs: Logs,
    meter: Meter,
    gate: Gate,
    modem: Modem,
    radio: Radio,
    router: Router,
    rundown: Rundown,
    account: Account,
    keys: Keys,
    magic: Magic,
    scribe: Scribe,
    memos: Memos,
    translator: Translator,
    editor: Editor,
    reader: Reader,
    chat: Chat,
    pokedex: Pokedex,
    web: Web,
    setter: Setter,
    curator: Curator,
    signal: Signal,
    drafts: Drafts,
    calls: Calls,
    cost: Cost,
    about: About,
    timing: Arc<Timing>,
    online: Online,
    camera: Option<Camera>,
    gallery: Option<Gallery>,
}

impl Parts {
    fn is_quiet(&self) -> bool {
        app::is_quiet(&[
            self.camera.as_ref().is_none_or(Camera::is_off),
            self.gallery.as_ref().is_none_or(Gallery::is_off),
            self.storage.is_off(),
            self.radio.is_off(),
            self.modem.is_off(),
            self.router.is_off(),
            self.rundown.is_off(),
            self.memos.is_off(),
            self.translator.is_off(),
            self.reader.is_off(),
            self.chat.is_off(),
            self.pokedex.is_off(),
            self.web.is_off(),
            self.setter.is_off(),
            self.curator.is_off(),
            self.signal.is_off(),
            self.drafts.is_off(),
            self.calls.is_off(),
            self.editor.is_off(),
            sound::is_off(),
        ])
    }
}

struct App {
    cmdline: String,
    slot: Marking,
    epoll: Epoll,
    ui: Ui,
    parts: Parts,
    host: Host,
    ready: Vec<u64>,
    frame_line: String,
    event_line: String,
    events: Vec<Event>,
    // The picture swapped between the camera and the calls part, so neither makes a new one for each frame.
    call_picture: CallPicture,
    cycle: Cycle,
    boost: Boost,
    tilt: Option<Tilt>,
    tilt_attempt: Attempt,
    light: Light,
    levels: Latch<(u8, u8)>,
    is_searching: Latch<bool>,
    _search_sound: Option<sound::SoundStream>,
    notices: Latch<u32>,
    _notice_sound: Option<sound::SoundStream>,
    bell: Alarm,
    bell_at: Latch<Option<u64>>,
    is_ringing: Latch<bool>,
    _ring_sound: Option<sound::SoundStream>,
    logging: Latch<Logging>,
    roles: Latch<Roles>,
    settings: Latch<Settings>,
    scene_words: Latch<(&'static str, &'static str)>,
    is_pacing: Latch<bool>,
}

pub fn run() -> ! {
    // Before the first thread starts, so that the table grows without a grace period.
    if let Err(e) = sys::grow_descriptor_table() {
        log!("descriptor table: grows as it fills: {e}");
    }
    let mut app = App::start();
    loop {
        app.pass();
    }
}

impl App {
    fn pass(&mut self) {
        for line in klog::drain() {
            if let Some(console) = &mut self.host.console {
                console.log(&line);
            }
        }
        let now = self.cycle.begin_pass(sys::monotonic_ns());
        let mut pass = Pass::default();
        self.follow_settings();
        self.tick(now, &mut pass);
        self.draw(now);
        self.set_levels();
        self.wait();
        let now = self.cycle.after_wait(now, sys::monotonic_ns());
        let ready = mem::take(&mut self.ready);
        for &token in &ready {
            self.serve(token, now, &mut pass);
        }
        self.ready = ready;
        self.cycle.note_change(mem::take(&mut self.host.requests.redraw));
        self.events.append(&mut self.host.requests.events);
        self.handle_inputs(now, &mut pass);
        let exit = pass.to_exit();
        self.steer(now, exit.is_some());
        self.go_down(exit);
    }

    fn begin_sleep(&mut self) {
        self.set_screen(false);
        log!("{SCREEN_OFF_LINE}");
        logs::snapshot(SLEEP);
    }

    fn wake(&mut self, now: u64, has_cable: bool) {
        let lines = self.ui.note_wake(now, has_cable);
        self.set_screen(true);
        lines.iter().for_each(|line| log!("{line}"));
        logs::snapshot(WAKE);
    }

    fn set_screen(&mut self, is_on: bool) {
        if is_on {
            self.cycle.note_change(true);
            self.light.relight();
            return;
        }
        if let Err(e) = backlight::off() {
            log!("backlight: {e}");
        }
        self.power_panel(false);
    }

    fn power_panel(&mut self, is_on: bool) {
        if let Some(Err(e)) = self.parts.fb.as_mut().map(|fb| fb.set_power(is_on)) {
            log!("screen: panel: {e}");
        }
    }

    fn are_lines_read(&self) -> bool {
        are_lines_read(klog::is_silent(), self.host.console.is_some(), logs::is_on())
    }
}
