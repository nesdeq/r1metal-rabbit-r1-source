mod companion;
mod deck;
mod dial;
mod rail;
mod stage;
mod terminal;

use super::bar::BarStyle;
use super::corner::CornerStyle;
use super::edit::CaretStyle;
use super::gauge::{ClockPlace, Gauge, GaugePlace};
use super::grid::Grid;
use super::keys::KeysStyle;
use super::list::ListStyle;
use super::picture::RollStyle;
use super::ruler::Ruler;
use super::textroom::RoomStyle;
use crate::gfx::key::Ids;
use crate::gfx::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Motion {
    Slide,
    Roll,
    Deal,
    Print,
    Typed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HomeWords {
    Nothing,
    Prompt,
    Sentence,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum HomeSeat {
    Flight,
    Square,
    Bar,
    Hero,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct HomeStyle {
    pub(super) is_suggested: bool,
    pub(super) has_stages: bool,
    pub(super) has_hero: bool,
    pub(super) words: HomeWords,
    pub(super) seat: HomeSeat,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Look {
    pub(super) list: ListStyle,
    pub(super) bar: BarStyle,
    pub(super) is_one_size: bool,
    pub(super) has_status: bool,
    pub(super) room: RoomStyle,
    pub(super) keys: KeysStyle,
    pub(super) caret: CaretStyle,
    pub(super) roll: RollStyle,
    pub(super) motion: Motion,
    pub(super) corner: CornerStyle,
    pub(super) home: HomeStyle,
    pub(super) has_rail: bool,
}

pub(super) trait Layout: Sync {
    fn look(&self) -> Look;

    fn home_grid(&self, ids: &Ids, ruler: &Ruler, count: usize) -> Grid {
        Grid::new(ids, ruler.under_bar(), ruler.height, ruler.gap, 1, count.max(1))
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, count: usize) -> GaugePlace;

    fn place_clock(&self, ruler: &Ruler) -> ClockPlace;
}

pub(super) fn layout_of(theme: Theme) -> &'static dyn Layout {
    match theme {
        Theme::Stage => &stage::StageLayout,
        Theme::Dial => &dial::DialLayout,
        Theme::Deck => &deck::DeckLayout,
        Theme::Terminal => &terminal::TerminalLayout,
        Theme::Companion => &companion::CompanionLayout,
        Theme::Rail => &rail::RailLayout,
    }
}
