use super::{CHEVRON, Item, LARGE_SCALE, Role, paint_gauge, split_value};
use crate::base::math::round;
use crate::gfx::measures::PHI2;
use crate::gfx::{Lettering, Surface};
use crate::ui::field::{cells, cut, paint_field, paint_scaled, sentence_count, sentence_lines};
use crate::ui::kit::Kit;

const LARGE_GAUGE: i32 = 20;
const UNWRITTEN_SENTENCE: &str = "I have not written it down yet.";
const LONG: &str = " long";

fn large_gauge_side(kit: &Kit<'_>, width: i32) -> i32 {
    (width - 2 * kit.ruler.framed_inset() - (LARGE_GAUGE - 1) * kit.ruler.lines().0) / LARGE_GAUGE
}

pub(in crate::ui) fn large_height(kit: &Kit<'_>, item: &Item, width: i32) -> i32 {
    let ruler = kit.ruler;
    let gauge = if matches!(item.role, Role::Primary(Some(_))) { ruler.gap + large_gauge_side(kit, width) } else { 0 };
    LARGE_SCALE * ruler.large() + 2 * ruler.framed_inset() + gauge
}

pub(super) fn paint_primary_large(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, level: Option<u8>) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    plate.fill(plate.bounds(), roles.background);
    let (inset, gap, small, large, width) =
        (ruler.framed_inset(), ruler.gap, ruler.small(), ruler.large(), plate.width());
    let ((head, tail), room, height) = (split_value(&item.word), width - 2 * inset, LARGE_SCALE * large);
    let figure_end = paint_scaled(kit, plate, LARGE_SCALE, (inset, inset), head, room, roles.text);
    let text = if item.text.is_empty() { tail } else { item.text.as_str() };
    let left = figure_end + 2 * gap;
    let text_room = width - left - inset;
    if text_room > 0 {
        let (max_lines, room) = ((height / small.max(1)).max(1) as usize, cells(text_room, &ruler.body));
        let count = sentence_count(text, room);
        let (mut line, mut y) = (0, inset + (height - count.min(max_lines) as i32 * small) / 2);
        sentence_lines(text, room, |start, row| {
            if line < max_lines {
                let colour = if line == 0 { roles.text } else { roles.text_dim };
                if line + 1 == max_lines && count > max_lines {
                    paint_field(kit, plate, &ruler.body, (left, y), &text[start..], text_room, colour);
                } else {
                    plate.text(plate.bounds(), &ruler.body, (left, y), row, colour, 1, Lettering::default());
                }
                y += small;
            }
            line += 1;
        });
    }
    if let Some(level) = level {
        let side = large_gauge_side(kit, width);
        paint_gauge(kit, plate, (inset, inset + height + gap), (side, side), LARGE_GAUGE, level);
    }
}

pub(super) fn paint_columns(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    plate.fill(plate.bounds(), roles.background);
    let (inset, small, width, height) = (ruler.framed_inset(), ruler.small(), plate.width(), plate.height());
    let column = round(width as f32 / PHI2);
    let y = (height - small) / 2;
    let mut right = width - inset;
    if item.has_lead {
        let side = ruler.line_icon_side();
        kit.paint_icon(plate, CHEVRON, (right - side, (height - side) / 2), side, (roles.text_dim, roles.background));
        right -= side + ruler.gap;
    }
    paint_field(kit, plate, &ruler.body, (inset, y), &item.name, column - inset - ruler.gap, roles.text_dim);
    paint_field(kit, plate, &ruler.body, (column, y), &item.word, right - column, item.ink(&roles));
}

pub(in crate::ui) fn entry_marked_height(kit: &Kit<'_>, item: &Item) -> i32 {
    let ruler = kit.ruler;
    let length = if item.word.is_empty() { 0 } else { ruler.small() };
    ruler.large() + length + ruler.small() + 2 * ruler.framed_inset()
}

pub(super) fn paint_entry_marked(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    plate.fill(plate.bounds(), roles.background);
    let (inset, side, gap, width) = (ruler.framed_inset(), ruler.line_icon_side(), ruler.gap, plate.width());
    let (small, large, small_cell) = (ruler.small(), ruler.large(), ruler.small_cell());
    let top = inset + (plate.height() - entry_marked_height(kit, item)).max(0) / 2;
    if let Some(icon) = item.mark.or(item.icon) {
        kit.paint_icon(plate, icon, (inset, top), side, (roles.mark, roles.background));
    }
    let left = inset + side + gap;
    let room = width - left - inset;
    // The name's last word stands small beside the rest: an entry's time, an edition's year.
    let (head, tail) = item.name.rsplit_once(' ').unwrap_or((item.name.as_str(), ""));
    let tail_room = if tail.is_empty() { 0 } else { small_cell / 2 + ruler.body.text_width(tail) };
    let head_end = paint_field(kit, plate, &ruler.heading, (left, top), head, room - tail_room, roles.text);
    if !tail.is_empty() {
        let at = (head_end + small_cell / 2, top + (large - small));
        plate.text(plate.bounds(), &ruler.body, at, tail, roles.text_dim, 1, Lettering::default());
    }
    let mut y = top + large;
    if !item.word.is_empty() {
        let length = [item.word.as_str(), LONG].concat();
        paint_field(kit, plate, &ruler.body, (left, y), &length, room, roles.text_dim);
        y += small;
    }
    if item.text.is_empty() {
        paint_field(kit, plate, &ruler.body, (left, y), UNWRITTEN_SENTENCE, room, roles.text_dim);
    } else {
        let text = cut(&item.text, cells(room, &ruler.body));
        plate.text(plate.bounds(), &ruler.body, (left, y), text, roles.content, 1, Lettering::default());
    }
}
