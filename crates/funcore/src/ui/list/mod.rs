pub(super) mod dial;
pub(super) mod stage;

use super::field::{Tickers, cells, draw_tickers, paint_with_tickers};
use super::grid::{Band, Cell, Grid};
use super::item::{BUTTON_MARKS, Form, Item, Role, Table, caret_of, entry_marked_height, large_height};
use super::kit::{Kit, draw_frame, find_glyph, trace};
use super::ruler::{RULE, Ruler};
use crate::base::math::round;
use crate::gfx::glyph;
use crate::gfx::key::Key;
use crate::gfx::pen::Pen;
use crate::gfx::store::{AsPicture, Store};
use crate::gfx::{Picture, Rect};
use alloc::vec::Vec;

pub(super) const BOARD_COLUMNS: usize = 2;
pub(super) const BOARD_ROWS: usize = 4;
pub(super) const CARD_ROWS: usize = 2;
const ACTION_COLUMNS: usize = 3;
pub(super) const MORE: &str = icon!("keyboard_arrow_down");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ListForm {
    Rows,
    Dial,
    Typed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Page {
    Lines(usize),
    Fill,
    Board,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Primary {
    Hidden,
    Line,
    Card,
    Large,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Forms {
    pub(super) figure: Form,
    pub(super) action: Form,
    pub(super) entry: Form,
    pub(super) plain: Form,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ListStyle {
    pub(super) form: ListForm,
    pub(super) page: Page,
    pub(super) has_rules: bool,
    pub(super) is_carded: bool,
    pub(super) is_at_foot: bool,
    pub(super) has_arrow: bool,
    pub(super) primary: Primary,
    pub(super) forms: Forms,
}

fn even_height(ruler: &Ruler, min_height: i32) -> f32 {
    let (room, gap) = (ruler.under_bar().h.max(0), ruler.gap);
    let rows = ((room + gap) / (min_height + gap).max(1)).max(1) as usize;
    Band::even(usize::MAX, 1, room as f32, gap as f32, rows).height
}

impl ListStyle {
    const fn form_of(&self, role: Role) -> Form {
        match (role, self.primary) {
            (Role::Primary(level), Primary::Card) => Form::PrimaryCard(level),
            (Role::Primary(level), Primary::Large) => Form::PrimaryLarge(level),
            (Role::Primary(_) | Role::Figure, _) => self.forms.figure,
            (Role::Action, _) => self.forms.action,
            (Role::Entry, _) => self.forms.entry,
            (Role::Plain, _) => self.forms.plain,
        }
    }

    pub(super) fn form_for(&self, item: &Item) -> Form {
        if item.palette.is_some() && self.form == ListForm::Typed {
            Form::PaletteTyped
        } else {
            self.form_of(item.role)
        }
    }

    pub(super) fn page_rows(&self, ruler: &Ruler) -> usize {
        match self.page {
            Page::Lines(lines) => lines,
            Page::Fill => (ruler.under_bar().h / ruler.line_icon_side()).max(1) as usize,
            Page::Board => BOARD_ROWS,
        }
    }

    fn line_band(&self, ruler: &Ruler) -> Band {
        match self.form {
            ListForm::Typed => {
                Band { items: usize::MAX, columns: 1, height: ruler.line_icon_side() as f32, caption: 0.0 }
            }
            ListForm::Rows | ListForm::Dial => {
                let room = ruler.under_bar().h as f32;
                Band::even(usize::MAX, 1, room, ruler.gap as f32, self.page_rows(ruler))
            }
        }
    }

    fn board_height(&self, ruler: &Ruler, places: usize) -> f32 {
        let (gap, place) = (ruler.gap as f32, self.line_band(ruler).height);
        places as f32 * place + places.saturating_sub(1) as f32 * gap
    }

    fn shape(&self, kit: &Kit<'_>, item: &Item) -> (usize, f32) {
        let ruler = kit.ruler;
        let (band, gap) = (self.line_band(ruler).height, ruler.gap as f32);
        let (large, small) = (ruler.large() as f32, ruler.small() as f32);
        match self.form_for(item) {
            Form::PrimaryCard(_) => (1, self.board_height(ruler, CARD_ROWS)),
            Form::PrimaryLarge(_) => (1, large_height(kit, item, ruler.under_bar().w) as f32),
            Form::FigureTile | Form::ActionTile => (BOARD_COLUMNS, band),
            Form::FigureTyped => (1, small + large + gap),
            Form::Columns => (1, small + 2.0 * gap),
            Form::Button => (ACTION_COLUMNS, band),
            Form::EntryMarked => (1, even_height(ruler, entry_marked_height(kit, item))),
            Form::EntryTable | Form::PaletteTyped => (1, 2.0 * small + gap),
            Form::EntryCard
            | Form::Row(_)
            | Form::Typed
            | Form::Hero(_)
            | Form::MenuTile
            | Form::PaletteCard(_)
            | Form::Icon => (1, band),
        }
    }

    pub(super) const fn is_on_card(&self, role: Role) -> bool {
        matches!(self.form_of(role), Form::PrimaryCard(_) | Form::FigureTile | Form::ActionTile | Form::EntryCard)
    }

    fn share_actions(&self, kit: &Kit<'_>, bands: &mut [Band], plans: &[LinePlan], items: &[Option<Item>]) {
        let ruler = kit.ruler;
        let width = if self.form == ListForm::Typed { ruler.width } else { ruler.under_bar().w };
        let mut first = 0;
        for band in bands {
            if self.page != Page::Board && plans.get(first).is_some_and(|plan| plan.role == Role::Action) {
                band.columns = band.columns.min(band.items).max(1);
                let lines = items.iter().skip(first).take(band.items).flatten();
                let longest = lines.map(|item| glyph::cells_of(&item.name)).max().unwrap_or(0);
                let room = cells(width / band.columns as i32 - ruler.typed_left(), &ruler.body);
                if self.forms.action == Form::Button && longest + BUTTON_MARKS > room {
                    band.columns = 1;
                }
            }
            first += band.items;
        }
    }
}

#[derive(Default)]
pub(super) struct LinePicture {
    pub(super) picture: Picture,
    pub(super) tickers: Tickers,
    pub(super) reversed: Option<ReversedPicture>,
}

impl AsPicture for LinePicture {
    fn picture(&self) -> &Picture {
        &self.picture
    }
}

#[derive(Default)]
pub(super) struct ReversedPicture {
    picture: Picture,
    caret: Option<Rect>,
    tickers: Tickers,
}

impl LinePicture {
    pub(super) fn paint(
        spare: Option<LinePicture>,
        kit: &Kit<'_>,
        (item, form, table): (&Item, Form, Table),
        size: (usize, usize),
        is_reversed: bool,
    ) -> LinePicture {
        let LinePicture { picture: previous, tickers: slots, reversed: spare_reversed } = spare.unwrap_or_default();
        let (picture, tickers) = paint_with_tickers(kit, (previous.words, slots), size, |plate| {
            item.paint(kit, plate, form, table, false);
        });
        let reversed = is_reversed.then(|| {
            let ReversedPicture { picture: previous, tickers: slots, .. } = spare_reversed.unwrap_or_default();
            let (picture, tickers) = paint_with_tickers(kit, (previous.words, slots), size, |plate| {
                item.paint(kit, plate, form, table, true);
            });
            ReversedPicture { picture, caret: caret_of(kit, item, form, table, size), tickers }
        });
        LinePicture { picture, tickers, reversed }
    }
}

#[derive(Clone, Copy)]
enum Heading {
    Caption(&'static str),
    Table,
}

#[derive(Clone, Copy)]
struct LinePlan {
    role: Role,
    heading: Option<Heading>,
}

const NO_PLAN: LinePlan = LinePlan { role: Role::Plain, heading: None };

#[derive(Clone, Copy)]
pub(super) enum Mark {
    Frame(i32),
    Reversed { caret: Option<bool> },
    Nothing,
}

#[derive(Clone, Copy)]
pub(super) struct RowsStyle {
    pub(super) has_rules: bool,
    pub(super) has_arrow: bool,
    pub(super) mark: Mark,
    pub(super) table: Table,
}

#[derive(Default)]
pub(super) struct Lines {
    pub(super) store: Store<LinePicture>,
    plans: Vec<LinePlan>,
    pub(super) round: Option<u32>,
    pub(super) items: Vec<Option<Item>>,
}

impl Lines {
    pub(super) fn plan(&mut self, kit: &Kit<'_>, style: &ListStyle, items: &[Option<Item>], bands: &mut Vec<Band>) {
        let (ruler, plans) = (kit.ruler, &mut self.plans);
        bands.clear();
        plans.clear();
        let caption_height = (ruler.small() + ruler.gap) as f32;
        for item in items {
            let Some(item) = item else {
                plans.push(NO_PLAN);
                continue;
            };
            let (columns, height) = style.shape(kit, item);
            let begins_table = item.role == Role::Entry
                && style.forms.entry == Form::EntryTable
                && plans.last().is_none_or(|last| last.role != Role::Entry);
            let caption = item.caption.filter(|_| style.has_rules).map(Heading::Caption);
            let heading = caption.or(begins_table.then_some(Heading::Table));
            plans.push(LinePlan { role: item.role, heading });
            match bands.last_mut() {
                Some(band) if heading.is_none() && band.columns == columns && band.height == height => band.items += 1,
                _ => {
                    let caption = if heading.is_some() { caption_height } else { 0.0 };
                    bands.push(Band { items: 1, columns, height, caption });
                }
            }
        }
        if bands.is_empty() {
            bands.push(style.line_band(ruler));
        }
        style.share_actions(kit, bands, plans, items);
        if let Some(last) = bands.last_mut() {
            last.items = usize::MAX;
        }
    }

    fn plan_of(&self, item: usize) -> LinePlan {
        self.plans.get(item).copied().unwrap_or(NO_PLAN)
    }

    pub(super) fn role_of(&self, item: usize) -> Role {
        self.plan_of(item).role
    }

    fn draw_line(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, cell: &Cell) {
        self.store.draw(pen, (cell.spot, cell.placement), (cell.area, cell.area), |pen, line, at, within| {
            draw_tickers(kit, pen, &line.tickers, at, within);
        });
    }

    pub(super) fn draw_rows(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, grid: &Grid, style: &RowsStyle) {
        let (ruler, roles) = (kit.ruler, kit.roles);
        let (gap, inset) = (ruler.gap, ruler.framed_inset());
        draw_more(pen, kit, grid, style.has_arrow);
        let frame = grid.frame_area();
        let is_beside_frame = |y: i32| frame.is_some_and(|frame| y > frame.y - gap && y < frame.bottom() + gap);
        let mut last: Option<Rect> = None;
        let departure = grid.departure_places();
        let pinned = departure..departure + grid.pinned_places();
        let is_pin_selected = grid.selection < grid.pinned_places();
        for (index, cell) in grid.cells().enumerate().filter(|entry| entry.1.has_item && !pinned.contains(&entry.0)) {
            let area = cell.area;
            self.draw_line(pen, kit, &cell);
            let item = index.checked_sub(departure).map(|at| grid.item_of(at));
            let is_headed = item.is_some_and(|item| self.plan_of(item).heading.is_some());
            let above =
                last.filter(|above| style.has_rules && !is_headed && above.bottom() < area.y && above.x == area.x);
            if let Some(above) = above {
                let y = (above.bottom() + area.y) / 2;
                if !is_beside_frame(y) {
                    pen.fill(Rect::new(area.x + inset, y, area.w - 2 * inset, RULE), roles.rule);
                }
            }
            last = Some(area);
        }
        for (item, room) in grid.captions() {
            match self.plan_of(item).heading {
                Some(Heading::Table) => style.table.draw_head(kit, pen, room.y),
                Some(Heading::Caption(caption)) => {
                    pen.text(None, &ruler.body, (room.x + inset, room.y), caption, roles.text_dim, 1);
                }
                None => {}
            }
        }
        if !is_pin_selected {
            self.draw_mark(pen, kit, grid, style.mark);
        }
        let pinned_cells = || grid.cells().skip(pinned.start).take(pinned.len()).filter(|cell| cell.has_item);
        if let Some(foot) = pinned_cells().map(|cell| cell.area.bottom()).max() {
            pen.fill(Rect::new(0, 0, ruler.width, foot), roles.background);
            pinned_cells().for_each(|cell| self.draw_line(pen, kit, &cell));
        }
        if is_pin_selected {
            self.draw_mark(pen, kit, grid, style.mark);
        }
    }

    fn draw_mark(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, grid: &Grid, mark: Mark) {
        let Some(frame) = grid.frame_area() else {
            return;
        };
        match mark {
            Mark::Frame(width) => draw_frame(pen, frame, width, kit.roles.glow),
            Mark::Reversed { caret } => self.draw_reversed(pen, kit, grid, frame, caret),
            Mark::Nothing => {}
        }
    }

    fn draw_reversed(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, grid: &Grid, frame: Rect, caret: Option<bool>) {
        let roles = kit.roles;
        pen.fill(frame, roles.text);
        for place in 0..grid.places() {
            let item = grid.item_of(place);
            let Some(area) = grid.area_of(item).filter(|area| area.clip(frame).is_some()) else {
                continue;
            };
            let line = grid.spot_of(item).and_then(|spot| self.store.get(spot));
            let Some((LinePicture { reversed: Some(reversed), .. }, id)) = line else {
                continue;
            };
            let ReversedPicture { picture, caret: block, tickers } = reversed;
            let name = Key::default().word(id).word(1).value();
            pen.blit(frame, area.x, area.y, picture.width, &picture.words, name);
            draw_tickers(kit, pen, tickers, (area.x, area.y), frame);
            if let (Some(block), Some(is_lit)) = (block.filter(|_| item == grid.selection), caret) {
                let block = Rect::new(area.x + block.x, area.y + block.y, block.w, block.h);
                let colour = if is_lit { roles.background } else { roles.text };
                pen.fill(block.clip(frame).unwrap_or(Rect::new(0, 0, 0, 0)), colour);
            }
        }
    }
}

pub(super) fn draw_more(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, grid: &Grid, has_arrow: bool) {
    let side = kit.ruler.small() as f32;
    let arrow = find_glyph(kit.glyphs, MORE).filter(|_| has_arrow);
    let Some((arrow, (low, high))) = arrow.and_then(|arrow| Some((arrow, arrow.extent()?))) else {
        return;
    };
    let (width, height) = ((high.x - low.x) * side, (high.y - low.y) * side);
    for room in grid.spare_rooms().filter(|room| room.h as f32 >= height) {
        let left = round(room.x as f32 + (room.w as f32 - width) / 2.0 - low.x * side) as f32;
        let top = round(room.y as f32 + (room.h as f32 - height) / 2.0 - low.y * side) as f32;
        pen.cover(kit.roles.text_dim, kit.roles.background, |mask| trace(mask, arrow, left, top, side));
    }
}
