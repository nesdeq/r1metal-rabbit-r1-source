use super::{CHEVRON, Item, SAMPLE_TIME, paint_sample, paint_under, split_value};
use crate::gfx::palette::Roles;
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::field::{paint_field, paint_lines};
use crate::ui::gauge::BlockRow;
use crate::ui::kit::{Kit, paint_card};
use crate::ui::words::{HOLD_TO_TALK, write_palette_name};
use alloc::string::String;

pub(super) fn paint_hero(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, side: i32) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (width, height) = (plate.width(), plate.height());
    paint_card(plate, &roles);
    let inset = ruler.card_inset();
    let left = inset + side + ruler.gap;
    let (large, small) = (ruler.large(), ruler.small());
    let room = width - left - inset;
    let mut y = inset;
    paint_field(kit, plate, &ruler.heading, (left, y), &item.name, room, roles.text);
    y += large;
    paint_field(kit, plate, &ruler.body, (left, y), &item.word, room, roles.text_dim);
    y += small + ruler.gap;
    let max_lines = ((height - y - inset) / small.max(1)).max(0);
    paint_lines(kit, plate, (left, y), (room, max_lines * small), &item.text, roles.content);
}

pub(super) fn paint_tile(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, has_chevron: bool) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let surface = paint_card(plate, &roles);
    let (inset, width, height) = (ruler.card_inset(), plate.width(), plate.height());
    let (large, small, side) = (ruler.large(), ruler.small(), ruler.line_icon_side());
    let foot = height - inset - large - small;
    let ink = item.ink(&roles);
    if let Some(icon) = item.icon {
        let icon_side = ruler.card_icon(foot - inset, large).max(side);
        kit.paint_icon(plate, icon, (inset, foot - icon_side), icon_side, (ink, surface));
    }
    let mut room = width - 2 * inset;
    if has_chevron && item.has_lead {
        let at = (width - inset - side, foot + (large + small - side) / 2);
        kit.paint_icon(plate, CHEVRON, at, side, (roles.text_dim, surface));
        room -= side + ruler.gap;
    }
    paint_field(kit, plate, &ruler.heading, (inset, foot), &item.name, room, ink);
    paint_under(kit, plate, item, (inset, foot + large), room, roles.text_dim);
}

pub(super) fn paint_palette_card(kit: &Kit<'_>, plate: &mut Surface<'_>, at: usize) {
    let (ruler, roles, sample) = (kit.ruler, kit.roles, Roles::at(at));
    let (width, height) = (plate.width(), plate.height());
    paint_card(plate, &roles);
    let (gap, inset, large, small) = (ruler.gap, ruler.framed_inset(), ruler.large(), ruler.small());
    let (block, low, block_gap) = ruler.block();
    let blocks = height - gap - low;
    let name_top = blocks - block_gap - small;
    let panel = Rect::new(gap, gap, width - 2 * gap, name_top - gap / 2 - gap);
    plate.fill(panel, sample.background);
    let x = panel.x + gap;
    let y = panel.y + (panel.h - large - 2 * small) / 2;
    let (whole, plain) = (plate.bounds(), Lettering::default());
    plate.text(whole, &ruler.heading, (x, y), SAMPLE_TIME.trim_end(), sample.text, 1, plain);
    plate.text(whole, &ruler.body, (x, y + large), HOLD_TO_TALK, sample.text_dim, 1, plain);
    paint_sample(kit, plate, &sample, (x, y + large + small), panel.right() - gap - x, false);
    let mut name = String::new();
    write_palette_name(at, &mut name);
    paint_field(kit, plate, &ruler.body, (inset, name_top), &name, width - 2 * inset, roles.text);
    let row = BlockRow { first: Rect::new(inset, blocks, block, low), step: block + block_gap };
    row.fill(plate, sample.swatches().map(Some));
}

pub(super) fn paint_figure_tile(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let ground = paint_card(plate, &roles);
    let (inset, side, small, large) = (ruler.card_inset(), ruler.line_icon_side(), ruler.small(), ruler.large());
    let (width, height) = (plate.width(), plate.height());
    let (top, mut room) = (height - inset - small - large - small, width - 2 * inset);
    if item.has_lead {
        let at = (width - inset - side, top + small + (large - side) / 2);
        kit.paint_icon(plate, CHEVRON, at, side, (roles.text_dim, ground));
        room -= side + ruler.gap;
    }
    paint_field(kit, plate, &ruler.body, (inset, top), &item.name, room, roles.text_dim);
    let (head, tail) = split_value(&item.word);
    paint_field(kit, plate, &ruler.heading, (inset, top + small), head, room, item.ink(&roles));
    if !tail.is_empty() {
        paint_field(kit, plate, &ruler.body, (inset, top + small + large), tail, room, roles.text_dim);
    }
}
