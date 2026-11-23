use super::bar::BarWords;
use super::tree::{COST_ICON, Content, Node, setting};
use super::{FORWARD, View};
use crate::domain::chat::tools::Destination;
use crate::domain::cost::{Figure, Page};
use crate::domain::scene::{COST, LIMIT};
use crate::domain::settings::Setting;
use crate::domain::uses::Use;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::screen::Screen;
use crate::ui::speech::write_robot_words;
use crate::ui::view::lines::List;
use alloc::string::String;
use alloc::vec::Vec;

const COUNTING: &str = "COUNTING";
const SPEECH_START: &str = "I cost ";
const LIMIT_ICON: &str = icon!("savings");
pub(super) const ICONS: [&str; 1] = [LIMIT_ICON];
// The setting lines of the list, between the figures and the key's line.
pub(super) const SETTINGS: [Node; 1] = [setting(LIMIT, LIMIT_ICON, Setting::Limit)];

pub(crate) struct Cost {
    pub(crate) grid: Grid,
    figures: Vec<Figure>,
    gathering: u32,
    pub(super) is_counting: bool,
    pub(super) total: Option<String>,
    is_total_known: bool,
    pub(crate) spending: Grid,
    pub(crate) share: Grid,
}

impl Cost {
    pub(crate) fn new() -> Cost {
        Cost {
            grid: Grid::empty(),
            figures: Vec::new(),
            gathering: 0,
            is_counting: false,
            total: None,
            is_total_known: false,
            spending: Grid::empty(),
            share: Grid::empty(),
        }
    }
}

fn figure_item(figure: &Figure) -> Item {
    Item { has_lead: figure.page.is_some(), ..Item::figure(figure.name.clone(), figure.value.clone()) }
}

fn page_item(page: Option<&Page>, index: usize) -> Option<Item> {
    page?.lines.get(index).map(figure_item)
}

fn write_speech(ui: &Ui, text: &mut String) {
    if let Some(total) = ui.cost.figures.first() {
        text.push_str(SPEECH_START);
        write_robot_words(total.month_part(), text, false);
    }
}

impl Ui {
    pub(super) fn open_cost(&mut self) {
        self.cost.figures.clear();
        self.cost.grid.begin();
        self.stand_dial(0);
        (self.cost.gathering, self.cost.is_counting) = (self.cost.gathering.wrapping_add(1), true);
        self.screen = Some(Screen::Cost);
    }

    pub(crate) fn cost_item(&self, index: usize) -> Option<Item> {
        if index == self.set_key_at() {
            return Some(self.set_key_item());
        }
        if let Some(node) = self.cost_node(index) {
            return Some(self.node_item(node));
        }
        let at = self.figure_at(index);
        self.cost.figures.get(at).map(|figure| {
            if at == 0 {
                let (amount, rest) = figure.split_amount();
                return self.primary_item(figure.name.clone(), amount, None, rest.into(), COST_ICON);
            }
            figure_item(figure)
        })
    }

    pub(crate) fn spending_item(&self, index: usize) -> Option<Item> {
        page_item(self.spending(), index)
    }

    pub(crate) fn share_item(&self, index: usize) -> Option<Item> {
        page_item(self.share(), index)
    }

    /// The number of the cost gathering to run.
    pub fn cost_request(&self) -> Option<u32> {
        self.pending_cost().filter(|_| self.is_awake())
    }

    fn pending_cost(&self) -> Option<u32> {
        let is_on_screen = self.screen == Some(Screen::Cost) && self.cost.is_counting;
        (is_on_screen || self.does_cost_line_show() && !self.cost.is_total_known).then_some(self.cost.gathering)
    }

    pub(crate) fn cost_tick(&mut self) {
        self.cost.is_total_known &=
            self.does_cost_line_show() || matches!(self.screen, Some(Screen::Cost | Screen::Spending | Screen::Share));
    }

    fn does_cost_line_show(&self) -> bool {
        let nodes = self.tree.level().nodes;
        let is_cost = |node: &Node| matches!(node.content, Content::Action(Use::Open(Destination::Cost)));
        self.screen.is_none() && nodes.iter().any(is_cost)
    }

    /// Takes a gathering's figures; true if the screen changed.
    #[must_use]
    pub fn set_cost(&mut self, gathering: u32, figures: Vec<Figure>) -> bool {
        if self.pending_cost() != Some(gathering) {
            return false;
        }
        let total = figures.first().map(|total| String::from(total.month_part()));
        (self.cost.total, self.cost.is_total_known) = (total, true);
        if self.screen != Some(Screen::Cost) {
            return self.redeal_lines(&[Use::Open(Destination::Cost)]);
        }
        (self.cost.is_counting, self.cost.figures) = (false, figures);
        self.show_lines(List::Cost, self.set_key_at() + 1, None);
        true
    }

    // Where the settings' lines begin: behind the figures the list shows.
    fn settings_at(&self) -> usize {
        self.cost.figures.len().saturating_sub(self.figure_at(0))
    }

    fn set_key_at(&self) -> usize {
        self.settings_at() + SETTINGS.len()
    }

    // The setting's node a line of the list stands for.
    pub(in crate::ui) fn cost_node(&self, index: usize) -> Option<&'static Node> {
        let nodes: &'static [Node] = &SETTINGS;
        nodes.get(index.checked_sub(self.settings_at())?)
    }

    pub(super) fn deal_cost(&mut self) {
        self.deal_lines(List::Cost, |_| true);
    }

    pub(super) fn pick_cost(&mut self) {
        if self.cost.grid.selection == self.set_key_at() {
            return self.open_key(Some(Screen::Cost));
        }
        match self.spending().map(|page| page.lines.len()) {
            Some(lines) => self.open_cost_page(List::Spending, lines, Screen::Spending),
            None => self.cost.grid.swing(1.0),
        }
    }

    pub(super) fn pick_spending(&mut self) {
        match self.share().map(|page| page.lines.len()) {
            Some(lines) => self.open_cost_page(List::Share, lines, Screen::Share),
            None => self.cost.spending.swing(1.0),
        }
    }

    pub(super) fn pick_share(&mut self) {
        self.cost.share.swing(1.0);
    }

    fn open_cost_page(&mut self, list: List, lines: usize, screen: Screen) {
        self.depart(FORWARD);
        self.show_lines(list, lines, None);
        self.screen = Some(screen);
    }

    fn figure_at(&self, index: usize) -> usize {
        index + self.primary_gap()
    }

    pub(super) fn spending(&self) -> Option<&Page> {
        self.cost.figures.get(self.figure_at(self.cost.grid.selection))?.page.as_ref()
    }

    pub(super) fn share(&self) -> Option<&Page> {
        self.spending()?.lines.get(self.cost.spending.selection)?.page.as_ref()
    }

    pub(super) fn view_cost(&self) -> View<'_> {
        let total = self.cost.figures.first().map(|figure| figure.value.as_str());
        let words = BarWords {
            status: self.cost.is_counting.then_some(COUNTING),
            figure: total.filter(|total| !total.is_empty()),
            place: self.place_in(List::Cost),
            speech: Some(write_speech),
            ..BarWords::default()
        };
        View::Lines { list: List::Cost, name: COST, words }
    }

    fn view_page<'a>(&'a self, list: List, page: Option<&'a Page>) -> View<'a> {
        let words = BarWords { subject: Some(page.map_or("", |page| page.subject.as_str())), ..BarWords::default() };
        View::Lines { list, name: page.map_or("", |page| page.name), words }
    }

    pub(super) fn view_spending(&self) -> View<'_> {
        self.view_page(List::Spending, self.spending())
    }

    pub(super) fn view_share(&self) -> View<'_> {
        self.view_page(List::Share, self.share())
    }
}
