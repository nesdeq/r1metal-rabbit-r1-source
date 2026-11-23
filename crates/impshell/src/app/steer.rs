use super::{App, Source, turn, watch};
use crate::camera::Camera;
use crate::gallery::Gallery;
use crate::input::tilt::Tilt;
use crate::power::sleep;
use crate::storage::logs;
use crate::sys;
use crate::{clock, log, sound};
use r1_funcore::base::time::day_number;
use r1_funcore::base::time::setting::to_moved_line;
use r1_funcore::domain::app::cycle::{Pass, to_exit_line};
use r1_funcore::domain::chat::tools::{Act, to_act_line};
use r1_funcore::domain::chat::{self, to_search_line};
use r1_funcore::domain::memos::Request;
use r1_funcore::domain::news::editor;
use r1_funcore::domain::recording::to_asked_line;
use r1_funcore::domain::router::{Asker, Order};
use r1_funcore::domain::scribe;
use r1_funcore::domain::signal::auto::Limits;
use r1_funcore::domain::voice::Bid;
use r1_funcore::domain::{app, bell, scene};
use r1_funcore::gfx::palette::Roles;
use r1_funcore::hw::FULL_LOAD;
use r1_funcore::hw::power::doze::Step;
use r1_funcore::hw::power::{Power, SHUTDOWN_TIMEOUT};
use r1_funcore::hw::sound::NOTICE_LINE;
use r1_funcore::input::pose::to_named_line;
use r1_funcore::input::{Event, to_event_text};
use r1_funcore::logs::LineFile;
use r1_funcore::ui::Action;
use std::mem;
use std::time::Instant;

enum Input {
    Event(Event),
    Act(Act),
}

fn search_sound() -> Option<sound::SoundStream> {
    let mut playback = chat::search_sound()?;
    let fill = move |block: &mut [i16]| {
        playback.fill_looped(block);
        true
    };
    sound::play_aside(fill).inspect_err(|e| log!("sound: the robot's searching: {e}")).ok()
}

fn ring_sound() -> Option<sound::SoundStream> {
    let mut ring = bell::ring_sound()?;
    let fill = move |block: &mut [i16]| {
        ring.fill_looped(block);
        true
    };
    sound::play_sound(fill).inspect_err(|e| log!("sound: the bell: {e}")).ok()
}

fn notice_sound() -> Option<sound::SoundStream> {
    let mut notice = r1_funcore::hw::sound::notice()?;
    sound::play_sound(move |block| notice.fill_block(block)).inspect_err(|e| log!("sound: the notice: {e}")).ok()
}

impl App {
    pub(super) fn handle_inputs(&mut self, now: u64, pass: &mut Pass) {
        // A finger that rests on the screen makes no event, which a frame would be drawn for, and is use of the device all the same.
        if self.parts.inputs.take_finger_rest() {
            self.ui.note_finger_rests(now);
        }
        let acts = self.ui.take_acts();
        if self.boost.take_inputs(self.ui.is_awake(), &acts, &self.events)
            && let Err(e) = sys::set_clock_floor(FULL_LOAD)
        {
            log!("clock floor: {e}");
        }
        let mut events = mem::take(&mut self.events);
        for input in acts.into_iter().map(Input::Act).chain(events.drain(..).map(Input::Event)) {
            let action = match input {
                Input::Act(act) => {
                    log!("{}", to_act_line(&act));
                    self.cycle.note_change(true);
                    self.ui.do_act(act, now)
                }
                Input::Event(event) => {
                    if self.are_lines_read() {
                        self.ui.write_event_line(&mut self.event_line, &event);
                        logs::add_line(LineFile::Input, to_event_text(&self.event_line));
                        if let Some(console) = &mut self.host.console {
                            console.log(&self.event_line);
                        }
                    }
                    let action = self.ui.handle(event, now);
                    // A frame is drawn for what changes the picture: a finger moving over a text changes none.
                    self.cycle.note_change(self.ui.does_redraw());
                    action
                }
            };
            self.act(action, now, pass);
        }
        self.events = events;
    }

    pub(super) fn act(&mut self, action: Option<Action>, now: u64, pass: &mut Pass) {
        match action {
            Some(Action::Power(power)) => {
                pass.add_power(Some(power));
            }
            Some(Action::Sleep) => self.begin_sleep(),
            Some(Action::Wake) => self.wake(now, false),
            Some(Action::Shoot) => self.parts.camera.iter().for_each(Camera::shoot),
            Some(Action::Look) => self.parts.camera.iter().for_each(Camera::look),
            Some(Action::Play) => self.parts.gallery.iter().for_each(Gallery::play),
            Some(Action::Time) => self.parts.timing.ask(self.parts.online.clone(), true),
            Some(Action::Clock(part, steps)) => match clock::move_by(part, steps, self.ui.settings().offset) {
                Ok(utc) => log!("{}", to_moved_line(part, steps, utc)),
                Err(e) => log!("clock: setting it by hand: {e}"),
            },
            Some(Action::Charge) => self.parts.meter.touch(),
            None => {}
        }
    }

    pub(super) fn steer(&mut self, now: u64, is_exiting: bool) {
        self.keep_networks();
        self.parts.storage.ask_safe(self.ui.safe_request());
        self.steer_keys();
        let home = (self.ui.does_home_show(), day_number(clock::local()));
        self.steer_requests(home);
        self.steer_bell();
        self.cycle.note_change(self.ui.update_memo_work(now));
        if let Some(words) = self.scene_words.change(self.ui.scene().to_words()) {
            log!("{}", scene::to_line(words));
        }
        self.steer_pictures(home);
        self.steer_notes();
        self.steer_recording();
        self.steer_voices();
        self.steer_gatherings();
        self.steer_router();
        self.steer_radio();
        self.steer_tilt();
        if self.is_pacing.change(self.ui.is_pacing()).is_some() {
            self.ui.pacing().to_log_lines().iter().for_each(|line| log!("{line}"));
        }
        if let Some(settings) = self.ui.take_changed_settings(is_exiting) {
            self.parts.keeper.keep(settings);
        }
        if let Some(bell) = self.ui.take_changed_bell() {
            self.parts.keeper.keep_bell(bell);
        }
        if let Some(kept) = self.ui.take_changed_rundowns() {
            self.parts.editor.set_topics(kept.clone());
            self.parts.keeper.keep_rundowns(kept);
        }
        let uses = self.ui.take_uses();
        uses.iter().for_each(|entry| self.parts.keeper.add_use(entry.to_word(), sys::realtime_seconds()));
    }

    fn keep_networks(&mut self) {
        if let Some((change, place)) = self.ui.take_network_change() {
            self.parts.keeper.change(change, place);
        }
    }

    fn steer_keys(&mut self) {
        if self.parts.account.take_refused() {
            self.parts.keys.recheck();
        }
        self.parts.keys.check(self.ui.key_check());
        if let Some(key) = self.host.requests.key.take() {
            self.parts.keys.keep(key);
        }
    }

    fn steer_requests(&mut self, home: (bool, u64)) {
        // Wanted before it is asked, so that a request is taken with the shelf the same pass shows.
        self.parts.memos.want(self.ui.shelf(), self.ui.file_changes(), home);
        let requests = self.ui.take_memo_requests();
        if !requests.is_empty() {
            // An entry is forgotten before its files go, so that the scribe never takes up one that is going.
            requests
                .iter()
                .filter_map(Request::delete_target)
                .for_each(|(kind, stem)| self.parts.scribe.forget(kind, stem));
            self.parts.memos.ask(requests);
        }
        for errand in self.ui.take_errands() {
            if let Some(gallery) = &self.parts.gallery {
                gallery.add_errand(errand);
            }
        }
        // Wanted before it is asked, so that the press that woke the device is heard from its start.
        self.parts.chat.want(self.ui.is_awake());
        let mut bids = self.ui.take_chat_bids();
        bids.extend(self.host.requests.chat.drain(..).map(Bid::Text));
        if !bids.is_empty() {
            self.parts.chat.ask(bids);
        }
        self.parts.pokedex.want(self.ui.is_pokedex_wanted());
        let bids = self.ui.take_pokedex_bids();
        if !bids.is_empty() {
            self.parts.pokedex.ask(bids);
        }
        self.parts.web.want(self.ui.is_web_wanted());
        let bids = self.ui.take_web_bids();
        if !bids.is_empty() {
            self.parts.web.ask(bids);
        }
        self.parts.setter.want(self.ui.is_bell_wanted());
        let bids = self.ui.take_bell_bids();
        if !bids.is_empty() {
            self.parts.setter.ask(bids);
        }
        self.parts.curator.want(self.ui.are_rundowns_wanted());
        let bids = self.ui.take_rundown_bids();
        if !bids.is_empty() {
            self.parts.curator.ask(bids);
        }
        let settings = self.ui.settings();
        let (period, limits) = (settings.signal_wake, (Limits::from(&settings), !settings.is_text_only));
        self.parts.signal.want(self.ui.signal_view(), self.ui.is_awake(), period, limits);
        self.parts.signal.want_media(
            self.ui.signal_playing(),
            self.ui.signal_pictures_wanted(),
            Roles::at(settings.palette()),
        );
        self.parts.signal.ask(self.ui.take_signal_commands());
        self.parts.calls.want(self.ui.far_picture_room());
        self.ui.take_call_orders().into_iter().for_each(|order| self.parts.calls.ask(order));
        self.parts.signal.set_calling(self.ui.are_calls_wanted(self.parts.calls.is_standing()));
        self.parts.drafts.want(self.ui.are_drafts_wanted());
        self.parts.drafts.ask(self.ui.take_signal_bids());
    }

    fn steer_bell(&mut self) {
        if let Some(at) = self.bell_at.change(self.ui.next_bell_s()) {
            match self.bell.arm(at) {
                Ok(true) => {}
                // The clocks were set since the alarm was last heard, which its reading takes in: it is set after that.
                Ok(false) => self.bell_at.forget(),
                Err(e) => log!("bell: the alarm: {e}"),
            }
        }
        if let Some(is_ringing) = self.is_ringing.change(self.ui.does_ring_sound()) {
            log!("{}", bell::to_ring_line(is_ringing));
            self._ring_sound = is_ringing.then(ring_sound).flatten();
        }
    }

    fn steer_pictures(&mut self, (home, day): (bool, u64)) {
        let (viewfinder, request) = (self.ui.camera_request(), self.ui.gallery_request());
        self.cycle.note_viewfinder(viewfinder, sys::monotonic_ns());
        let is_reserved = self.ui.is_camera_reserved();
        self.parts.camera.iter().for_each(|camera| camera.set_reserve(is_reserved));
        self.parts.camera.iter().for_each(|camera| camera.want(viewfinder));
        self.parts.camera.iter().for_each(|camera| camera.pause(self.ui.is_reviewing()));
        let is_picture_wanted = self.parts.calls.wants_picture();
        self.parts.camera.iter().for_each(|camera| camera.want_call_pictures(is_picture_wanted));
        self.parts.gallery.iter().for_each(|gallery| gallery.want(request, self.ui.pixel_changes()));
        self.parts.gallery.iter().for_each(|gallery| gallery.set_home(home, day));
        self.parts.magic.want(self.ui.are_photos_wanted());
    }

    fn steer_notes(&mut self) {
        let shelf = self.ui.shelf();
        self.parts.scribe.want(scribe::is_wanted(shelf));
        self.parts.editor.want(editor::is_wanted(shelf));
        if mem::take(&mut self.host.requests.edition) {
            self.parts.editor.make_edition();
        }
    }

    fn steer_recording(&mut self) {
        if let (Some((is_on, at_ns)), Some(recorder)) = (self.host.requests.record.take(), &mut self.parts.recorder) {
            if is_on {
                recorder.begin(at_ns);
                self.cycle.note_change(true);
            } else {
                recorder.end(at_ns);
            }
            log!("{}", to_asked_line(is_on));
        }
    }

    fn steer_voices(&mut self) {
        self.parts.translator.want(self.ui.session_request());
        self.cycle.note_change(self.ui.end_unheard_reading());
        self.parts.reader.want(self.ui.reading_request(self.parts.chat.voice()));
        if let Some(is_searching) = self.is_searching.change(self.ui.does_search_sound()) {
            log!("{}", to_search_line(is_searching));
            self._search_sound = is_searching.then(search_sound).flatten();
        }
        if self.notices.change(self.ui.notices()).is_some() {
            log!("{NOTICE_LINE}");
            self._notice_sound = notice_sound();
        }
    }

    fn steer_gatherings(&mut self) {
        self.parts.cost.want(self.ui.cost_request());
        self.parts.about.want(self.ui.is_about_wanted().then_some(()));
        self.parts.rundown.want(self.ui.is_rundown_on());
    }

    fn steer_router(&mut self) {
        let settings = self.ui.settings();
        self.parts.router.want(settings.lends_modem(self.parts.modem.is_usable()), settings.uses_tunnel);
        let handed = self.host.requests.tunnel.take().map(|config| Order::Keep(config, Asker::Console));
        for order in self.ui.take_router_order().into_iter().chain(handed) {
            self.parts.router.ask(order);
        }
    }

    fn steer_radio(&mut self) {
        let pins = [self.ui.take_pin(), self.host.requests.pin.take()];
        let (attach, commands) = (self.host.requests.attach.take(), mem::take(&mut self.host.requests.at));
        let pass = self.ui.radio();
        let trial = self.ui.joining();
        let parts = &self.parts;
        let debts = app::Debts {
            editor: !parts.editor.is_off(),
            reader: parts.reader.is_owing(),
            chat: parts.chat.is_owing(),
            signal: !parts.signal.is_off(),
            router: parts.router.is_routing(),
        };
        if let Some(grant) = parts.radio.set_resting(debts.is_resting(self.ui.is_awake())) {
            for grantee in debts.to_grantees() {
                match grantee {
                    app::Grantee::Editor => parts.editor.grant_radio(&grant),
                    app::Grantee::Signal => parts.signal.grant_radio(&grant),
                }
            }
        }
        let (power, link, is_cell_only) = self.ui.console_radio();
        parts.radio.want(power);
        parts.radio.want_link(link);
        parts.radio.want_cell(is_cell_only);
        parts.radio.want_route(debts.router);
        parts.modem.want(self.ui.settings().is_cellular);
        parts.modem.keep_pin(self.ui.settings().is_pin_kept);
        parts.modem.set_awake(self.ui.is_awake());
        for pin in pins.into_iter().flatten() {
            parts.modem.enter_pin(pin);
        }
        if let Some(holds_attach) = attach {
            parts.modem.hold_attach(holds_attach);
        }
        for command in commands {
            parts.modem.send(command);
        }
        if mem::take(&mut self.host.requests.time) {
            parts.timing.ask(parts.online.clone(), false);
        }
        parts.radio.want_scan(pass);
        parts.radio.want_trial(trial);
    }

    fn steer_tilt(&mut self) {
        let does_heed = self.ui.does_heed(self.parts.camera.is_some(), self.parts.gallery.is_some());
        let console_pose = app::to_console_pose(does_heed, self.host.requests.pose.take())
            .inspect(|pose| log!("{}", to_named_line(*pose)));
        let is_tilt_wanted = app::is_tilt_wanted(does_heed, self.ui.is_awake(), &self.light, self.ui.tilts());
        if self.tilt_attempt.is_due(is_tilt_wanted, self.tilt.is_none()) {
            self.tilt = watch(&mut self.epoll, Source::Tilt, "accelerometer", Tilt::open(), Tilt::fd);
        }
        let is_switched = self.tilt.as_mut().is_some_and(|tilt| tilt.want(is_tilt_wanted));
        for pose in app::to_poses(is_switched, console_pose) {
            self.cycle.note_change(turn(pose, &mut self.ui, &self.parts.camera, &self.parts.gallery));
        }
    }

    pub(super) fn go_down(&mut self, exit: Option<Power>) {
        if let Some(power) = exit {
            if let Some(console) = &mut self.host.console {
                console.flush();
            }
            log!("{}", to_exit_line(power));
            let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            self.parts.meter.save(deadline);
            self.parts.keeper.settle(deadline);
            if let Err(e) = self.parts.radio.shut(deadline) {
                log!("radio: leaving before the restart: {e}");
            }
            if let Err(e) = self.parts.modem.shut(deadline) {
                log!("cell: stopping the modem before the restart: {e}");
            }
            logs::settle(deadline);
            log!("reboot refused: {}", sys::reboot(power));
        }
        let Some(doze) = self.ui.doze_mut() else {
            return;
        };
        while let Some(step) = doze.next_step(self.parts.is_quiet()) {
            match step {
                Step::Suspend => {
                    if let Some(console) = &mut self.host.console {
                        console.flush();
                    }
                    logs::settle(Instant::now() + SHUTDOWN_TIMEOUT);
                    // A part may have begun work in the flush's seconds: the parts are asked anew.
                    sleep::suspend(doze, self.parts.is_quiet());
                }
                Step::Ask => self.parts.gate.want(Some(())),
            }
        }
    }
}
