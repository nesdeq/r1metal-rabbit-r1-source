use super::View;
use super::action::Action;
use super::bar::BarWords;
use super::tree::POWER_ICON;
use crate::domain::battery::{self, Facts};
use crate::domain::chat::tools::Destination;
use crate::domain::scene::POWER;
use crate::domain::uses::Use;
use crate::hw::power::Power;
use crate::hw::power::battery::{Cell, Reading, Supply};
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::robot::{Mood, News};
use crate::ui::screen::Screen;
use crate::ui::view::lines::{LineTag, List};
use alloc::string::String;

pub(super) const WAYS_DOWN: &str = "WAYS DOWN";
const STANDBY_ICON: &str = icon!("bedtime");
const REBOOT_ICON: &str = icon!("restart_alt");
const POWER_OFF_ICON: &str = icon!("power_settings_new");
const BOOTLOADER_ICON: &str = icon!("memory");
pub(super) const ICONS: [&str; 4] = [STANDBY_ICON, REBOOT_ICON, POWER_OFF_ICON, BOOTLOADER_ICON];

const fn power_words(power: Power) -> (&'static str, &'static str) {
    match power {
        Power::Reboot => ("REBOOT", "RESTARTS THE DEVICE"),
        Power::Bootloader => ("FASTBOOT", "RESTARTS INTO THE BOOTLOADER"),
        Power::PowerOff => ("POWER OFF", "SWITCHES THE DEVICE OFF"),
    }
}

pub(super) fn power_item(power: Power) -> Item {
    let (name, word) = power_words(power);
    let icon = match power {
        Power::Reboot => REBOOT_ICON,
        Power::Bootloader => BOOTLOADER_ICON,
        Power::PowerOff => POWER_OFF_ICON,
    };
    Item::action(name, word, icon)
}

pub(crate) struct Battery {
    pub(crate) grid: Grid,
    cell: Option<Cell>,
}

impl Battery {
    pub(crate) fn new() -> Battery {
        Battery { grid: Grid::empty(), cell: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Cell(battery::Line),
    Standby,
    Down(Power),
}

const WAYS: [Line; 3] = [Line::Standby, Line::Down(Power::Reboot), Line::Down(Power::PowerOff)];

const fn tag_of(line: Line) -> LineTag {
    match line {
        Line::Cell(battery::Line::Battery) => LineTag::Primary,
        Line::Cell(battery::Line::TimeLeft) => LineTag::Folded,
        _ => LineTag::Plain,
    }
}

fn write_speech(ui: &Ui, text: &mut String) {
    text.push_str(match ui.charge.reading {
        None if ui.battery_facts().is_uncounted() => "I know my level after a full charge.",
        None => "I'm measuring my battery.",
        Some((_, Supply::Battery)) if ui.charge.is_low => "I'm low. A cable soon?",
        Some((_, Supply::Battery)) => "I'm on my battery.",
        Some((_, Supply::Charging)) => "I'm charging.",
        Some((_, Supply::Cable)) => "I'm on the cable, held.",
        Some((_, Supply::Charged)) => "I'm charged.",
    });
}

impl Ui {
    fn does_power_show(&self) -> bool {
        self.screen == Some(Screen::Power)
    }

    pub(super) const fn battery_facts(&self) -> Facts {
        Facts { reading: self.charge.reading, is_low: self.charge.is_low, cell: self.power.cell }
    }

    fn power_lines(&self) -> impl Iterator<Item = Line> + '_ {
        battery::LINES.map(Line::Cell).into_iter().chain(WAYS).filter(|line| self.is_tag_listed(tag_of(*line)))
    }

    pub(super) fn open_power(&mut self) {
        self.screen = Some(Screen::Power);
        let count = self.power_lines().count();
        self.power.grid.begin();
        self.power.grid.set_items(count);
        self.stand_dial(0);
        self.deal_lines(List::Power, |_| true);
    }

    pub(crate) fn power_item_at(&self, index: usize) -> Option<Item> {
        let line = self.power_lines().nth(index)?;
        let (name, word) = self.power_words(line);
        Some(match line {
            Line::Cell(battery::Line::Battery) => {
                let fold = self
                    .power
                    .cell
                    .map_or_else(String::new, |cell| self.battery_facts().to_time_left_word(&cell, true));
                self.folding_item(name, word, self.charge.reading.map(|reading| reading.0), fold, POWER_ICON)
            }
            Line::Standby => Item::action(name, word, STANDBY_ICON).with_caption(WAYS_DOWN),
            Line::Down(power) => power_item(power),
            Line::Cell(_) => Item::figure(name, word),
        })
    }

    fn power_words(&self, line: Line) -> (&'static str, String) {
        match line {
            Line::Cell(line) => self.battery_facts().to_words(line),
            Line::Standby => ("STANDBY", "PUTS THE DEVICE TO SLEEP".into()),
            Line::Down(power) => (power_words(power).0, power_words(power).1.into()),
        }
    }

    pub(super) fn pick_power(&mut self, now_ns: u64) -> Option<Action> {
        let selection = self.power_lines().nth(self.power.grid.selection);
        match selection {
            Some(Line::Standby) => Some(self.fall_asleep(now_ns)),
            Some(Line::Down(power)) => Some(Action::Power(power)),
            _ => {
                self.power.grid.swing(1.0);
                None
            }
        }
    }

    pub(super) fn wheel_power(&mut self, detents: i32) {
        if self.turn_lines(List::Power, detents) {
            self.deal_lines(List::Power, |_| true);
        }
    }

    /// Takes the battery thread's cell; true if the screen changed.
    #[must_use]
    pub fn set_cell(&mut self, cell: Cell) -> bool {
        self.power.cell = Some(cell);
        self.redeal_power()
    }

    // A cable put in or pulled is use of the device, and lights a screen dark under a task as it comes.
    fn note_replug(&mut self, is_replugged: bool, now_ns: u64) {
        if is_replugged {
            (self.used_at, self.input_at) = (Some(now_ns), Some(now_ns));
            self.is_dark &= !self.charge.is_plugged;
        }
    }

    /// Sets what the battery icon shows; true if the picture changed.
    #[must_use]
    pub fn set_battery(&mut self, level: Option<u8>, supply: Supply, now_ns: u64) -> bool {
        let change = self.charge.take(level, supply);
        let is_mark_new = self.corner.show(level, supply) | change.is_tint_new;
        let is_plugged = self.charge.is_plugged;
        if change.was_known && change.is_replugged {
            self.robot.news = Some(if is_plugged { News::Plugged } else { News::Unplugged });
        }
        if change.has_filled {
            self.robot.news = Some(News::Full);
        }
        self.note_replug(change.is_replugged, now_ns);
        self.robot.mood = if self.charge.is_low { Mood::Worried } else { Mood::Usual };
        let is_new = change.is_level_new || is_mark_new;
        let is_dealt = self.redeal_power() || self.does_power_show() && is_new;
        let is_listed = self.do_menus_show() && is_new;
        if is_listed {
            self.deal_menu();
        }
        let is_status = self.does_status_show() && (change.is_level_new || change.is_replugged);
        let is_staged =
            change.is_level_new && self.does_line_show(|content| content.runs(Use::Open(Destination::Power)));
        let is_marked = self.screen.is_none() || self.does_corner_show();
        is_mark_new && is_marked || is_dealt || is_listed || is_status || is_staged
    }

    /// Takes the battery thread's reading, its cable always and its level and cell once it is shown; returns if the picture changed, the doze's line, and if a cable came.
    #[must_use]
    pub fn set_reading(&mut self, reading: &Reading, now_ns: u64) -> (bool, Option<String>, bool) {
        let was_unplugged = !self.charge.is_plugged;
        let is_changed = if reading.is_shown {
            let has_battery_changed = self.set_battery(reading.level, reading.supply(), now_ns);
            let has_cell_changed = self.set_cell(reading.cell);
            has_battery_changed || has_cell_changed
        } else {
            let is_replugged = self.charge.plug(reading.supply().is_plugged());
            self.note_replug(is_replugged, now_ns);
            false
        };
        let doze_line = self.doze.as_mut().and_then(|doze| doze.take_reading(reading));
        let is_plugged_in = !self.is_awake() && was_unplugged && self.charge.is_plugged;
        (is_changed, doze_line, is_plugged_in)
    }

    fn redeal_power(&mut self) -> bool {
        self.does_power_show() && self.deal_lines(List::Power, |_| true)
    }

    pub(super) fn view_power<'a>(&'a self, word: &'a mut String) -> View<'a> {
        self.battery_facts().write_charge(word);
        let figure: &'a str = word;
        let words = BarWords {
            figure: Some(figure),
            place: self.place_in(List::Power),
            speech: Some(write_speech),
            ..BarWords::default()
        };
        View::Lines { list: List::Power, name: POWER, words }
    }
}
