use super::{Form, Item, before_blocks, paint_blocks, paint_sample};
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::font::Font;
use crate::gfx::measures::PHI;
use crate::gfx::palette::{self, Roles};
use crate::gfx::pen::Pen;
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::field::{cells, cut, paint_field};
use crate::ui::gauge::Sweep;
use crate::ui::kit::Kit;
use alloc::borrow::Cow;
use alloc::string::String;

const LEAD_MARK: &str = "\u{203A}";
const DOTS: &str = "................................";
const OPEN: &str = "[ ";
const CLOSE: &str = " ]";
pub(in crate::ui) const BUTTON_MARKS: usize = OPEN.len() + CLOSE.len();
const RECORDING_MARK: &str = "[ REC ]";
const BRIEF_CELLS: i32 = 12;
const LENGTH_CELLS: i32 = 5;
const COLUMN_GAP_CELLS: i32 = 2;
const WHEN: &str = "WHEN";
const LENGTH: &str = "LEN";
const WHAT: &str = "WHAT";
const TIMED_COLUMNS: [(&str, i32); 3] =
    [(WHEN, 0), (LENGTH, BRIEF_CELLS + COLUMN_GAP_CELLS), (WHAT, BRIEF_CELLS + LENGTH_CELLS + 2 * COLUMN_GAP_CELLS)];
const PLAIN_COLUMNS: [(&str, i32); 2] = [(WHEN, 0), (WHAT, BRIEF_CELLS + COLUMN_GAP_CELLS)];
const UNWRITTEN: &str = "not written down yet";

fn paint_ground(plate: &mut Surface<'_>, roles: &Roles, is_reversed: bool, is_faint: bool) -> (Color, Color, Color) {
    let (ground, colours) = if is_reversed {
        (roles.text, (roles.background, roles.background, roles.background))
    } else {
        (roles.background, (if is_faint { roles.text_dim } else { roles.text }, roles.text_dim, roles.rule))
    };
    plate.fill(plate.bounds(), ground);
    colours
}

#[derive(Clone, Copy)]
pub(in crate::ui) struct Table {
    pub(in crate::ui) has_lengths: bool,
}

impl Table {
    const fn columns(self) -> &'static [(&'static str, i32)] {
        if self.has_lengths { &TIMED_COLUMNS } else { &PLAIN_COLUMNS }
    }

    fn column(self, kit: &Kit<'_>, name: &str) -> Option<i32> {
        let column = self.columns().iter().find(|column| column.0 == name);
        column.map(|column| kit.ruler.typed_left() + column.1 * kit.ruler.small_cell())
    }

    pub(in crate::ui) fn draw_head(self, kit: &Kit<'_>, pen: &mut Pen<'_, '_>, y: i32) {
        let ruler = kit.ruler;
        for (name, at) in self.columns() {
            let x = ruler.typed_left() + at * ruler.small_cell();
            pen.text(None, &ruler.body, (x, y), name, kit.roles.text_dim, 1);
        }
    }
}

fn typed_edge(kit: &Kit<'_>, item: &Item, width: i32) -> i32 {
    let lead = if item.keeps_lead_room() { kit.ruler.cell() + kit.ruler.gap } else { 0 };
    width - kit.ruler.typed_left() - lead
}

fn typed_rooms(kit: &Kit<'_>, (name, detail): (&str, &str), right: i32) -> (i32, i32) {
    let ruler = kit.ruler;
    let left = ruler.typed_left();
    if detail.is_empty() {
        return (right - left, 0);
    }
    let room = (right - left - ruler.cell() - 2 * ruler.small_cell()).max(0);
    let (name_width, detail_width) = (ruler.heading.text_width(name), ruler.body.text_width(detail));
    if name_width + detail_width <= room {
        return (name_width, detail_width);
    }
    let name = (room - detail_width).max(name_width.min(round(room as f32 / PHI)));
    (name, room - name)
}

fn end_of(font: &Font, x: i32, text: &str, room: i32) -> i32 {
    x + font.text_width(text).min(room)
}

fn detail_start(kit: &Kit<'_>, detail: &str, (right, room): (i32, i32)) -> i32 {
    right - kit.ruler.body.text_width(detail).min(room)
}

// The pacing sweep of a typed line without lead or blocks: between its name and its detail.
pub(in crate::ui) fn typed_sweep(
    kit: &Kit<'_>,
    (name, detail): (&str, &str),
    (width, height): (usize, usize),
) -> Sweep {
    let ruler = kit.ruler;
    let (left, right, small_cell) = (ruler.typed_left(), width as i32 - ruler.typed_left(), ruler.small_cell());
    let (name_room, room) = typed_rooms(kit, (name, detail), right);
    Sweep {
        start: end_of(&ruler.heading, left, name, name_room) + small_cell,
        end: detail_start(kit, detail, (right, room)) - small_cell,
        top: (height as i32 - ruler.block().1) / 2,
    }
}

fn paint_lead(kit: &Kit<'_>, plate: &mut Surface<'_>, colour: Color) {
    let ruler = kit.ruler;
    let (x, y) = (plate.width() - ruler.typed_left() - ruler.cell(), (plate.height() - ruler.large()) / 2);
    plate.text(plate.bounds(), &ruler.heading, (x, y), LEAD_MARK, colour, 1, Lettering::default());
}

fn recording_text(kit: &Kit<'_>, item: &Item) -> (i32, String) {
    let ruler = kit.ruler;
    let x = ruler.typed_left() + ruler.body.text_width(RECORDING_MARK) + ruler.small_cell();
    (x, [item.name.as_str(), item.word.as_str()].join(" "))
}

pub(super) fn paint_typed(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, is_reversed: bool) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (ink, dim, faint) = paint_ground(plate, &roles, is_reversed, item.is_faint);
    let (width, height) = (plate.width(), plate.height());
    let (whole, plain) = (plate.bounds(), Lettering::default());
    let (small_cell, small, cell, large, left) =
        (ruler.small_cell(), ruler.small(), ruler.cell(), ruler.large(), ruler.typed_left());
    let middle = (height - small) / 2;
    if item.is_recording {
        let red = if is_reversed { ink } else { roles.alert };
        plate.text(whole, &ruler.body, (left, middle), RECORDING_MARK, red, 1, plain);
        let well = is_reversed.then_some(roles.background);
        let right = paint_blocks(kit, plate, item, (width - left, height / 2), well);
        let (x, text) = recording_text(kit, item);
        paint_field(kit, plate, &ruler.body, (x, middle), text.trim(), right - x, red);
        return;
    }
    if item.has_lead {
        paint_lead(kit, plate, dim);
    }
    let edge = typed_edge(kit, item, width);
    let right = paint_blocks(kit, plate, item, (edge, height / 2), is_reversed.then_some(roles.background));
    let (detail, is_text) = item.detail();
    let (name_room, room) = typed_rooms(kit, (&item.name, detail), right);
    let name_end = paint_field(kit, plate, &ruler.heading, (left, (height - large) / 2), &item.name, name_room, ink);
    if detail.is_empty() {
        return;
    }
    let detail = if is_text { cut(detail, cells(room, &ruler.body)) } else { detail };
    let start = detail_start(kit, detail, (right, room));
    let mut dots = cells(start - name_end - cell - small_cell, &ruler.body);
    let mut x = name_end + cell + small_cell / 2;
    while dots > 0 {
        let run = dots.min(DOTS.len());
        x = plate.text(whole, &ruler.body, (x, middle), &DOTS[..run], faint, 1, plain);
        dots -= run;
    }
    paint_field(kit, plate, &ruler.body, (start, middle), detail, right - start, dim);
}

pub(super) fn paint_palette(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    plate.fill(plate.bounds(), roles.background);
    let (width, height) = (plate.width(), plate.height());
    let (cell, large, small, left) = (ruler.cell(), ruler.large(), ruler.small(), ruler.typed_left());
    if let Some(at) = item.palette {
        let (letter, at, plain) = (palette::letter(at), (left, (height - large) / 2), Lettering::default());
        plate.text(plate.bounds(), &ruler.heading, at, letter.encode_utf8(&mut [0u8; 4]), roles.mark, 1, plain);
    }
    let (side, right) = (ruler.line_icon_side(), width - left);
    plate.fill(Rect::new(right - side, (height - small) / 2, side, small), roles.text);
    let (x, top) = (left + 2 * cell, (height - 2 * small) / 2);
    let room = right - side - ruler.gap - x;
    paint_field(kit, plate, &ruler.body, (x, top), &item.name, room, roles.text);
    paint_sample(kit, plate, &roles, (x, top + small), room, true);
}

pub(super) fn paint_typed_figure(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, is_reversed: bool) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (ink, dim, _) = paint_ground(plate, &roles, is_reversed, item.is_faint);
    let (left, top, small) = (ruler.typed_left(), ruler.gap / 2, ruler.small());
    if item.has_lead {
        paint_lead(kit, plate, dim);
    }
    let room = typed_edge(kit, item, plate.width()) - left;
    paint_field(kit, plate, &ruler.body, (left, top), &item.name, room, dim);
    let value: Cow<'_, str> = if item.text.is_empty() {
        Cow::Borrowed(&item.word)
    } else {
        [item.word.as_str(), &item.text].join(" ").into()
    };
    paint_field(kit, plate, &ruler.heading, (left, top + small), &value, room, ink);
}

fn button_room(kit: &Kit<'_>, item: &Item, width: i32) -> i32 {
    let ruler = kit.ruler;
    let marks = BUTTON_MARKS as i32 * ruler.small_cell();
    ruler.body.text_width(&item.name).min(width - ruler.typed_left() - marks).max(0)
}

pub(super) fn paint_button(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, is_reversed: bool) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (ink, ..) = paint_ground(plate, &roles, is_reversed, item.is_faint);
    let room = button_room(kit, item, plate.width());
    let y = (plate.height() - ruler.small()) / 2;
    let (whole, plain) = (plate.bounds(), Lettering::default());
    let name_left = plate.text(whole, &ruler.body, (ruler.typed_left(), y), OPEN, ink, 1, plain);
    let name_end = paint_field(kit, plate, &ruler.body, (name_left, y), &item.name, room, ink);
    plate.text(whole, &ruler.body, (name_end, y), CLOSE, ink, 1, plain);
}

fn brief_field<'a>(kit: &Kit<'_>, table: Table, item: &'a Item) -> (i32, &'a str, i32) {
    let when = table.column(kit, WHEN).unwrap_or(kit.ruler.typed_left());
    let next = table.column(kit, LENGTH).or_else(|| table.column(kit, WHAT)).unwrap_or(when);
    let brief = if item.brief.is_empty() { &item.name } else { &item.brief };
    (when, brief, next - when - COLUMN_GAP_CELLS * kit.ruler.small_cell())
}

pub(super) fn paint_entry_typed(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, table: Table, is_reversed: bool) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (ink, dim, _) = paint_ground(plate, &roles, is_reversed, item.is_faint);
    let small = ruler.small();
    let top = (plate.height() - 2 * small) / 2;
    let (when, brief, brief_room) = brief_field(kit, table, item);
    let what = table.column(kit, WHAT).unwrap_or(when);
    paint_field(kit, plate, &ruler.body, (when, top), brief, brief_room, ink);
    if let Some(length) = table.column(kit, LENGTH) {
        let room = what - length - COLUMN_GAP_CELLS * ruler.small_cell();
        paint_field(kit, plate, &ruler.body, (length, top), &item.word, room, dim);
    }
    let (whole, plain) = (plate.bounds(), Lettering::default());
    if item.has_lead {
        plate.text(whole, &ruler.body, (what, top), LEAD_MARK, dim, 1, plain);
    }
    let room = plate.width() - 2 * when;
    if item.text.is_empty() {
        paint_field(kit, plate, &ruler.body, (when, top + small), UNWRITTEN, room, dim);
    } else {
        let colour = if is_reversed { ink } else { roles.content };
        let text = cut(&item.text, cells(room, &ruler.body));
        plate.text(whole, &ruler.body, (when, top + small), text, colour, 1, plain);
    }
}

pub(in crate::ui) fn caret_of(
    kit: &Kit<'_>,
    item: &Item,
    form: Form,
    table: Table,
    (width, height): (usize, usize),
) -> Option<Rect> {
    let ruler = kit.ruler;
    let (width, height) = (width as i32, height as i32);
    let (small_cell, small, left) = (ruler.small_cell(), ruler.small(), ruler.typed_left());
    let caret_after = |font: &Font, x: i32, text: &str, room: i32, y: i32| {
        Rect::new(end_of(font, x, text, room), y, font.width as i32, font.height as i32)
    };
    Some(match form {
        Form::PaletteTyped | Form::PaletteCard(_) => return None,
        Form::FigureTyped => {
            caret_after(&ruler.body, left, &item.name, typed_edge(kit, item, width) - left, ruler.gap / 2)
        }
        Form::Button => {
            let x = left + button_room(kit, item, width) + BUTTON_MARKS as i32 * small_cell;
            Rect::new(x, (height - small) / 2, small_cell, small)
        }
        Form::EntryTable => {
            let (when, brief, room) = brief_field(kit, table, item);
            caret_after(&ruler.body, when, brief, room, (height - 2 * small) / 2)
        }
        _ if item.is_recording => {
            let (x, text) = recording_text(kit, item);
            let room = before_blocks(kit, item, width - left) - x;
            caret_after(&ruler.body, x, text.trim(), room, (height - small) / 2)
        }
        _ => {
            let right = before_blocks(kit, item, typed_edge(kit, item, width));
            let name_room = typed_rooms(kit, (&item.name, item.detail().0), right).0;
            caret_after(&ruler.heading, left, &item.name, name_room, (height - ruler.large()) / 2)
        }
    })
}
