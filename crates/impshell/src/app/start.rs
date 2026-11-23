use super::console::{Dispatch, Host, Requests, open_console};
#[cfg(feature = "debug-shell")]
use super::shell::Shell;
use super::{App, Parts, Source, mark_slot, require, watch};
use crate::about::{self, About};
use crate::aloud::Reader;
use crate::bell::Setter;
use crate::calls::Calls;
use crate::camera::Camera;
use crate::cell::Modem;
use crate::chat::Chat;
use crate::cost::Cost;
use crate::display::fb::Framebuffer;
use crate::display::recorder::Recorder;
use crate::gallery::Gallery;
use crate::input::Inputs;
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
use crate::storage::Storage;
use crate::storage::logs::Logs;
use crate::storage::settings::Keeper;
use crate::sys::devnodes;
use crate::sys::uevent::Monitor;
use crate::sys::{self, Epoll};
use crate::timekeeper::Timing;
use crate::translator::Translator;
use crate::usb::drive::Drive;
use crate::web::Web;
use crate::wifi::radio::Radio;
use crate::{log, sound, usb};
use r1_funcore::base::handoff::{Attempt, Latch};
use r1_funcore::domain::app::cycle::Cycle;
use r1_funcore::domain::app::light::Light;
use r1_funcore::domain::app::{self, Boost};
use r1_funcore::domain::camera::CallPicture;
use r1_funcore::domain::{device, magic, news, place, router, uses};
use r1_funcore::gfx::panel::to_shape;
use r1_funcore::gfx::refresh::REFRESH_NS;
use r1_funcore::hw::power::alarm::Reason;
use r1_funcore::hw::power::battery;
use r1_funcore::hw::slots::Marking;
use r1_funcore::openai::key;
use r1_funcore::storage::{CHAT, CHAT_PRIVATE, INTERFACE, NEWS, NOTES, PICTURES, TALKS, VIDEOS};
use r1_funcore::ui::Ui;
use r1_funcore::{cell, logs, signal, wifi};

impl App {
    pub(super) fn start() -> App {
        let cmdline = sys::read_kernel_text(sys::CMDLINE).unwrap_or_default();
        log!("{}", device::to_cmdline_line(&cmdline));
        let flash = sys::read_flash();
        // This thread draws: no ordinary thread, the kernel's charger thread included, may take its core in the middle of a frame.
        if let Err(e) = sys::set_drawing_priority() {
            log!("drawing thread: stays an ordinary one: {e}");
        }

        let mut epoll = Epoll::new().expect("epoll is always available");
        let monitor = watch(&mut epoll, Source::Uevent, "device events", Monitor::open(), Monitor::fd);
        let udc = usb::start(device::parse_serial_number(&cmdline)).inspect_err(|e| log!("usb gadget: {e}")).ok();
        devnodes::coldplug().to_gadget_line().iter().for_each(|line| log!("{line}"));
        let console = open_console(&mut epoll);
        if let Some(fd) = usb::frames::control()
            && let Err(e) = epoll.add(fd, app::to_token(Source::Frames as usize))
        {
            log!("watching the frames function: {e}");
        }
        if let Err(e) = epoll.add(sound::level_fd(), app::to_token(Source::Microphone as usize)) {
            log!("watching the microphone's level: {e}");
        }

        let inputs = Inputs::open();
        for (index, fd) in inputs.fds() {
            if let Err(e) = epoll.add(fd, index as u64) {
                log!("watching input {index}: {e}");
            }
        }
        let fb = Framebuffer::open().inspect_err(|e| log!("framebuffer: {e}")).ok();
        let storage = require(&mut epoll, Epoll::add, Source::Storage, Storage::start(), Storage::fd);
        let drive = Drive::start(storage.data()).inspect_err(|e| log!("drive: {e}")).ok();
        let scope = storage.within(&[INTERFACE, wifi::DIRECTORY, uses::DIRECTORY]);
        let keeper = require(&mut epoll, Epoll::add, Source::Settings, Keeper::start(scope, flash), Keeper::fd);
        let logs = Logs::start(storage.within(&[logs::DIRECTORY]));
        let logs = require(&mut epoll, Epoll::add, Source::Logs, logs, Logs::fd);
        let meter = Meter::start(storage.within(&[battery::DIRECTORY]), keeper.settler());
        let meter = require(&mut epoll, Epoll::add, Source::Battery, meter, Meter::fd);
        let gate = require(&mut epoll, Epoll::add, Source::Suspend, Gate::new(), Gate::fd);
        let bell = require(&mut epoll, Epoll::add_wakeup, Source::Bell, Alarm::new(Reason::Bell), Alarm::fd);

        let facts = device::start_facts(
            sys::read_kernel_text(sys::OS_RELEASE).ok(),
            fb.as_ref().map(|fb| fb.geometry().to_text()),
            &inputs.names(),
            udc,
        );
        for (part, value) in &facts {
            log!("{}", part.to_line(value.as_deref()));
        }
        let (width, height, format) = to_shape(fb.as_ref().map(Framebuffer::geometry));
        let mut ui = Ui::new(width as i32, height as i32, format, facts);
        ui.await_safe();
        ui.await_settings();
        let networks = keeper.networks();
        let timing = Timing::new(storage.within(&[place::DIRECTORY]));
        let timing = require(&mut epoll, Epoll::add, Source::Time, timing, |timing| timing.fd());
        let modem = Modem::start(storage.within(&[cell::DIRECTORY]));
        let modem = require(&mut epoll, Epoll::add, Source::Cell, modem, Modem::fd);
        let radio = Radio::start(networks.clone(), timing.hold_hook(), modem.cellular());
        let radio = require(&mut epoll, Epoll::add, Source::Radio, radio, Radio::fd);
        let online = radio.online();
        let router = Router::start(storage.within(&[router::DIRECTORY]), online.clone());
        let router = require(&mut epoll, Epoll::add, Source::Router, router, Router::fd);
        let account = Account::new(storage.within(&[key::DIRECTORY]), online.clone());
        let rundown = require(&mut epoll, Epoll::add, Source::Rundown, Rundown::new(online.clone()), Rundown::fd);
        let keys = require(&mut epoll, Epoll::add, Source::Keys, Keys::start(account.clone()), Keys::fd);
        let magic = Magic::start(storage.within(&[magic::DIRECTORY, PICTURES]), account.clone());
        let magic = require(&mut epoll, Epoll::add, Source::Magic, magic, Magic::fd);
        let scribe = Scribe::start(storage.within(&[NOTES, TALKS]), account.clone());
        let scribe = require(&mut epoll, Epoll::add, Source::Scribe, scribe, Scribe::fd);
        let memos = Memos::start(storage.within(&[NOTES, TALKS, NEWS]));
        let memos = require(&mut epoll, Epoll::add, Source::Memos, memos, Memos::fd);
        let translator = Translator::start(storage.within(&[TALKS]), account.clone());
        let translator = require(&mut epoll, Epoll::add, Source::Talk, translator, Translator::fd);
        let editor = Editor::start(storage.within(&[NEWS, news::STATE_DIR]), account.clone());
        let editor = require(&mut epoll, Epoll::add, Source::Editor, editor, Editor::fd);
        let reader = require(&mut epoll, Epoll::add, Source::Aloud, Reader::start(account.clone()), Reader::fd);
        let chat_scope = storage.within(&[CHAT, CHAT_PRIVATE, NOTES, TALKS, NEWS, place::DIRECTORY]);
        let chat = Chat::start(chat_scope, account.clone());
        let chat = require(&mut epoll, Epoll::add, Source::Chat, chat, Chat::fd);
        let pokedex = Pokedex::start(account.clone(), online.clone(), format, ui.artwork_side());
        let pokedex = require(&mut epoll, Epoll::add, Source::Pokedex, pokedex, Pokedex::fd);
        let web = Web::start(account.clone(), storage.within(&[place::DIRECTORY]));
        let web = require(&mut epoll, Epoll::add, Source::Web, web, Web::fd);
        let setter = require(&mut epoll, Epoll::add, Source::Setter, Setter::start(account.clone()), Setter::fd);
        let curator = Curator::start(account.clone(), storage.within(&[place::DIRECTORY]));
        let curator = require(&mut epoll, Epoll::add, Source::Curator, curator, Curator::fd);
        let signal_scope = storage.within(&[signal::device::DIRECTORY, PICTURES, VIDEOS, NOTES]);
        let signal = Signal::start(signal_scope, online.clone(), account.clone(), (keeper.uses(), format));
        let signal = require(&mut epoll, Epoll::add, Source::Signal, signal, Signal::fd);
        let drafts = require(&mut epoll, Epoll::add, Source::Drafts, Drafts::start(account.clone()), Drafts::fd);
        let calls = require(&mut epoll, Epoll::add, Source::Calls, Calls::start(online.clone(), format), Calls::fd);
        let cost_account = account.clone();
        let cost = require(&mut epoll, Epoll::add, Source::Cost, Cost::new(move || cost_account.ledger()), Cost::fd);
        let data = storage.within(&[]);
        let about = About::new(move || about::station_address(&networks), move || data.room());
        let about = require(&mut epoll, Epoll::add, Source::About, about, About::fd);
        let recorder = fb.as_ref().filter(|_| usb::frames::is_ready()).and_then(|_| {
            Recorder::start(width as usize, height as usize, format).inspect_err(|e| log!("recording: {e}")).ok()
        });
        let camera = fb.as_ref().and_then(|_| {
            let files = storage.within(&[PICTURES, VIDEOS, magic::DIRECTORY]);
            watch(&mut epoll, Source::Camera, "camera", Camera::start(format, REFRESH_NS, files), Camera::fd)
        });
        let gallery = fb.as_ref().and_then(|_| {
            let files = storage.within(&[PICTURES, VIDEOS, magic::DIRECTORY]);
            let result = Gallery::start(format, REFRESH_NS, files);
            watch(&mut epoll, Source::Gallery, "gallery", result, Gallery::fd)
        });
        let slot = Marking::with(mark_slot(&cmdline));
        slot.to_waiting_line().iter().for_each(|line| log!("{line}"));

        let is_recordable = recorder.is_some();
        #[cfg(feature = "debug-shell")]
        let shell = watch(&mut epoll, Source::Shell, "shell", Shell::start(storage.data()), Shell::fd);
        #[cfg(feature = "debug-shell")]
        let dispatch = Dispatch::with_shell(shell);
        #[cfg(not(feature = "debug-shell"))]
        let dispatch = Dispatch::default();
        let light = Light::default();
        App {
            cmdline,
            slot,
            epoll,
            ui,
            parts: Parts {
                monitor,
                inputs,
                fb,
                recorder,
                storage,
                drive,
                keeper,
                logs,
                meter,
                gate,
                modem,
                radio,
                router,
                rundown,
                account,
                keys,
                magic,
                scribe,
                memos,
                translator,
                editor,
                reader,
                chat,
                pokedex,
                web,
                setter,
                curator,
                signal,
                drafts,
                calls,
                cost,
                about,
                timing,
                online,
                camera,
                gallery,
            },
            host: Host { console, requests: Requests::default(), dispatch, is_recordable },
            ready: Vec::new(),
            frame_line: String::new(),
            event_line: String::new(),
            events: Vec::new(),
            call_picture: CallPicture::default(),
            cycle: Cycle::default(),
            boost: Boost::default(),
            tilt: None,
            tilt_attempt: Attempt::default(),
            light,
            levels: Latch::default(),
            is_searching: Latch::with(false),
            _search_sound: None,
            notices: Latch::with(0),
            _notice_sound: None,
            bell,
            bell_at: Latch::default(),
            is_ringing: Latch::with(false),
            _ring_sound: None,
            logging: Latch::default(),
            roles: Latch::default(),
            settings: Latch::default(),
            scene_words: Latch::with(("", "")),
            is_pacing: Latch::with(false),
        }
    }
}
