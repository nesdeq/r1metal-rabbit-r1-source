use super::View;
use super::bar::BarWords;
use crate::domain::chat::tools::Destination;
use crate::domain::scene;
use crate::domain::uses::Use;
use crate::gfx::palette::{self, PALETTES, Roles};
use crate::gfx::store::Store;
use crate::gfx::theme::{THEMES, Theme};
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::list::LinePicture;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::speech::{push_lowercase, write_place, write_robot_sentence};
use crate::ui::view::lines::List;
use crate::ui::words::{PALETTE_NAMES, theme_summary};
use alloc::string::String;
use core::fmt::Write;

pub(crate) const IN_USE: &str = "IN USE";
pub(crate) const COLUMNS: usize = 3;
pub(crate) const ROWS: usize = 2;

pub(crate) struct Themes {
    pub(crate) cards_grid: Grid,
    pub(crate) cards: Store<LinePicture>,
    dealt_round: u32,
    pub(crate) cards_due: u32,
    pub(crate) palettes: Grid,
}

impl Themes {
    pub(crate) fn new(cards_grid: Grid) -> Themes {
        Themes { cards_grid, cards: Store::default(), dealt_round: 0, cards_due: 0, palettes: Grid::empty() }
    }
}

fn write_colours_speech(ui: &Ui, text: &mut String) {
    let selection = ui.themes.palettes.selection;
    let is_used = selection == ui.settings.palette();
    text.push_str(if is_used { "I wear " } else { "Try " });
    push_lowercase(text, PALETTE_NAMES[selection]);
    text.push(if is_used { '.' } else { '?' });
}

fn write_theme_speech(ui: &Ui, text: &mut String) {
    write_robot_sentence(theme_summary(Theme::at(ui.themes.cards_grid.selection)), text);
}

impl Ui {
    pub(in crate::ui) fn open_themes(&mut self) {
        self.themes.cards_grid = self.board_grid(COLUMNS, ROWS);
        self.themes.cards_grid.begin();
        self.themes.cards_grid.set_items_at(THEMES.len(), self.settings.theme as usize);
        self.deal_themes();
        self.screen = Some(Screen::Themes);
    }

    fn deal_themes(&mut self) {
        self.themes.dealt_round = self.themes.cards_grid.round;
        self.themes.cards_due = (1 << THEMES.len()) - 1;
        self.deal_card(self.themes.cards_grid.selection);
    }

    pub(crate) fn advance_themes(&mut self, now_ns: u64) {
        if self.themes.cards_due != 0 {
            self.deal_card(self.themes.cards_due.trailing_zeros() as usize);
        }
        self.themes.cards_grid.advance(now_ns);
        let Themes { cards_grid, cards, .. } = &mut self.themes;
        cards.advance(now_ns, cards_grid);
    }

    pub(super) fn pick_theme(&mut self, now_ns: u64) {
        let theme = Theme::at(self.themes.cards_grid.selection);
        if theme == self.settings.theme {
            return self.themes.cards_grid.swing(1.0);
        }
        self.settings.theme = theme;
        self.relayout();
        if let Some(places) = self.route_to(&|content| content.runs(Use::Open(Destination::Themes))) {
            self.stand_at(places, now_ns);
        }
        let grid = self.board_grid(COLUMNS, ROWS);
        let current = &self.themes.cards_grid;
        if grid.rest_area(0) == current.rest_area(0) && grid.place_size() == current.place_size() {
            self.themes.cards_grid.renew();
        } else {
            let selection = self.themes.cards_grid.selection;
            self.themes.cards_grid = grid;
            self.themes.cards_grid.set_items_at(THEMES.len(), selection);
            self.themes.cards_grid.land();
        }
        if !self.recolour() {
            self.deal_themes();
        }
        self.react(Reaction::Admire, now_ns);
    }

    pub(super) fn wheel_themes(&mut self, detents: i32) {
        self.themes.cards_grid.step(detents);
        if self.themes.cards_grid.round != self.themes.dealt_round {
            self.deal_themes();
        }
    }

    pub(super) fn view_themes<'a>(&'a self, word: &'a mut String) -> View<'a> {
        word.push_str(theme_summary(Theme::at(self.themes.cards_grid.selection)));
        let subject: &'a str = word;
        let words = BarWords { subject: Some(subject), speech: Some(write_theme_speech), ..BarWords::default() };
        View::Cards { grid: &self.themes.cards_grid, store: &self.themes.cards, name: scene::THEMES, words }
    }

    pub(in crate::ui) fn open_colours(&mut self) {
        self.show_lines(List::Colours, PALETTES.len(), Some(self.settings.palette()));
        self.screen = Some(Screen::Colours);
    }

    pub(crate) fn palette_item(&self, index: usize) -> Option<Item> {
        (index < PALETTES.len())
            .then(|| Item::palette(index, if index == self.settings.palette() { IN_USE } else { "" }))
    }

    pub(super) fn pick_colours(&mut self, now_ns: u64) {
        let selection = self.themes.palettes.selection;
        if selection == self.settings.palette() {
            self.recolour();
            return self.themes.palettes.swing(1.0);
        }
        self.take_palette(selection, now_ns);
    }

    pub(super) fn take_palette(&mut self, palette: usize, now_ns: u64) {
        self.settings = self.settings.with_palette(palette);
        self.recolour();
        self.react(Reaction::Admire, now_ns);
    }

    pub(crate) fn recolour(&mut self) -> bool {
        self.wear(self.settings.palette())
    }

    fn wear(&mut self, palette: usize) -> bool {
        let roles = Roles::at(palette);
        if core::mem::replace(&mut self.roles, roles) == roles {
            return false;
        }
        self.renew_rooms();
        match self.screen {
            Some(Screen::Colours) => {
                self.deal_lines(List::Colours, |_| true);
            }
            Some(Screen::Themes) => self.deal_themes(),
            Some(_) => self.deal_settings(),
            None => {}
        }
        self.deal_menu();
        true
    }

    pub(super) fn wheel_colours(&mut self, detents: i32) {
        self.wheel_lines(List::Colours, detents);
        self.wear(self.themes.palettes.selection);
    }

    pub(super) fn view_colours<'a>(&'a self, text: &'a mut String) -> View<'a> {
        let (selection, current) = (self.themes.palettes.selection, self.settings.palette());
        write_place(text, selection + 1, PALETTES.len());
        if selection == current {
            let _ = write!(text, ", {IN_USE}");
        }
        let place_end = text.len();
        let _ = write!(text, "{IN_USE} {}", palette::letter(current));
        let text: &'a str = text;
        let (place, state) = text.split_at(place_end);
        let words = BarWords {
            subject: Some(place),
            state: Some(state),
            speech: Some(write_colours_speech),
            ..BarWords::default()
        };
        View::Palettes { name: scene::COLORS, words }
    }
}
