use super::{App, Source, mark_slot, turn};
use crate::input::Inputs;
use crate::sys::devnodes::{self, DEV};
use crate::sys::uevent::Monitor;
use crate::sys::{self, Epoll};
use crate::{log, sound, usb};
use r1_funcore::domain::aloud::ReadingSource;
use r1_funcore::domain::aloud::state::Report;
use r1_funcore::domain::app;
use r1_funcore::domain::app::cycle::{Cycle, Pass};
use r1_funcore::domain::bell::RINGS_LINE;
use r1_funcore::domain::router::Asker;
use r1_funcore::domain::signal::robot::Reply;
use r1_funcore::gfx::Picture;
use r1_funcore::hw::power::doze::CABLE_LINE;
use r1_funcore::hw::uevent::Uevent;
use r1_funcore::input::pose::{to_held_line, to_picked_up_line};
use r1_funcore::input::to_added_line;
use r1_funcore::openai::ledger::SummaryKind;
use r1_funcore::storage::safe::THRICE_WRONG_LINE;
use r1_funcore::ui::{Action, Reading, Ui};
use std::mem;
use std::path::Path;

fn handle_uevent(event: &Uevent, inputs: &mut Inputs, epoll: &mut Epoll, ui: &mut Ui, cycle: &mut Cycle) {
    if let Some(node) = event.node() {
        let kind = if node.is_block { libc::S_IFBLK } else { libc::S_IFCHR };
        if let Err(e) = devnodes::make_node(node.name, kind, node.major, node.minor) {
            log!("device node {}: {e}", node.name);
        }
        if node.is_input
            && let Some((index, fd)) = inputs.add(&Path::new(DEV).join(node.name))
        {
            match epoll.add(fd, index as u64) {
                Ok(()) => log!("{}", to_added_line(inputs.name(index))),
                Err(e) => log!("watching input {index}: {e}"),
            }
            cycle.note_change(ui.set_inputs(&inputs.names()));
        }
    }
}

impl App {
    pub(super) fn serve(&mut self, token: u64, now: u64, pass: &mut Pass) {
        let Some(source) = app::parse_token(token, Source::ALL.len()).map(|place| Source::ALL[place]) else {
            return self.serve_input(token as usize);
        };
        match source {
            #[cfg(feature = "debug-shell")]
            Source::Shell => {
                pass.add_power(self.host.answer_shell(&mut self.ui, self.parts.modem.is_usable()));
            }
            Source::Console => {
                pass.add_power(self.host.serve(&mut self.epoll, &mut self.ui, self.parts.modem.is_usable()));
            }
            Source::Uevent => self.serve_uevents(),
            Source::Battery => self.serve_battery(now),
            Source::Storage => self.serve_storage(now, pass),
            Source::Settings => {
                if let Some(stored) = self.parts.keeper.take_stored() {
                    self.cycle.note_change(self.ui.recall(stored.settings));
                    self.cycle.note_change(self.ui.set_known(stored.known));
                    self.cycle.note_change(self.ui.recall_bell(stored.bell));
                    self.cycle.note_change(self.ui.recall_rundowns(stored.topics));
                    self.parts.editor.set_topics(self.ui.kept_rundowns().clone());
                    self.parts.keys.begin(stored.key);
                }
            }
            Source::Keys => self.serve_keys(now, pass),
            Source::Radio => self.serve_radio(now),
            Source::Router => self.serve_router(now, pass),
            Source::Cell => {
                if let Some(status) = self.parts.modem.latest().status {
                    self.cycle.note_change(self.ui.set_modem(status));
                }
            }
            Source::Camera => self.serve_camera(now),
            Source::Gallery => self.serve_gallery(now),
            Source::Magic => self.serve_magic(now),
            Source::Memos => self.serve_memos(now),
            Source::Scribe => self.cycle.note_change(self.ui.take_scribe_report(self.parts.scribe.latest(), now)),
            Source::Talk => self.serve_talk(now),
            Source::Editor => self.cycle.note_change(self.ui.take_editor_report(self.parts.editor.latest(), now)),
            Source::Aloud => {
                let Report { text, reply, signal, heard } = self.parts.reader.latest();
                if let Some((round, count)) = heard {
                    self.parts.chat.set_heard(round, count);
                }
                for (round, source, phase) in [text, reply, signal].into_iter().flatten() {
                    match source {
                        ReadingSource::Text(_) | ReadingSource::Web => {
                            self.cycle.note_change(self.ui.set_reading_phase(round, phase, now));
                        }
                        ReadingSource::Reply => self.parts.chat.set_voice_phase(round, phase),
                        ReadingSource::Signal => {
                            self.cycle.note_change(self.ui.set_signal_reading_phase(round, phase));
                        }
                    }
                }
            }
            Source::Chat => self.serve_chat(now),
            Source::Pokedex => {
                let report = self.parts.pokedex.latest();
                self.cycle.note_change(self.ui.set_pokedex(report.phase, report.article, now));
            }
            Source::Web => {
                let report = self.parts.web.latest();
                self.cycle.note_change(self.ui.set_web(report.phase, report.page, now));
            }
            Source::Setter => {
                let report = self.parts.setter.latest();
                self.cycle.note_change(self.ui.take_bell_report(report, now));
            }
            Source::Curator => {
                let report = self.parts.curator.latest();
                self.cycle.note_change(self.ui.take_rundowns_report(report, now));
            }
            Source::Signal => {
                let mut report = self.parts.signal.latest();
                if let Some(reply) = report.reply.take() {
                    self.parts.chat.answer_readout(reply);
                }
                self.cycle.note_change(self.ui.take_signal_report(report, now));
                let mut report = self.parts.signal.take_call_report();
                self.parts.calls.take_news(&mut report);
                self.cycle.note_change(self.ui.take_call_log(report.log, report.callees));
            }
            Source::Calls => {
                let report = self.parts.calls.latest(self.ui.far_picture_mut());
                self.parts.signal.ask(report.commands);
                if let Some(view) = report.view {
                    let (is_changed, action) = self.ui.set_call(view, now);
                    self.cycle.note_change(is_changed);
                    self.act(action, now, pass);
                }
                if report.has_picture {
                    self.cycle.note_change(self.ui.note_far_frame());
                }
            }
            Source::Drafts => {
                let report = self.parts.drafts.latest();
                self.cycle.note_change(self.ui.set_signal_draft(report.phase, report.draft, now));
                self.cycle.note_change(self.ui.set_signal_note(report.note, now));
            }
            Source::Rundown => {
                if let Some(status) = self.parts.rundown.latest().status {
                    self.cycle.note_change(self.ui.set_rundown(status));
                }
            }
            Source::Cost => {
                if let Some((number, figures)) = self.parts.cost.latest() {
                    self.cycle.note_change(self.ui.set_cost(number, figures));
                }
            }
            Source::About => {
                if let Some(sources) = self.parts.about.latest() {
                    self.cycle.note_change(self.ui.set_about(sources));
                }
            }
            Source::Frames => usb::frames::serve(),
            Source::Microphone => self.cycle.note_change(self.ui.set_microphone(sound::take_peak())),
            Source::Time => {
                let report = self.parts.timing.latest();
                if let Some(answer) = report.answer {
                    self.cycle.note_change(self.ui.take_time_answer(answer, now));
                }
                if let Some(offset) = report.offset {
                    self.cycle.note_change(self.ui.set_offset(offset));
                }
            }
            Source::Suspend => {
                // An answer that comes after the button has ended the sleep is for nobody; once it has come, the next question is asked.
                if let (Some(count), Some(doze)) = (self.parts.gate.take_answer(), self.ui.doze_mut()) {
                    doze.take_answer(count);
                }
            }
            Source::Logs => {
                if let Some(state) = self.parts.logs.latest() {
                    self.cycle.note_change(self.ui.set_logs(&state));
                }
            }
            Source::Tilt => self.serve_tilt(),
            Source::Bell => self.serve_bell(now, pass),
        }
    }

    fn serve_bell(&mut self, now: u64, pass: &mut Pass) {
        self.bell.acknowledge();
        self.bell_at.forget();
        self.cycle.note_change(true);
        let action = self.ui.ring(sys::realtime_seconds(), now);
        if action == Some(Action::Wake) {
            log!("{RINGS_LINE}");
        }
        self.act(action, now, pass);
    }

    fn serve_storage(&mut self, now: u64, pass: &mut Pass) {
        let report = self.parts.storage.latest();
        if let Some(progress) = &report.progress {
            self.cycle.note_change(self.ui.set_progress(progress, now));
        }
        if let Some((number, change)) = report.change {
            self.cycle.note_change(self.ui.take_safe_change(number, change, now));
        }
        self.follow_safe_exit(pass);
    }

    fn follow_safe_exit(&self, pass: &mut Pass) {
        if pass.add_power(self.ui.safe_way_down()) {
            log!("{THRICE_WRONG_LINE}");
        }
    }

    fn serve_uevents(&mut self) {
        for event in self.parts.monitor.as_ref().map(Monitor::read).unwrap_or_default() {
            handle_uevent(&event, &mut self.parts.inputs, &mut self.epoll, &mut self.ui, &mut self.cycle);
            if self.slot.is_due(event.is_block_add()) {
                self.slot.note_try(mark_slot(&self.cmdline));
            }
            if event.is_block_add() {
                self.parts.storage.device_added();
            }
            if let (Some(drive), Some(state)) = (&self.parts.drive, event.host) {
                drive.set_host_state(state);
            }
            // A host on the port makes the cable a computer's, which charges the battery to a ceiling.
            if let Some(state) = event.host {
                self.parts.meter.set_host_state(state);
            }
            if event.is_light_add() {
                self.light.note_device();
            }
        }
    }

    fn serve_battery(&mut self, now: u64) {
        if let Some(reading) = self.parts.meter.latest().reading {
            let (is_changed, lines, is_plugged) = self.ui.set_reading(&reading, now);
            self.cycle.note_change(is_changed);
            lines.iter().for_each(|line| log!("{line}"));
            if is_plugged {
                log!("{CABLE_LINE}");
                self.wake(now, true);
            }
        }
    }

    fn serve_keys(&mut self, now: u64, pass: &mut Pass) {
        let report = self.parts.keys.latest();
        self.cycle.note_change(self.ui.take_key_report(&report, now));
        if report.is_flashed {
            self.parts.keeper.keep_flash_stamp();
        }
        if let Some(result) = report.store {
            pass.add_power(self.host.answer_keep(result, &mut self.ui, self.parts.modem.is_usable()));
        }
    }

    fn serve_router(&mut self, now: u64, pass: &mut Pass) {
        let report = self.parts.router.latest();
        self.cycle.note_change(self.ui.take_router_report(&report, now));
        // What the console handed over is answered there; what the window read, the interface took above.
        if let Some((Asker::Console, result)) = report.store {
            pass.add_power(self.host.answer_keep(result, &mut self.ui, self.parts.modem.is_usable()));
        }
    }

    fn serve_radio(&mut self, now: u64) {
        let report = self.parts.radio.latest();
        self.parts.account.drop_ended_connections();
        self.cycle.note_change(self.ui.set_link(report.link));
        if let Some(scan) = report.scan {
            self.cycle.note_change(self.ui.set_scan(scan.pass, scan.networks.as_deref()));
        }
        if let Some(trial) = report.trial {
            self.cycle.note_change(self.ui.take_join_outcome(trial.number, trial.outcome, now));
        }
    }

    fn serve_camera(&mut self, now: u64) {
        if let Some(camera) = &self.parts.camera {
            let (viewfinder, picture) = self.ui.camera_picture_mut();
            let report = camera.latest(viewfinder, picture);
            if camera.take_call_picture(&mut self.call_picture) {
                self.parts.calls.add_picture(&mut self.call_picture);
            }
            self.parts.magic.take_orders(report.orders);
            if let Some(shot) = report.shot {
                self.cycle.note_change(self.ui.take_shot(shot, now));
            }
            if let Some(photo) = report.taken {
                self.ui.note_taken(photo);
            }
            if let Some(development) = report.development {
                self.cycle.note_change(self.ui.take_development(development));
            }
            if let Some(review) = report.review {
                self.cycle.note_change(self.ui.take_review(review));
            }
            if let Some(info) = report.picture {
                let (is_changed, is_paced) = self.ui.note_camera_frame();
                self.cycle.note_change(is_changed);
                if is_paced {
                    self.cycle.take_picture(viewfinder, info.at_ns, info.is_in_step);
                }
            }
            // The stop for a photo comes after every picture the stream handed before it, so it darkens one taken with it.
            if report.is_shooting {
                self.cycle.note_change(self.ui.shut_finder());
                self.cycle.stop_stream();
            }
            self.cycle.note_change(self.ui.set_recording(report.is_recording, now));
            if let Some(outcome) = report.outcome {
                self.cycle.note_change(self.ui.take_outcome(&outcome, now));
            }
            if let Some(note) = report.note {
                self.cycle.note_change(self.ui.take_gallery_note(note, now));
            }
            if let Some(code) = report.code
                && let Some(scan) = self.ui.take_scan(&code.0, now)
            {
                log!("{}", scan.to_log_line(code.0.len()));
                self.cycle.note_change(true);
            }
        }
    }

    fn serve_gallery(&mut self, now: u64) {
        if let Some(gallery) = &self.parts.gallery {
            let item = self.ui.visible_roll_request().and_then(|request| request.large_item());
            let mut picture = Picture::default();
            let report = gallery.latest(item, &mut picture);
            self.parts.magic.take_orders(report.orders);
            if let Some(camera) = &self.parts.camera {
                report.copies.into_iter().for_each(|photo| camera.copy(photo));
            }
            if let Some(shown) = report.picture {
                self.ui.take_roll_picture(shown, &mut picture);
                self.cycle.note_change(true);
            }
            self.ui.set_playing(report.is_playing);
            if let Some((kind, count)) = report.item_count {
                self.cycle.note_change(self.ui.set_items(kind, count));
            }
            for entry in report.thumbnails {
                self.cycle.note_change(self.ui.set_thumbnail(entry.round, entry.item, entry.thumbnail));
            }
            if let Some(note) = report.note {
                self.cycle.note_change(self.ui.take_gallery_note(note, now));
            }
            if let Some(sending) = report.send {
                self.cycle.note_change(self.ui.send_item(sending, now));
            }
            if let Some(rolls) = report.tally {
                self.cycle.note_change(self.ui.set_rolls(rolls));
            }
        }
    }

    fn serve_magic(&mut self, now: u64) {
        let mut report = self.parts.magic.latest();
        if let Some(gallery) = &self.parts.gallery {
            gallery.add_redraws(mem::take(&mut report.redraws));
        }
        if let (Some(camera), Some((photo, view))) = (&self.parts.camera, self.ui.review_art(&report.kept)) {
            camera.show_art(photo, view);
        }
        self.cycle.note_change(self.ui.take_magic_report(&report, now));
    }

    fn serve_memos(&mut self, now: u64) {
        let mut report = self.parts.memos.latest();
        self.parts.scribe.set_recording(report.recording.clone());
        for (kind, stem) in mem::take(&mut report.new_entries) {
            self.parts.scribe.add_new(kind, stem);
        }
        self.cycle.note_change(self.ui.take_memos_report(report, now));
    }

    fn serve_talk(&mut self, now: u64) {
        let mut report = self.parts.translator.latest();
        for stem in mem::take(&mut report.new_stems) {
            self.parts.scribe.add_new(SummaryKind::Talk, stem);
        }
        self.cycle.note_change(self.ui.take_talk_report(&report, now));
    }

    fn serve_chat(&mut self, now: u64) {
        let mut report = self.parts.chat.latest();
        if let Some((round, script)) = report.reply.take() {
            self.parts.reader.set_reply(round, script);
        }
        let (is_changed, readout) = self.ui.take_chat_report(report, now);
        self.cycle.note_change(is_changed);
        match readout {
            Some(Reading::Told(told)) => self.parts.chat.answer_readout(Reply { told, draft: None }),
            Some(Reading::Ask(question)) => self.parts.signal.ask_question(question),
            None => {}
        }
    }

    fn serve_tilt(&mut self) {
        let reading = self.tilt.as_mut().map(|tilt| (tilt.read(), tilt.posture.last(), tilt.posture.gravity()));
        match reading {
            Some((Ok((pose_change, is_lifted)), last, (x, y))) => {
                if let Some(pose) = pose_change {
                    log!("{}", to_held_line(pose, last));
                    self.cycle.note_change(turn(pose, &mut self.ui, &self.parts.camera, &self.parts.gallery));
                }
                self.cycle.note_change(self.ui.set_tilt(x, y));
                if self.ui.take_lift(is_lifted) {
                    log!("{}", to_picked_up_line(last));
                }
            }
            Some((Err(e), ..)) => {
                log!("accelerometer dropped: {e}");
                if let Some(tilt) = self.tilt.take()
                    && let Err(e) = self.epoll.remove(tilt.fd())
                {
                    log!("accelerometer: ending its watch: {e}");
                }
            }
            None => {}
        }
    }

    fn serve_input(&mut self, place: usize) {
        if let Err(e) = self.parts.inputs.read(place, &mut self.events) {
            log!("input {place} dropped: {e}");
            if let Some(fd) = self.parts.inputs.fd(place)
                && let Err(e) = self.epoll.remove(fd)
            {
                log!("input {place}: ending its watch: {e}");
            }
            self.parts.inputs.close(place);
            self.cycle.note_change(self.ui.set_inputs(&self.parts.inputs.names()));
        }
    }
}
