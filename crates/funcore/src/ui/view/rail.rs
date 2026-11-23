use super::lines::List;
use crate::domain::chat::tools::Destination;
use crate::domain::uses::Use;
use crate::gfx::Rect;
use crate::gfx::pen::Pen;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::kit::draw_upright_rule;
use crate::ui::screen::Screen;
use crate::ui::screen::action::Action;
use crate::ui::screen::tree::{Content, Node};
use alloc::vec::Vec;

#[derive(Default, Clone, Copy)]
pub(in crate::ui) struct Rail {
    // The level and line the pane follows, once the rail came to rest on it.
    line: Option<(usize, usize)>,
    // The screen the rail opened of that line, while the wheel turns the rail beside it.
    screen: Option<Screen>,
}

// Where the rail stood: the place of each level on the way in, and what its pane followed.
pub(in crate::ui) struct Stand {
    places: Vec<usize>,
    rail: Rail,
}

// A level stands on the rail where each of its lines opens something, none setting a value or only telling.
pub(super) fn is_rail_level(nodes: &[Node]) -> bool {
    !nodes.iter().any(|node| matches!(node.content, Content::Setting(_) | Content::Note(_)))
}

// What the pane opens of a line: a screen, but for those that open on a click alone: the viewfinders, which take the panel and power the camera, the passphrase's window, the PIN's keys, the tunnel's window and the debug screen.
const fn is_followed(action: Use) -> bool {
    match action {
        Use::Open(
            Destination::PhotoCamera
            | Destination::VideoCamera
            | Destination::Passphrase
            | Destination::SimPin
            | Destination::Tunnel
            | Destination::Debug,
        ) => false,
        Use::Chat | Use::Open(_) => true,
        Use::Robot | Use::Forget => false,
    }
}

impl Ui {
    // The deepest level on the way in that stands on the rail.
    fn rail_level(&self) -> usize {
        let levels = &self.tree.levels;
        self.tree.path.iter().rev().copied().find(|level| levels[*level].is_rail()).unwrap_or(0)
    }

    pub(super) fn does_rail_show(&self) -> bool {
        let is_whole = matches!(
            self.screen,
            Some(Screen::Finder(_) | Screen::Roll(_) | Screen::Key | Screen::Safe | Screen::Ring | Screen::Incoming)
        );
        let is_opening = matches!(self.screen, Some(Screen::Grid(_))) && self.rolls.grid.opening() > 0.0;
        self.look().has_rail && !is_whole && !is_opening && self.keys_up().is_none()
    }

    fn rail_line(&self) -> Option<(usize, usize)> {
        let level = self.tree.level();
        level.is_rail().then(|| (self.tree.front(), level.selection()))
    }

    fn rail_use(&self) -> Option<Use> {
        match self.selected_content()? {
            Content::Action(action) if is_followed(action) => Some(action),
            _ => None,
        }
    }

    // True while the wheel turns the rail beside the screen it opened of the line it rests on.
    pub(in crate::ui) fn does_rail_lead(&self) -> bool {
        self.rail.screen.is_some_and(|screen| self.screen == Some(screen)) && self.rail.line == self.rail_line()
    }

    // The screen the button and the wheel reach: none while the rail turns beside the screen it opened.
    pub(in crate::ui) fn entered_screen(&self) -> Option<Screen> {
        self.screen.filter(|_| !self.does_rail_lead())
    }

    pub(in crate::ui) fn does_rail_turn(&self) -> bool {
        self.rail_line().is_some() && self.entered_screen().is_none()
    }

    fn has_rail_wheel(&self) -> bool {
        self.rail_level() == self.tree.front() && self.entered_screen().is_none()
    }

    // Whether the wheel is on the rail, where the level stands on it.
    pub(super) fn rail_wheel(&self, level: usize) -> Option<bool> {
        (self.tree.levels[level].is_rail() && level == self.rail_level()).then(|| self.has_rail_wheel())
    }

    // The level whose lines the pane shows of the line the rail rests on.
    pub(in crate::ui) fn pane_level(&self) -> Option<usize> {
        let (rail, line) = self.rail.line?;
        let level = self.tree.levels.get(rail).filter(|level| level.selection() == line)?;
        let child = level.children[*level.visible.get(line)?]?;
        self.tree.path.contains(&rail).then_some(child)
    }

    pub(in crate::ui) fn pane_list(&self) -> Option<List> {
        self.pane_level().filter(|level| *level != self.tree.front()).map(List::Menu)
    }

    // The rail's list, which moves while it shows, whatever the pane shows.
    pub(in crate::ui) fn rail_list(&self) -> Option<List> {
        self.does_rail_show().then(|| List::Menu(self.rail_level()))
    }

    // Leaves the screen the rail opened, as a swipe leaves it.
    fn close_rail_screen(&mut self) {
        self.leave_screen();
        self.close_screen();
    }

    pub(in crate::ui) fn turn_rail(&mut self, detents: i32) {
        if self.does_rail_lead() {
            self.close_rail_screen();
        }
        self.rail = Rail::default();
        self.wheel_lines(List::Menu(self.tree.front()), detents);
    }

    // The pane follows the line the rail came to rest on.
    pub(in crate::ui) fn follow_rail(&mut self, now_ns: u64) -> Option<Action> {
        let line = self.rail_line()?;
        let is_resting = self.tree.level().menu().is_some_and(Grid::is_resting);
        if !is_resting || self.rail.line == Some(line) || self.entered_screen().is_some() {
            return None;
        }
        self.follow_line(line, now_ns)
    }

    // Back on the rail from a level it held, that level's lines stand in the pane at once.
    pub(in crate::ui) fn follow_rail_back(&mut self, now_ns: u64) {
        if let Some(line) = self.rail_line() {
            self.follow_line(line, now_ns);
        }
    }

    // Opens what the rail's line holds, as a click would, but for the wheel, which stays on the rail.
    fn follow_line(&mut self, line: (usize, usize), now_ns: u64) -> Option<Action> {
        if self.does_rail_lead() {
            self.close_rail_screen();
        }
        self.rail = Rail { line: Some(line), screen: None };
        if let Some(level) = self.pane_level() {
            self.seat_level(level, false);
            self.tree.levels[level].stand(0, false);
            self.deal_lines(List::Menu(level), |_| true);
            return None;
        }
        let action = self.perform(self.rail_use()?, now_ns);
        self.rail.screen = self.screen;
        self.land_screen();
        action
    }

    // The line the pane follows stays on its node as the lines of its level that show change; where that node goes, the pane follows the line the rail rests on next.
    pub(in crate::ui) fn keep_rail_line(&mut self, level: usize, to_new: &dyn Fn(usize) -> Option<usize>) {
        if let Some((rail, line)) = self.rail.line.filter(|(rail, _)| *rail == level) {
            self.rail.line = to_new(line).map(|line| (rail, line));
        }
    }

    // The screen the rail opened takes the wheel, as a click on the rail gives it, and counts as a use.
    pub(in crate::ui) fn enter_rail_screen(&mut self) {
        if !self.does_rail_lead() {
            return;
        }
        self.rail.screen = None;
        if let Some(action) = self.rail_use() {
            self.uses.push(action);
        }
        self.land_screen();
    }

    // Back beside the rail: up a level from the rail, at home nothing, and the wheel to the rail from the screen it opened; true where taken.
    pub(in crate::ui) fn rail_back(&mut self, now_ns: u64) -> bool {
        if self.does_rail_lead() {
            if self.tree.path.len() > 1 {
                self.close_rail_screen();
                self.rail = Rail::default();
                self.level_back(now_ns);
            }
            return true;
        }
        let is_root = self.screen.is_some_and(|screen| screen.parent().is_none());
        let line = self.rail_line().filter(|_| is_root && self.does_rail_show() && self.rail_use().is_some());
        if line.is_none() {
            return false;
        }
        self.rail = Rail { line, screen: self.screen };
        true
    }

    // As the conversation opens over what shows, the rail stands on its line, the wheel in the conversation; returns where the rail stood.
    pub(in crate::ui) fn stand_rail_on_chat(&mut self, now_ns: u64) -> Option<Stand> {
        let places = self.route_to(&|content| content.runs(Use::Chat)).filter(|_| self.look().has_rail)?;
        let stand = Stand { places: self.path_places(), rail: self.rail };
        self.stand_at(places, now_ns);
        self.rail = Rail { line: self.rail_line(), screen: None };
        Some(stand)
    }

    // Back from that conversation the rail stands where it stood.
    pub(in crate::ui) fn return_rail(&mut self, stand: Stand, now_ns: u64) {
        self.rail = stand.rail;
        self.stand_at(stand.places, now_ns);
    }

    pub(in crate::ui) fn draw_rail(&self, pen: &mut Pen<'_, '_>) {
        if !self.does_rail_show() {
            return;
        }
        let ruler = &self.ruler;
        pen.fill(Rect::new(0, 0, ruler.left, ruler.height), self.roles.background);
        draw_upright_rule(pen, &self.kit(), ruler.left);
        self.draw_rows(pen, List::Menu(self.rail_level()));
        self.draw_corner(pen);
    }
}
