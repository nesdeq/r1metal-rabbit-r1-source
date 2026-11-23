mod band;
mod card;
mod large;
mod row;
mod tile;
mod typed;

pub(super) use band::{band_blocks, paint_band};
pub(super) use large::{entry_marked_height, large_height};
pub(super) use row::{line_blocks, row_sweep};
pub(super) use typed::{BUTTON_MARKS, Table, caret_of, typed_sweep};

use super::field::{cells, cut, paint_field};
use super::gauge::BlockRow;
use super::kit::Kit;
use super::words::PALETTE_NAMES;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::key::Key;
use crate::gfx::palette::Roles;
use crate::gfx::{Lettering, Rect, Surface};
use crate::hw::sound::LOUDNESS_STEPS;
use alloc::string::String;
use core::hash::{Hash, Hasher};

const PART_SEPARATOR: &str = ", ";
const SAMPLE_TIME: &str = "14:32 ";
const SAMPLE_TEXT: &str = "A text, ";
const SAMPLE_MARK: &str = "marked";
// A level is told in hundredths.
const WHOLE_LEVEL: u8 = 100;
const SIGNAL_FAINT: i32 = -90;
const SIGNAL_FULL: i32 = -50;
pub(super) const SIGNAL_BLOCKS: usize = LOUDNESS_STEPS;
pub(super) const CHEVRON: &str = icon!("chevron_right");
pub(super) const DISC: &str = "<svg viewBox=\"0 -960 960 960\"><path d=\"M950-480Q950-285.32 812.34-147.66Q674.68-10 480-10Q285.32-10 147.66-147.66Q10-285.32 10-480Q10-674.68 147.66-812.34Q285.32-950 480-950Q674.68-950 812.34-812.34Q950-674.68 950-480Z\"/></svg>";
pub(super) const FIGURE_SCALE: i32 = 2;
pub(super) const LARGE_SCALE: i32 = 3;
pub(super) const ENTRY_LINES: usize = 2;

fn lit_count(signal: i32) -> usize {
    let (span, level) = (SIGNAL_FULL - SIGNAL_FAINT, signal.clamp(SIGNAL_FAINT, SIGNAL_FULL) - SIGNAL_FAINT);
    ((level * SIGNAL_BLOCKS as i32 + span - 1) / span).max(1) as usize
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Aside {
    Nothing,
    Signal(i32),
    Swatches(usize),
    Meter,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Role {
    Plain,
    Primary(Option<u8>),
    Figure,
    Action,
    Entry,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct RowStyle {
    pub(super) has_icons: bool,
    pub(super) is_carded: bool,
    pub(super) is_led: bool,
    pub(super) has_leads: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Form {
    PrimaryCard(Option<u8>),
    PrimaryLarge(Option<u8>),
    FigureTile,
    FigureTyped,
    Columns,
    ActionTile,
    Button,
    EntryCard,
    EntryMarked,
    EntryTable,
    Row(RowStyle),
    Typed,
    Hero(i32),
    MenuTile,
    PaletteCard(usize),
    PaletteTyped,
    Icon,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Item {
    pub(super) icon: Option<&'static str>,
    pub(super) mark: Option<&'static str>,
    pub(super) name: String,
    pub(super) word: String,
    pub(super) text: String,
    pub(super) brief: String,
    pub(super) has_lead: bool,
    pub(super) is_faint: bool,
    pub(super) role: Role,
    pub(super) caption: Option<&'static str>,
    pub(super) is_recording: bool,
    pub(super) aside: Aside,
    pub(super) palette: Option<usize>,
}

impl Item {
    pub(super) fn new(name: impl Into<String>, word: impl Into<String>) -> Item {
        Item {
            icon: None,
            mark: None,
            name: name.into(),
            word: word.into(),
            text: String::new(),
            brief: String::new(),
            has_lead: false,
            is_faint: false,
            role: Role::Plain,
            caption: None,
            is_recording: false,
            aside: Aside::Nothing,
            palette: None,
        }
    }

    pub(super) fn lead(name: impl Into<String>, word: impl Into<String>) -> Item {
        Item { has_lead: true, ..Item::new(name, word) }
    }

    pub(super) fn figure(label: impl Into<String>, value: impl Into<String>) -> Item {
        Item { role: Role::Figure, ..Item::new(label, value) }
    }

    pub(super) fn primary(label: impl Into<String>, value: impl Into<String>, level: Option<u8>, text: String) -> Item {
        Item { role: Role::Primary(level), text, ..Item::new(label, value) }
    }

    pub(super) fn action(name: impl Into<String>, word: impl Into<String>, icon: &'static str) -> Item {
        Item { role: Role::Action, icon: Some(icon), ..Item::new(name, word) }
    }

    pub(super) fn entry(name: String, brief: String, length: String, text: String) -> Item {
        Item { role: Role::Entry, brief, text, ..Item::lead(name, length) }
    }

    pub(super) fn palette(at: usize, word: impl Into<String>) -> Item {
        Item { palette: Some(at), aside: Aside::Swatches(at), ..Item::new(PALETTE_NAMES[at], word) }
    }

    pub(super) fn with_caption(self, caption: &'static str) -> Item {
        Item { caption: Some(caption), ..self }
    }

    pub(super) fn with_faint(self, is_faint: bool) -> Item {
        Item { is_faint, ..self }
    }

    pub(super) fn with_icon(self, icon: &'static str) -> Item {
        Item { icon: Some(icon), ..self }
    }

    pub(super) fn with_mark(self, mark: &'static str) -> Item {
        Item { mark: Some(mark), ..self }
    }

    pub(super) fn with_aside(self, aside: Aside) -> Item {
        Item { aside, ..self }
    }

    pub(super) fn with_recording(self) -> Item {
        Item { is_recording: true, ..self }
    }

    const fn ink(&self, roles: &Roles) -> Color {
        if self.is_faint { roles.text_dim } else { roles.text }
    }

    const fn keeps_lead_room(&self) -> bool {
        self.has_lead || matches!(self.aside, Aside::Signal(_))
    }

    fn detail(&self) -> (&str, bool) {
        if self.word.is_empty() { (&self.text, !self.text.is_empty()) } else { (&self.word, false) }
    }

    pub(super) fn to_key(&self, size: (usize, usize), roles: &Roles) -> u64 {
        let mut key = Key::default();
        (self, size, roles).hash(&mut key);
        key.finish()
    }

    pub(super) fn paint(&self, kit: &Kit<'_>, plate: &mut Surface<'_>, form: Form, table: Table, is_reversed: bool) {
        match form {
            Form::PrimaryCard(level) => card::paint_primary_card(kit, plate, self, level),
            Form::PrimaryLarge(level) => large::paint_primary_large(kit, plate, self, level),
            Form::FigureTile => tile::paint_figure_tile(kit, plate, self),
            Form::FigureTyped => typed::paint_typed_figure(kit, plate, self, is_reversed),
            Form::Columns => large::paint_columns(kit, plate, self),
            Form::ActionTile => tile::paint_tile(kit, plate, self, true),
            Form::Button => typed::paint_button(kit, plate, self, is_reversed),
            Form::EntryCard => card::paint_entry_card(kit, plate, self),
            Form::EntryMarked => large::paint_entry_marked(kit, plate, self),
            Form::EntryTable => typed::paint_entry_typed(kit, plate, self, table, is_reversed),
            Form::Row(style) => row::paint_row(kit, plate, self, style),
            Form::Typed => typed::paint_typed(kit, plate, self, is_reversed),
            Form::Hero(side) => tile::paint_hero(kit, plate, self, side),
            Form::MenuTile => tile::paint_tile(kit, plate, self, false),
            Form::PaletteCard(at) => tile::paint_palette_card(kit, plate, at),
            Form::PaletteTyped => typed::paint_palette(kit, plate, self),
            Form::Icon => row::paint_icon_line(kit, plate, self),
        }
    }
}

fn split_value(value: &str) -> (&str, &str) {
    value.split_once(PART_SEPARATOR).unwrap_or((value, ""))
}

fn blocks_at(kit: &Kit<'_>, right: i32, middle: i32) -> BlockRow {
    let (block, low, gap) = kit.ruler.block();
    let first = right - SIGNAL_BLOCKS as i32 * (block + gap) + gap;
    BlockRow { first: Rect::new(first, middle - low / 2, block, low), step: block + gap }
}

fn before_blocks(kit: &Kit<'_>, item: &Item, right: i32) -> i32 {
    if item.aside == Aside::Nothing { right } else { blocks_at(kit, right, 0).first.x - kit.ruler.gap }
}

fn paint_blocks(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    item: &Item,
    (right, middle): (i32, i32),
    well: Option<Color>,
) -> i32 {
    let (block, low, gap) = kit.ruler.block();
    let roles = kit.roles;
    let colours: [Option<Color>; SIGNAL_BLOCKS] = match item.aside {
        Aside::Nothing => return right,
        Aside::Signal(dbm) => {
            core::array::from_fn(|at| Some(if at < lit_count(dbm) { item.ink(&roles) } else { roles.rule }))
        }
        Aside::Swatches(at) => {
            let [text, content, mark] = Roles::at(at).swatches();
            [None, Some(text), Some(content), Some(mark)]
        }
        Aside::Meter => [Some(roles.rule); SIGNAL_BLOCKS],
    };
    let row = blocks_at(kit, right, middle);
    if let (Some(well), Some(lit)) = (well, colours.iter().position(Option::is_some)) {
        let left = row.first.x + lit as i32 * (block + gap) - gap;
        plate.fill(Rect::new(left, middle - low / 2 - gap, right + gap - left, low + 2 * gap), well);
    }
    row.fill(plate, colours);
    before_blocks(kit, item, right)
}

fn paint_under(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    item: &Item,
    (x, y): (i32, i32),
    room: i32,
    colour: Color,
) -> i32 {
    let body = &kit.ruler.body;
    if item.word.is_empty() && !item.text.is_empty() {
        let text = cut(&item.text, cells(room, body));
        return plate.text(plate.bounds(), body, (x, y), text, colour, 1, Lettering::default());
    }
    paint_field(kit, plate, body, (x, y), &item.word, room, colour)
}

fn paint_sample(kit: &Kit<'_>, plate: &mut Surface<'_>, roles: &Roles, (x, y): (i32, i32), room: i32, has_time: bool) {
    let body = &kit.ruler.body;
    let strong = Lettering { is_strong: true, is_stressed: false };
    let parts = [
        (SAMPLE_TIME, roles.text_dim, Lettering::default()),
        (SAMPLE_TEXT, roles.content, Lettering::default()),
        (SAMPLE_MARK, roles.mark, strong),
    ];
    let mut left = x;
    for (part, colour, lettering) in parts.into_iter().skip(usize::from(!has_time)) {
        if left + body.text_width(part) > x + room {
            return;
        }
        left = plate.text(plate.bounds(), body, (left, y), part, colour, 1, lettering);
    }
}

fn paint_gauge(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    (x, y): (i32, i32),
    (width, height): (i32, i32),
    count: i32,
    level: u8,
) {
    let lit = round(f32::from(level.min(WHOLE_LEVEL)) * count as f32 / f32::from(WHOLE_LEVEL));
    let row = BlockRow { first: Rect::new(x, y, width, height), step: width + kit.ruler.lines().0 };
    row.fill(plate, (0..count).map(|block| Some(if block < lit { kit.roles.text } else { kit.roles.rule })));
}
