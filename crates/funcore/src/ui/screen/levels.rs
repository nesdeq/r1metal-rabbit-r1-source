use super::View;
use super::bar::BarWords;
use crate::domain::scene::{BRIGHTNESS, VOLUME};
use crate::domain::settings::{self, BRIGHTEST, DIMMEST};
use crate::gfx::icon::Icon;
use crate::gfx::motion::{Edge, NUDGE_SPEED};
use crate::hw::sound::volume::{self, LOUDEST, QUIETEST};
use crate::ui::Ui;
use crate::ui::gauge::{Gauge, lit_blocks};
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::kit::read_icon;
use crate::ui::screen::Screen;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

pub(crate) const BRIGHTNESS_BLOCKS: usize = ((BRIGHTEST - DIMMEST) / settings::BRIGHTNESS_STEP) as usize + 1;
pub(crate) const VOLUME_BLOCKS: usize = ((LOUDEST - QUIETEST) / volume::STEP) as usize;
pub(super) const SOUNDS: &str = "SOUNDS";
pub(super) const SUN_ICON: &str = icon!("brightness_6");
pub(super) const SPEAKER_ICON: &str = icon!("volume_up");
const CHIME_ICON: &str = icon!("notification_sound");
pub(super) const ICONS: [&str; 3] = [SUN_ICON, SPEAKER_ICON, CHIME_ICON];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Channel {
    Volume,
    Sounds,
}

pub(crate) struct Gauges {
    pub(crate) lines: Grid,
    sun: Icon,
    speaker: Icon,
    chime: Icon,
    pub(crate) level: [Edge; BRIGHTNESS_BLOCKS],
    pub(crate) volume: [Edge; VOLUME_BLOCKS],
    pub(crate) sounds: [Edge; VOLUME_BLOCKS],
    channel: Channel,
    pub(crate) sway: Edge,
}

impl Gauges {
    pub(crate) fn new() -> Gauges {
        Gauges {
            lines: Grid::empty(),
            sun: read_icon(SUN_ICON),
            speaker: read_icon(SPEAKER_ICON),
            chime: read_icon(CHIME_ICON),
            level: [Edge::still(0.0); BRIGHTNESS_BLOCKS],
            volume: [Edge::still(0.0); VOLUME_BLOCKS],
            sounds: [Edge::still(0.0); VOLUME_BLOCKS],
            channel: Channel::Volume,
            sway: Edge::still(0.0),
        }
    }
}

fn write_speech(ui: &Ui, text: &mut String) {
    let settings = ui.settings;
    let _ = match (ui.screen, ui.gauges.channel) {
        (Some(Screen::Volume), Channel::Volume) if settings.volume == 0 => text.write_str("I'm silent."),
        (Some(Screen::Volume), Channel::Volume) => write!(text, "My voice at {}%.", settings.volume),
        (Some(Screen::Volume), Channel::Sounds) if settings.sounds == 0 => text.write_str("My sounds are off."),
        (Some(Screen::Volume), Channel::Sounds) => write!(text, "My sounds at {}%.", settings.sounds),
        _ => write!(text, "My screen at {}%.", settings.brightness),
    };
}

fn volume_aims(level: u8) -> [f32; VOLUME_BLOCKS] {
    lit_blocks(usize::from(level.saturating_sub(QUIETEST) / volume::STEP))
}

impl Ui {
    pub(super) fn open_brightness(&mut self) {
        self.gauges.level = self.block_aims().map(Edge::still);
        self.screen = Some(Screen::Brightness);
        self.show_settings();
    }

    pub(super) fn open_volume(&mut self) {
        (self.gauges.volume, self.gauges.sounds) =
            (self.step_aims().map(Edge::still), self.sound_aims().map(Edge::still));
        self.gauges.channel = Channel::Volume;
        self.screen = Some(Screen::Volume);
        self.show_settings();
    }

    pub(super) fn adjust(&mut self, steps: i32) {
        let previous = self.settings;
        self.settings = match (self.screen, self.gauges.channel) {
            (Some(Screen::Volume), Channel::Volume) => previous.step_volume(steps),
            (Some(Screen::Volume), Channel::Sounds) => previous.step_sounds(steps),
            _ => previous.step_brightness(steps),
        };
        if self.settings == previous && steps != 0 {
            self.gauges.sway.speed += steps.signum() as f32 * NUDGE_SPEED;
        }
        self.deal_settings();
    }

    pub(super) fn choose_next_channel(&mut self) {
        self.gauges.channel = match self.gauges.channel {
            Channel::Volume => Channel::Sounds,
            Channel::Sounds => Channel::Volume,
        };
        self.deal_settings();
    }

    pub(crate) fn settings_place(&self) -> (usize, usize) {
        match self.screen {
            Some(Screen::Volume) => (Channel::Sounds as usize + 1, self.gauges.channel as usize),
            Some(Screen::Clock) => self.clock_parts(),
            _ => (1, 0),
        }
    }

    pub(crate) fn setting_item(&self, index: usize) -> Option<Item> {
        let figure = |name: &str, level: u8, icon| Item::figure(name, format!("{level}%")).with_icon(icon);
        Some(match (self.screen?, index) {
            (Screen::Brightness, 0) => figure(BRIGHTNESS, self.settings.brightness, SUN_ICON),
            (Screen::Volume, 0) => figure(VOLUME, self.settings.volume, SPEAKER_ICON),
            (Screen::Volume, 1) => figure(SOUNDS, self.settings.sounds, CHIME_ICON),
            (Screen::Clock, _) => return self.part_item(index),
            _ => return None,
        })
    }

    pub(crate) fn setting_gauge(&self, index: usize) -> Option<Gauge<'_>> {
        let gauges = &self.gauges;
        let gauge = |blocks, icon| Gauge { icon, name: "", value: "", blocks, is_set: true, sway: gauges.sway.at };
        match (self.screen?, index) {
            (Screen::Brightness, 0) => Some(gauge(&gauges.level[..], &gauges.sun)),
            (Screen::Volume, 0) => Some(gauge(&gauges.volume[..], &gauges.speaker)),
            (Screen::Volume, 1) => Some(gauge(&gauges.sounds[..], &gauges.chime)),
            _ => None,
        }
    }

    pub(crate) fn block_aims(&self) -> [f32; BRIGHTNESS_BLOCKS] {
        lit_blocks(usize::from(self.settings.brightness.saturating_sub(DIMMEST) / settings::BRIGHTNESS_STEP) + 1)
    }

    pub(crate) fn step_aims(&self) -> [f32; VOLUME_BLOCKS] {
        volume_aims(self.settings.volume)
    }

    pub(crate) fn sound_aims(&self) -> [f32; VOLUME_BLOCKS] {
        volume_aims(self.settings.sounds)
    }

    pub(super) fn view_brightness<'a>(&'a self, word: &'a mut String) -> View<'a> {
        let _ = write!(word, "{}%", self.settings.brightness);
        let value: &'a str = word;
        let gauges = &self.gauges;
        let gauge = Gauge {
            icon: &gauges.sun,
            name: BRIGHTNESS,
            value,
            blocks: &gauges.level,
            is_set: !self.does_rail_lead(),
            sway: gauges.sway.at,
        };
        let words = BarWords { subject: Some(value), speech: Some(write_speech), ..BarWords::default() };
        View::Gauges { name: BRIGHTNESS, words, gauges: [Some(gauge), None] }
    }

    pub(super) fn view_volume<'a>(&'a self, text: &'a mut String) -> View<'a> {
        let (settings, channel) = (self.settings, self.gauges.channel);
        let _ = write!(text, "{}%", settings.volume);
        let volume_end = text.len();
        let _ = write!(text, "{}%", settings.sounds);
        let sounds_end = text.len();
        let _ = match channel {
            Channel::Volume => write!(text, "{VOLUME} {}%", settings.volume),
            Channel::Sounds => write!(text, "{SOUNDS} {}%", settings.sounds),
        };
        let text: &'a str = text;
        let gauges = &self.gauges;
        let (sway, is_set) = (gauges.sway.at, |set: Channel| channel == set && !self.does_rail_lead());
        let volume = Gauge {
            icon: &gauges.speaker,
            name: VOLUME,
            value: &text[..volume_end],
            blocks: &gauges.volume,
            is_set: is_set(Channel::Volume),
            sway: if channel == Channel::Volume { sway } else { 0.0 },
        };
        let sounds = Gauge {
            icon: &gauges.chime,
            name: SOUNDS,
            value: &text[volume_end..sounds_end],
            blocks: &gauges.sounds,
            is_set: is_set(Channel::Sounds),
            sway: if channel == Channel::Sounds { sway } else { 0.0 },
        };
        let words = BarWords { subject: Some(&text[sounds_end..]), speech: Some(write_speech), ..BarWords::default() };
        View::Gauges { name: VOLUME, words, gauges: [Some(volume), Some(sounds)] }
    }
}
