use super::{App, Source};
use crate::display::backlight;
use crate::display::recorder::Recorder;
use crate::storage::logs;
use crate::sys::{self, klog};
use crate::{clock, log, sound};
use r1_funcore::domain::app;
use r1_funcore::domain::app::cycle::Pass;
use r1_funcore::domain::app::light::{NO_DEVICE_LINE, to_dark_line, to_level_line};
use r1_funcore::gfx::frame_stats::frame_line;
use r1_funcore::gfx::palette::Roles;
use r1_funcore::gfx::panel::Geometry;
use r1_funcore::gfx::{FrameKind, Surface};
use r1_funcore::hw::sound::volume::to_levels_line;
use r1_funcore::logs::{LineFile, are_frames_read};
use std::io;

impl App {
    pub(super) fn follow_settings(&mut self) {
        clock::set_offset(self.ui.settings().offset);
        if let Some(roles) = self.roles.change(Roles::at(self.ui.settings().palette())) {
            self.parts.editor.set_roles(roles);
            self.parts.scribe.set_roles(roles);
            self.parts.pokedex.set_roles(roles);
        }
        let logging = self.ui.logging_setting();
        self.parts.logs.want(logging);
        if let Some(logging) = logging.and_then(|logging| self.logging.change(logging)) {
            match klog::set(logging) {
                Ok(()) => log!("{}", logging.to_line()),
                Err(e) => log!("logging: the kernel's switches: {e}"),
            }
        }
        self.parts.editor.set_schedule(self.ui.edition_setting());
        if let Some(settings) = self.settings.change(self.ui.settings()) {
            self.parts.account.set_terms(settings.terms);
            self.parts.chat.set_settings(settings);
            self.parts.drafts.set_voice(settings.is_said_voice);
            self.parts.pokedex.set_language(settings.pokedex);
            self.parts.drive.iter().for_each(|drive| drive.set_shown(settings.is_drive_shown));
        }
    }

    pub(super) fn tick(&mut self, now: u64, pass: &mut Pass) {
        self.cycle.note_change(self.ui.set_time(sys::realtime_seconds()));
        self.cycle.note_change(self.ui.fade(now));
        let action = self.ui.tick(now);
        self.act(action, now, pass);
        if let Some(is_dark) = self.light.set_dark(self.ui.is_awake(), self.ui.is_dark()) {
            log!("{}", to_dark_line(is_dark));
            self.set_screen(!is_dark);
        }
        if self.ui.is_playing()
            && let Some(gallery) = &self.parts.gallery
            && gallery.take_frame(self.ui.roll_picture_mut(), (self.cycle.flipped_ns(), sys::monotonic_ns()))
        {
            self.ui.note_roll_frame();
        }
        if let Some(load) = self.boost.follow(self.ui.is_awake(), self.ui.is_straining())
            && let Err(e) = sys::set_clock_floor(load)
        {
            log!("clock floor: {e}");
        }
    }

    pub(super) fn draw(&mut self, now: u64) {
        // The panel's init blocks 0.6 s: here, after the tick, a release made meanwhile is read before a hold counts.
        if self.light.is_coming_back(self.ui.is_awake()) {
            self.power_panel(true);
        }
        if let Some(recorder) = &mut self.parts.recorder {
            recorder.set_dark(self.light.is_blank(self.ui.is_awake()), sys::monotonic_ns());
        }
        let is_due = self.cycle.begin_draw(self.ui.is_animating(), self.ui.is_awake(), &self.light);
        if is_due && let Some(screen) = &mut self.parts.fb {
            let start_ns = sys::monotonic_ns();
            // While a recording runs a frame is drawn whole into a page of cached memory, which the recorder reads fast, and copied onto the screen; otherwise it is drawn onto the screen's page where it differs from the frame that page shows, whole where that is not known.
            let mut page = self.parts.recorder.as_mut().and_then(Recorder::page);
            let frame = match &mut page {
                Some(pixels) => {
                    let Geometry { width, height, format, .. } = screen.geometry();
                    let mut surface =
                        Surface::new(pixels, width, height, width as usize, format).expect("a page holds the screen");
                    let frame = self.ui.render_over(&mut surface, now, None);
                    let mut front = screen.frame();
                    front.blit(front.bounds(), 0, 0, width as usize, pixels);
                    frame
                }
                None => {
                    let back_frame = screen.back_frame();
                    self.ui.render_over(&mut screen.frame(), now, back_frame)
                }
            };
            let draw_ns = sys::monotonic_ns();
            let result = match frame {
                FrameKind::New(frame) => Some(screen.present(frame)),
                FrameKind::Same(frame) => {
                    screen.skip_back(frame);
                    None
                }
            };
            match result {
                // A frame that changed nothing is the one on screen, so a picture is up: no refresh shows it anew, and what moves is drawn again at the panel's next refresh.
                None => {
                    self.cycle.note_unchanged(start_ns, draw_ns);
                    self.light.note_frame();
                }
                Some(Ok(())) => {
                    let present_ns = sys::monotonic_ns();
                    self.parts.camera.iter().for_each(|camera| camera.set_flipped(present_ns));
                    if let (Some(recorder), Some(pixels)) = (&mut self.parts.recorder, page.take()) {
                        recorder.add_frame(pixels, present_ns);
                    }
                    self.ui.note_frame_presented(start_ns, draw_ns, present_ns);
                    if let Some(number) = self.cycle.note_shown(start_ns, draw_ns, present_ns)
                        && are_frames_read(self.ui.logging_setting(), self.are_lines_read())
                    {
                        frame_line(&mut self.frame_line, number, start_ns, draw_ns, present_ns);
                        logs::add_line(LineFile::Frames, &self.frame_line);
                        if let Some(console) = &mut self.host.console {
                            console.log(&self.frame_line);
                        }
                    }
                    self.light.note_frame();
                }
                Some(Err(e)) => {
                    log!("present failed, running without a screen: {e}");
                    if let (Some(recorder), Some(pixels)) = (&mut self.parts.recorder, page.take()) {
                        recorder.give_back(pixels);
                    }
                    self.parts.fb = None;
                }
            }
        }
        self.cycle.end_draw(self.ui.is_animating()).iter().for_each(|line| log!("{line}"));
    }

    pub(super) fn set_levels(&mut self) {
        if let Some(brightness) = self.light.level(self.ui.is_awake(), self.ui.backlight()) {
            let result = backlight::set(brightness);
            let has_device = !result.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::NotFound);
            match result {
                Ok(level) => log!("{}", to_level_line(brightness, level)),
                Err(_) if !has_device => log!("{NO_DEVICE_LINE}"),
                Err(e) => log!("backlight: {e}"),
            }
            self.light.note_level(brightness, has_device);
        }
        let settings = self.ui.settings();
        if let Some((volume, sounds)) = self.levels.change((settings.volume, settings.sounds)) {
            sound::set_volume(volume, sounds);
            log!("{}", to_levels_line(volume, sounds));
        }
    }

    pub(super) fn wait(&mut self) {
        if let Some(console) = &mut self.host.console
            && let Some(backlog) = console.backlog()
            && let Err(e) = self.epoll.set_writable(console.fd(), app::to_token(Source::Console as usize), backlog)
        {
            log!("watching usb console for room: {e}");
        }
        let should_suspend = self.ui.doze_mut().is_some_and(|doze| doze.should_suspend(self.parts.is_quiet()));
        let due = [klog::has_pending(), should_suspend];
        let limit = self.cycle.to_wait(due, self.ui.is_awake(), self.ui.deadline(), sys::monotonic_ns());
        self.epoll.wait_into(limit, &mut self.ready).unwrap_or_else(|e| panic!("epoll wait: {e}"));
    }
}
