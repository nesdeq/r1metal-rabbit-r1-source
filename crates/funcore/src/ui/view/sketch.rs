use crate::gfx::Picture;
use crate::gfx::palette::Roles;
use crate::gfx::theme::{THEMES, Theme};
use crate::ui::Ui;
use crate::ui::kit::paint_picture;
use crate::ui::list::LinePicture;
use crate::ui::picture::sketch::Sketch;
use crate::ui::screen::themes::IN_USE;
use crate::ui::words::theme_name;
use alloc::vec::Vec;

impl Ui {
    pub(in crate::ui) fn deal_card(&mut self, place: usize) {
        let mask = 1 << place;
        if self.themes.cards_due & mask == 0 {
            return;
        }
        self.themes.cards_due &= !mask;
        let spare = self.themes.cards_grid.spot_of(place).and_then(|spot| self.themes.cards.spare(spot));
        let words = spare.map(|card| card.picture.words).unwrap_or_default();
        let size = self.themes.cards_grid.place_size();
        let picture = self.paint_theme_card(Theme::at(place), size, words);
        let grid = &self.themes.cards_grid;
        if let Some(spot) = grid.spot_of(place) {
            let card = LinePicture { picture, ..LinePicture::default() };
            self.themes.cards.put(&self.ids, grid, (spot, None), card, false);
        }
    }

    fn paint_theme_card(&mut self, theme: Theme, size: (usize, usize), words: Vec<u32>) -> Picture {
        let home = self.paint_home_of(theme);
        let sketch = Sketch {
            screen: &home,
            frame: Roles::at(self.settings.colours[theme as usize]).rule,
            name: theme_name(theme),
            names: &THEMES.map(theme_name),
            note: (theme == self.settings.theme).then_some(IN_USE),
        };
        let card = paint_picture(words, self.format, size, |plate| sketch.paint(plate, &self.kit()));
        self.offscreen.spare = home;
        card
    }

    fn paint_home_of(&mut self, theme: Theme) -> Vec<u32> {
        let previous_theme = core::mem::replace(&mut self.settings.theme, theme);
        self.set_ruler();
        let roles = core::mem::replace(&mut self.roles, Roles::at(self.settings.colours[theme as usize]));
        let (screen, dial) = (self.screen.take(), core::mem::take(&mut self.dial));
        let caret = core::mem::replace(&mut self.caret.is_lit, true);
        let draw_ns = core::mem::replace(&mut self.ticking.draw_ns, self.ticking.start_ns);
        let (lists, rail) = (core::mem::take(&mut self.lists), core::mem::take(&mut self.rail));
        let tree = self.grow_tree();
        let tree = core::mem::replace(&mut self.tree, tree);
        self.show_robot_place();
        self.deal_menu();
        if let Some(home) = self.tree.levels[0].menu_mut() {
            home.land();
        }
        self.offscreen.spare.clear();
        let words = self.draw_offscreen(|ui, pen| {
            ui.draw_screen(pen, ui.frame_ns);
            ui.draw_rail(pen);
            if ui.does_robot_live() {
                ui.robot.draw(pen, &ui.roles, &ui.robot_seat());
            }
        });
        (self.tree, self.dial, self.lists, self.rail) = (tree, dial, lists, rail);
        (self.settings.theme, self.roles, self.screen, self.caret.is_lit, self.ticking.draw_ns) =
            (previous_theme, roles, screen, caret, draw_ns);
        self.set_ruler();
        words
    }
}
