use super::keys::{KeyCaps, Keyboard, draw_keys};
use super::kit::{IconPlace, Kit, find_glyph};
use super::ruler::Ruler;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::Rect;
use crate::gfx::measures::PHI2;
use crate::gfx::pen::Pen;

pub(super) const EYE_ICONS: [&str; 2] = [icon!("visibility"), icon!("visibility_off")];
const DOT: char = '\u{2022}';
const DOTS: &str = "••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••••";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CaretStyle {
    Underline,
    Block,
}

#[derive(Default)]
pub(super) struct Caret {
    pub(super) start_ns: u64,
    pub(super) is_lit: bool,
    pub(super) due_ns: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) struct CaretMark {
    pub(super) style: CaretStyle,
    pub(super) is_lit: bool,
}

impl CaretMark {
    pub(super) fn draw(self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, caret: Rect) {
        match self.style {
            CaretStyle::Block => pen.fill(caret, if self.is_lit { kit.roles.rule } else { kit.roles.background }),
            CaretStyle::Underline => {
                let height = kit.ruler.lines().0;
                pen.fill(Rect::new(caret.x, caret.bottom() - height, caret.w, height), kit.roles.glow);
            }
        }
    }
}

pub(super) fn parts(text: &str, size: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    core::iter::from_fn(move || {
        let end = rest.char_indices().nth(size).map_or(rest.len(), |entry| entry.0);
        let (part, tail) = rest.split_at(end);
        rest = tail;
        (!part.is_empty()).then_some(part)
    })
}

fn draft_area(ruler: &Ruler, keys: &Keyboard) -> Rect {
    let (top, room) = (keys.top(), ruler.under_bar());
    let (gap, line) = (ruler.gap, ruler.small());
    let max_lines = (((top - ruler.bar()) as f32 / PHI2) as i32 / line.max(1)).max(1);
    let lines = (keys.caret().0 as i32 + 1).clamp(1, max_lines);
    Rect::new(room.x, top - gap - lines * line, room.w, lines * line)
}

pub(super) fn band_top(ruler: &Ruler, keys: &Keyboard) -> i32 {
    draft_area(ruler, keys).y - ruler.gap
}

pub(super) fn band_height(ruler: &Ruler, keys: &Keyboard) -> i32 {
    (ruler.height - band_top(ruler, keys)).max(0)
}

pub(super) fn band_foot(ruler: &Ruler, keys: &Keyboard, drop: f32) -> i32 {
    band_top(ruler, keys) + round(drop)
}

pub(super) fn band_aim(ruler: &Ruler, keys: &Keyboard, is_up: bool) -> f32 {
    if is_up { 0.0 } else { band_height(ruler, keys) as f32 }
}

pub(super) fn draw_band(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (keys, caps): (&Keyboard, KeyCaps<'_>),
    caret: CaretMark,
    is_up: bool,
    drop: f32,
) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (cell, line) = (ruler.small_cell(), ruler.small());
    let (draft, top, band) = (draft_area(ruler, keys), band_top(ruler, keys), round(drop));
    let status = ruler.height - ruler.status_height();
    if is_up && top + band > status {
        pen.fill(Rect::new(0, status, ruler.width, (top + band).min(ruler.height) - status), roles.background);
    }
    if band >= band_height(ruler, keys) {
        return;
    }
    pen.shift(band);
    pen.fill(Rect::new(0, top, ruler.width, (ruler.height - top).max(0)), roles.background);
    let (row, column) = keys.caret();
    let lines = (draft.h / line.max(1)).max(1) as usize;
    let first = (row + 1).saturating_sub(lines);
    let left = draft.x + keys.shake();
    let y = draft.y + row.saturating_sub(first) as i32 * line;
    caret.draw(pen, kit, Rect::new(left + round(column * cell as f32), y, cell, line));
    for (index, part) in parts(keys.text(), keys.line_length()).enumerate().skip(first) {
        pen.text(None, &ruler.body, (left, draft.y + (index - first) as i32 * line), part, roles.text, 1);
    }
    draw_keys(pen, kit, keys, caps);
    pen.shift(-band);
}

pub(super) fn eye_box(ruler: &Ruler) -> Rect {
    let (gap, side) = (ruler.gap, ruler.large());
    Rect::new(ruler.width - gap - side, ruler.bar() + gap, side, side)
}

pub(super) fn draw_eye(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, is_shown: bool) {
    let (eye, roles) = (eye_box(kit.ruler), kit.roles);
    if let Some(icon) = find_glyph(kit.glyphs, EYE_ICONS[usize::from(is_shown)]) {
        kit.draw_icon(pen, icon, IconPlace::at((eye.x, eye.y), eye.w), (roles.text_dim, roles.background));
    }
}

pub(super) fn draw_secret(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (line, colour): (Rect, Color),
    (text, is_shown): (&str, bool),
    caret: Option<(i32, f32, CaretMark)>,
) {
    let ruler = kit.ruler;
    let (cell, height) = (ruler.cell(), ruler.large());
    let room = ((eye_box(ruler).x - ruler.gap - line.x) / cell.max(1)).max(1) as usize - usize::from(caret.is_some());
    let count = text.chars().count();
    let skip_count = count.saturating_sub(room);
    let left = line.x + caret.map_or(0, |caret| caret.0);
    if let Some((_, column, mark)) = caret {
        let at = (column - skip_count as f32).clamp(0.0, room as f32);
        mark.draw(pen, kit, Rect::new(left + round(at * cell as f32), line.y, cell, height));
    }
    if is_shown {
        let start = text.char_indices().nth(skip_count).map_or(text.len(), |entry| entry.0);
        pen.text(None, &ruler.heading, (left, line.y), &text[start..], colour, 1);
    } else {
        let dots = (count - skip_count).min(DOTS.len() / DOT.len_utf8());
        pen.text(None, &ruler.heading, (left, line.y), &DOTS[..dots * DOT.len_utf8()], colour, 1);
    }
}
