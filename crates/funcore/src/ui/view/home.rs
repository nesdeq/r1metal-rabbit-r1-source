use super::lines::List;
use super::rail::is_rail_level;
use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::pen::Pen;
use crate::ui::Ui;
use crate::ui::field::{draw_field, draw_sentence};
use crate::ui::grid::Grid;
use crate::ui::kit::draw_rule;
use crate::ui::layout::{HomeSeat, HomeWords};
use crate::ui::list::stage::{Stage, draw_arrows, draw_name};
use crate::ui::list::{BOARD_COLUMNS, BOARD_ROWS, ListForm, Page};
use crate::ui::robot::Seat;
use crate::ui::ruler::HOME_SCALE;
use crate::ui::screen::bar::BarWords;
use crate::ui::screen::tree::{Content, HOME, Level, Node, SUGGESTED, Shape, TREE, Tree, icon_of, visible_of};
use crate::ui::words::HOLD_TO_TALK;
use alloc::boxed::Box;
use alloc::vec::Vec;

impl Ui {
    pub(in crate::ui) fn is_at_home(&self) -> bool {
        let level = &self.tree.levels[0];
        self.screen.is_none() && self.tree.path.len() == 1 && level.stage().is_none_or(|main| main.selection == HOME)
    }

    pub(in crate::ui) fn do_stages_show(&self) -> bool {
        self.screen.is_none() && self.tree.level().stage().is_some()
    }

    pub(in crate::ui) fn do_menus_show(&self) -> bool {
        self.screen.is_none() && self.tree.level().menu().is_some()
    }

    pub(in crate::ui) fn is_menu_home(&self) -> bool {
        self.do_menus_show() && self.tree.path.len() == 1
    }

    pub(in crate::ui) fn is_menu_under_home(&self) -> bool {
        self.tree.path.len() > 1 && self.tree.level().menu().is_some()
    }

    pub(in crate::ui) fn does_time_show(&self) -> bool {
        self.does_corner_show() || self.does_status_show()
    }

    pub(in crate::ui) fn menu_list(&self) -> Option<List> {
        self.tree.level().menu().map(|_| List::Menu(self.tree.front()))
    }

    // Deals the tree's lines that show: the menu with no screen over it, the pane's and the rail's.
    pub(in crate::ui) fn deal_menu(&mut self) {
        let menu = self.menu_list().filter(|_| self.screen.is_none());
        let beside = [self.pane_list(), self.rail_list()].into_iter().flatten().filter(move |list| Some(*list) != menu);
        for list in menu.into_iter().chain(beside) {
            self.deal_lines(list, |_| true);
        }
    }

    pub(in crate::ui) fn does_line_show(&self, is_line: impl Fn(Content) -> bool) -> bool {
        let level = self.tree.level();
        let is_shown = |item: usize| level.node(item).is_some_and(|node| is_line(node.content));
        self.screen.is_none()
            && match &level.shape {
                Shape::Stage(stage) => stage.feet().any(|foot| is_shown(foot.0)),
                Shape::Menu(grid) | Shape::Rail(grid) => grid.visible().any(is_shown),
            }
    }

    pub(in crate::ui) fn home_nodes(&self) -> &'static [Node] {
        if self.look().home.is_suggested { &SUGGESTED } else { &TREE }
    }

    pub(in crate::ui) fn does_home_speak(&self) -> bool {
        self.look().home.words == HomeWords::Sentence
    }

    pub(in crate::ui) fn does_greeting_show(&self) -> bool {
        self.is_menu_home() && self.does_home_speak()
    }

    pub(in crate::ui) fn deal_hero(&mut self) -> bool {
        let does_show = self.is_hero(List::Menu(0), 0) && self.is_menu_home();
        if does_show {
            self.deal_menu();
        }
        self.does_chat_show() || does_show
    }

    pub(in crate::ui) fn grow_tree(&self) -> Tree {
        let mut tree = Tree { levels: Vec::new(), path: Vec::from([0]) };
        self.grow(&mut tree.levels, self.home_nodes(), 0);
        tree
    }

    fn grow(&self, levels: &mut Vec<Level>, nodes: &'static [Node], depth: usize) -> usize {
        let visible = visible_of(nodes, self.gate());
        let shape = if self.look().home.has_stages {
            let icons = visible.iter().map(|at| icon_of(&self.glyphs, &nodes[*at])).collect();
            Shape::Stage(Stage::new(self.ruler.width, self.ruler.height, self.ruler.stage_side(), icons))
        } else {
            let is_rail = self.look().has_rail && is_rail_level(nodes);
            let mut grid = Box::new(self.menu_grid(visible.len(), depth == 0 || is_rail));
            grid.begin();
            if is_rail { Shape::Rail(grid) } else { Shape::Menu(grid) }
        };
        let level = levels.len();
        levels.push(Level { nodes, visible, children: Vec::new(), shape });
        if depth == 0 || levels[level].menu().is_some() {
            levels[level].stand(0, false);
        }
        for node in nodes {
            let child = match node.content {
                Content::Level(nodes) => Some(self.grow(levels, nodes, depth + 1)),
                Content::Action(_) | Content::Setting(_) | Content::Note(_) => None,
            };
            levels[level].children.push(child);
        }
        level
    }

    // A level that stands at home or on the rail takes the layout's home grid.
    fn menu_grid(&self, count: usize, is_home: bool) -> Grid {
        let style = self.look().list;
        if is_home {
            return self.layout().home_grid(&self.ids, &self.ruler, count);
        }
        let (room, height, gap) = (self.ruler.under_bar(), self.ruler.height, self.ruler.gap);
        match style.form {
            _ if style.page == Page::Board => {
                Grid::new(&self.ids, room, height, gap, BOARD_COLUMNS, BOARD_ROWS).with_fill()
            }
            ListForm::Dial => Grid::new(&self.ids, room, height, gap, 1, count.max(1)),
            ListForm::Rows | ListForm::Typed => self.list_grid(),
        }
    }

    // A level of the rail lays its lines out where it stands: as the rail on the way in, else as lines in the pane.
    pub(in crate::ui) fn seat_level(&mut self, level: usize, is_rail: bool) {
        let count = self.tree.levels[level].visible.len();
        let grid = self.menu_grid(count, is_rail);
        if let Shape::Rail(seated) = &mut self.tree.levels[level].shape {
            let selection = seated.selection;
            **seated = grid;
            seated.set_items_at(count, selection);
        }
    }

    fn draw_hold_word(&self, pen: &mut Pen<'_, '_>, x: i32, y: i32) {
        let (kit, body) = (self.kit(), &self.ruler.body);
        draw_field(&kit, pen, body, (x, y), HOLD_TO_TALK, body.text_width(HOLD_TO_TALK), kit.roles.text_dim);
    }

    fn draw_prompt(&self, pen: &mut Pen<'_, '_>) {
        let ruler = &self.ruler;
        if !self.is_awaited {
            let x = (ruler.width - ruler.body.text_width(HOLD_TO_TALK)) / 2;
            self.draw_hold_word(pen, x, ruler.square_foot());
        }
        if let Some(first) = self.tree.levels[0].menu().map(|home| home.rest_area(home.page())) {
            draw_rule(pen, &self.kit(), first.y - ruler.gap_phi2());
        }
    }

    fn draw_names(&self, pen: &mut Pen<'_, '_>, stage: &Stage) {
        let (kit, ruler) = (self.kit(), &self.ruler);
        let (level, front, mut word) = (self.tree.level(), self.tree.front(), self.bar_text.borrow_mut());
        for (icon, bottom, middle) in stage.feet() {
            let Some(node) = level.node(icon) else {
                continue;
            };
            let (foot, centre) = (round(bottom), round(middle));
            if front == 0 && icon == HOME {
                if !self.is_awaited {
                    let left = centre - ruler.body.text_width(HOLD_TO_TALK) / 2;
                    self.draw_hold_word(pen, left, foot + ruler.robot_margin() + ruler.gap_phi());
                }
                continue;
            }
            word.clear();
            self.write_node_word(node, &mut word);
            let cue = self.staged_word().filter(|_| icon == level.selection());
            draw_name(pen, &kit, (centre, foot), node.name, (&word, cue));
        }
    }

    fn draw_stages(&self, pen: &mut Pen<'_, '_>) {
        let (kit, tree) = (self.kit(), &self.tree);
        for (index, level) in tree.levels.iter().enumerate() {
            if let Some(stage) = level.stage() {
                stage.draw(pen, &kit.roles, (index == 0).then_some(HOME));
            }
        }
        let docked = &tree.path[..tree.path.len().saturating_sub(2)];
        draw_arrows(pen, &kit, docked.iter().filter_map(|index| tree.levels[*index].stage()));
        if let Some(stage) = tree.level().stage() {
            self.draw_names(pen, stage);
            stage.draw_dots(pen, &kit);
        }
    }

    pub(in crate::ui) fn draw_home(&self, pen: &mut Pen<'_, '_>, sentence: &str) {
        let (look, ruler) = (self.look(), &self.ruler);
        pen.fill(pen.bounds(), self.roles.background);
        match look.home.words {
            HomeWords::Nothing => {}
            HomeWords::Prompt => self.draw_prompt(pen),
            HomeWords::Sentence => {
                let at = (self.sentence_left(), ruler.square_foot());
                draw_sentence(&self.kit(), pen, sentence, at, self.sentence_room());
            }
        }
        if look.home.has_stages {
            self.draw_stages(pen);
        } else if look.list.form == ListForm::Dial {
            let (top, foot) = (ruler.square_foot(), ruler.foot_top() - ruler.gap);
            self.draw_dial(pen, List::Menu(0), Rect::new(0, top, ruler.width, foot - top));
        } else {
            self.draw_rows(pen, List::Menu(0));
        }
        if look.has_status {
            self.draw_status(pen, &BarWords::default());
        } else {
            self.draw_corner(pen);
        }
    }

    pub(in crate::ui) fn has_home_seat(&self) -> bool {
        self.look().home.seat != HomeSeat::Bar
    }

    pub(in crate::ui) fn home_seat(&self) -> Seat {
        let ruler = &self.ruler;
        match self.look().home.seat {
            HomeSeat::Square => self.seat_in_square(ruler.centre_left(ruler.home_side()), ruler.gap),
            HomeSeat::Bar => self.bar_seat(),
            HomeSeat::Hero => {
                let hero = self.tree.levels[0].menu().map_or(Rect::new(0, 0, 0, 0), |home| {
                    let rest = home.rest_area(0);
                    home.drawn_area(0).unwrap_or(Rect::new(rest.x, rest.y - ruler.height, rest.w, rest.h))
                });
                self.seat_in_square(hero.x + ruler.card_inset(), hero.y + ruler.card_inset())
            }
            HomeSeat::Flight => {
                let (shape_width, shape_height) = ruler.robot;
                let (size, middle) = (ruler.stage_side(), ruler.height as f32 / 2.0);
                let rest = self.tree.levels[0].stage().and_then(Stage::rest_edges);
                let (top, bottom) = rest.unwrap_or((middle - size / 2.0, middle + size / 2.0));
                let width = size * HOME_SCALE * shape_width;
                let height = size * HOME_SCALE * shape_height * (bottom - top) / size.max(1.0);
                let (x, y) = ((ruler.width as f32 - width) / 2.0, (top + bottom - height) / 2.0);
                Seat { x, y, width, height, quarters: 0, turn: 0.0 }
            }
        }
    }
}
