use super::View;
use super::bar::BarWords;
use crate::domain::device::Fact;
use crate::domain::scene::ABOUT;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::view::lines::List;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const UNLOCK_PRESSES: u32 = 5;

pub(crate) struct About {
    pub(crate) grid: Grid,
    facts: Vec<Fact>,
    is_waiting: bool,
    presses: u32,
}

impl About {
    pub(crate) fn new() -> About {
        About { grid: Grid::empty(), facts: Vec::new(), is_waiting: false, presses: 0 }
    }
}

fn write_speech(_: &Ui, text: &mut String) {
    let _ = write!(text, "I run r1metal {}.", crate::VERSION);
}

impl Ui {
    /// True while the about screen waits for its facts.
    pub fn is_about_wanted(&self) -> bool {
        self.is_awake() && self.does_about_wait()
    }

    fn does_about_wait(&self) -> bool {
        self.screen == Some(Screen::About) && self.about.is_waiting
    }

    /// Takes the device's facts; true if the screen changed.
    #[must_use]
    pub fn set_about(&mut self, facts: Vec<Fact>) -> bool {
        if !self.does_about_wait() {
            return false;
        }
        (self.about.is_waiting, self.about.facts) = (false, facts);
        self.show_lines(List::About, self.about.facts.len(), None);
        true
    }

    pub(super) fn open_about(&mut self) {
        self.about.facts.clear();
        self.about.grid.begin();
        (self.about.is_waiting, self.about.presses) = (true, 0);
        self.screen = Some(Screen::About);
    }

    pub(crate) fn about_item(&self, index: usize) -> Option<Item> {
        let fact = self.about.facts.get(index)?;
        Some(Item::figure(fact.label(), fact.to_value()))
    }

    pub(super) fn pick_about(&mut self, now_ns: u64) {
        let is_firmware = matches!(self.about.facts.get(self.about.grid.selection), Some(Fact::Firmware));
        self.about.presses = if is_firmware { self.about.presses.saturating_add(1) } else { 0 };
        if self.about.presses == UNLOCK_PRESSES && !self.is_unlocked {
            self.unlock();
            self.react(Reaction::Wink, now_ns);
            return;
        }
        self.about.grid.swing(1.0);
    }

    pub(super) fn view_about(&self) -> View<'_> {
        let words = BarWords { place: self.place_in(List::About), speech: Some(write_speech), ..BarWords::default() };
        View::Lines { list: List::About, name: ABOUT, words }
    }
}
