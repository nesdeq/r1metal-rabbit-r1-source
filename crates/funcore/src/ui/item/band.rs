use super::{Aside, CHEVRON, DISC, FIGURE_SCALE, Item, Role, SAMPLE_TIME, paint_sample, paint_under, split_value};
use crate::base::math::round;
use crate::gfx::palette::{self, Roles};
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::field::{paint_field, paint_lines, paint_scaled};
use crate::ui::gauge::BlockRow;
use crate::ui::kit::Kit;
use crate::ui::words::HOLD_TO_TALK;

fn band_icon(kit: &Kit<'_>) -> i32 {
    round(kit.ruler.visible())
}

fn after_icon(kit: &Kit<'_>, left: i32) -> i32 {
    left + band_icon(kit) + 2 * kit.ruler.gap
}

fn figure_top(kit: &Kit<'_>, height: i32) -> i32 {
    (height - 2 * kit.ruler.small() - FIGURE_SCALE * kit.ruler.large()) / 2
}

fn tail_top(kit: &Kit<'_>, height: i32) -> i32 {
    figure_top(kit, height) + kit.ruler.small() + FIGURE_SCALE * kit.ruler.large()
}

// The blocks of a figure with an icon: on its tail's line, from where its text starts.
pub(in crate::ui) fn band_blocks(kit: &Kit<'_>, height: i32) -> BlockRow {
    let ruler = kit.ruler;
    let (block, low, gap) = ruler.block();
    let top = tail_top(kit, height) + (ruler.small() - low) / 2;
    BlockRow { first: Rect::new(after_icon(kit, ruler.framed_inset()), top, block, low), step: block + gap }
}

fn paint_entry(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    item: &Item,
    (left, room): (i32, i32),
    height: i32,
    roles: &Roles,
) {
    let (large, small) = (kit.ruler.large(), kit.ruler.small());
    let top = (height - large - 2 * small) / 2;
    paint_field(kit, plate, &kit.ruler.heading, (left, top), &item.name, room, roles.text);
    let text = [item.word.as_str(), item.text.as_str()].join(" ");
    paint_lines(kit, plate, (left, top + large), (room, 2 * small), text.trim(), roles.content);
}

fn paint_figure(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    item: &Item,
    (left, room): (i32, i32),
    height: i32,
    roles: &Roles,
) {
    let (head, tail) = split_value(&item.word);
    let tail = if item.text.is_empty() { tail } else { item.text.as_str() };
    let (top, small) = (figure_top(kit, height), kit.ruler.small());
    paint_field(kit, plate, &kit.ruler.body, (left, top), &item.name, room, roles.text_dim);
    paint_scaled(kit, plate, FIGURE_SCALE, (left, top + small), head, room, roles.text);
    paint_field(kit, plate, &kit.ruler.body, (left, tail_top(kit, height)), tail, room, roles.text_dim);
}

fn paint_steps_back(kit: &Kit<'_>, plate: &mut Surface<'_>, roles: &Roles, (x, y): (i32, i32)) {
    let (whole, body, plain) = (plate.bounds(), &kit.ruler.body, Lettering::default());
    let end = plate.text(whole, body, (x, y), SAMPLE_TIME, roles.text, 1, plain);
    plate.text(whole, body, (end, y), HOLD_TO_TALK, roles.text_dim, 1, plain);
}

fn paint_plain(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    item: &Item,
    (left, right): (i32, i32),
    height: i32,
    roles: &Roles,
) {
    let (large, small) = (kit.ruler.large(), kit.ruler.small());
    let under = match item.palette {
        Some(_) => 2 * small,
        None if item.word.is_empty() && item.text.is_empty() => 0,
        None => small,
    };
    let top = (height - large - under) / 2;
    let font = &kit.ruler.heading;
    let mut x = left;
    if let Some(letter) = item.palette.map(palette::letter) {
        let (whole, plain) = (plate.bounds(), Lettering::default());
        let end = plate.text(whole, font, (x, top), letter.encode_utf8(&mut [0u8; 4]), roles.text, 1, plain);
        x = end + font.width as i32;
    }
    paint_field(kit, plate, font, (x, top), &item.name, right - x, roles.text);
    if item.palette.is_some() {
        paint_steps_back(kit, plate, roles, (left, top + large));
        paint_sample(kit, plate, roles, (left, top + large + small), right - left, false);
    } else {
        paint_under(kit, plate, item, (left, top + large), right - left, roles.text_dim);
    }
}

pub(in crate::ui) fn paint_band(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (width, height) = (plate.width(), plate.height());
    plate.fill(plate.bounds(), roles.background);
    let (gap, inset, large, small) = (ruler.gap, ruler.framed_inset(), ruler.large(), ruler.small());
    let mut left = inset;
    if item.is_recording {
        let disc = small;
        let top = (height - large - small) / 2 + (large - disc) / 2;
        kit.paint_icon(plate, DISC, (left, top), disc, (roles.alert, roles.background));
        left += disc + gap;
    }
    if let Some(icon) = item.icon {
        let side = band_icon(kit);
        kit.paint_icon(plate, icon, (left, (height - side) / 2), side, (roles.text, roles.background));
        left = after_icon(kit, left);
    }
    let mut right = width - inset;
    if item.has_lead {
        let chevron = ruler.line_icon_side();
        kit.paint_icon(
            plate,
            CHEVRON,
            (right - chevron, (height - chevron) / 2),
            chevron,
            (roles.text, roles.background),
        );
        right -= chevron + gap;
    }
    if let Aside::Swatches(at) = item.aside {
        let (block, low, block_gap) = ruler.block();
        let swatches = Roles::at(at).swatches();
        let count = swatches.len() as i32;
        let first = Rect::new(right - count * (block + block_gap) + block_gap, (height - low) / 2, block, low);
        BlockRow { first, step: block + block_gap }.fill(plate, swatches.map(Some));
        right -= count * (block + block_gap) + gap;
    }
    match item.role {
        Role::Entry => paint_entry(kit, plate, item, (left, right - left), height, &roles),
        Role::Figure | Role::Primary(_) => paint_figure(kit, plate, item, (left, right - left), height, &roles),
        Role::Plain | Role::Action => paint_plain(kit, plate, item, (left, right), height, &roles),
    }
}
