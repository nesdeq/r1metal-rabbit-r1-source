use super::{CHEVRON, ENTRY_LINES, FIGURE_SCALE, Item, paint_gauge, split_value};
use crate::gfx::Surface;
use crate::ui::field::{paint_field, paint_lines, paint_scaled};
use crate::ui::kit::{Kit, paint_card};

const CARD_GAUGE: i32 = 10;
const UNWRITTEN: &str = "NOT WRITTEN DOWN YET";

pub(super) fn paint_primary_card(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, level: Option<u8>) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let ground = paint_card(plate, &roles);
    let (inset, gap, small, large) = (ruler.card_inset(), ruler.gap, ruler.small(), ruler.large());
    let (width, height) = (plate.width(), plate.height());
    let ((head, tail), figure, room) = (split_value(&item.word), FIGURE_SCALE * large, width - 2 * inset);
    let lines = [tail, item.text.as_str()];
    let count = lines.iter().filter(|line| !line.is_empty()).count() as i32;
    let foot = height - inset - count * small;
    let top = foot - if count > 0 { gap } else { 0 } - figure;
    let label = top - small;
    if let Some(icon) = item.icon {
        let side = ruler.card_icon(label - inset, figure);
        kit.paint_icon(plate, icon, (inset, label - side), side, (roles.text, ground));
    }
    paint_field(kit, plate, &ruler.body, (inset, label), &item.name, room, roles.text_dim);
    let mut figure_room = room;
    if let Some(level) = level {
        let (block, block_gap) = ruler.card_gauge_block();
        let gauge_width = CARD_GAUGE * block + (CARD_GAUGE - 1) * block_gap;
        paint_gauge(kit, plate, (width - inset - gauge_width, top), (block, figure), CARD_GAUGE, level);
        figure_room -= gauge_width + gap;
    }
    paint_scaled(kit, plate, FIGURE_SCALE, (inset, top), head, figure_room, roles.text);
    let mut y = foot;
    for line in lines.into_iter().filter(|line| !line.is_empty()) {
        paint_field(kit, plate, &ruler.body, (inset, y), line, room, roles.text_dim);
        y += small;
    }
}

fn entry_card_height(kit: &Kit<'_>, item: &Item) -> i32 {
    let ruler = kit.ruler;
    let length = if item.word.is_empty() { 0 } else { ruler.small() };
    ruler.large() + length + ENTRY_LINES as i32 * ruler.small() + 2 * ruler.card_inset()
}

pub(super) fn paint_entry_card(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let ground = paint_card(plate, &roles);
    let (inset, side, width, height) = (ruler.card_inset(), ruler.line_icon_side(), plate.width(), plate.height());
    let (small, large) = (ruler.small(), ruler.large());
    let (right, room) = (width - inset - side - ruler.gap, width - 2 * inset);
    let top = inset + (height - entry_card_height(kit, item)).max(0) / 2;
    paint_field(kit, plate, &ruler.heading, (inset, top), &item.name, room, roles.text);
    let mut y = top + large;
    let (lines, length) = (ENTRY_LINES as i32 * small, if item.word.is_empty() { 0 } else { small });
    if item.has_lead {
        let at = (width - inset - side, y + (length + lines - side) / 2);
        kit.paint_icon(plate, CHEVRON, at, side, (roles.text_dim, ground));
    }
    if !item.word.is_empty() {
        paint_field(kit, plate, &ruler.body, (inset, y), &item.word, right - inset, roles.mark);
        y += small;
    }
    if item.text.is_empty() {
        paint_field(kit, plate, &ruler.body, (inset, y), UNWRITTEN, right - inset, roles.text_dim);
    } else {
        paint_lines(kit, plate, (inset, y), (right - inset, lines), &item.text, roles.content);
    }
}
