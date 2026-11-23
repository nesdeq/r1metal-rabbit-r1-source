use super::{CHEVRON, DISC, Item, Role, RowStyle, blocks_at, paint_blocks, paint_sample, paint_under};
use crate::base::math::round;
use crate::gfx::measures::PHI;
use crate::gfx::{Lettering, Surface};
use crate::ui::field::{cells, cut, paint_field};
use crate::ui::gauge::{BlockRow, Sweep};
use crate::ui::kit::{Kit, paint_card};
use crate::ui::robot::DRAWING;
use crate::ui::words::write_palette_name;
use alloc::string::String;

pub(in crate::ui) fn line_blocks(kit: &Kit<'_>, (width, height): (usize, usize), inset: i32) -> BlockRow {
    blocks_at(kit, width as i32 - inset, height as i32 / 2)
}

fn row_top(kit: &Kit<'_>, height: i32) -> i32 {
    (height - kit.ruler.large() - kit.ruler.small()) / 2
}

// The pacing sweep of a row without icon, lead or blocks: after its word, on the word's line.
pub(in crate::ui) fn row_sweep(kit: &Kit<'_>, word: &str, (width, height): (usize, usize)) -> Sweep {
    let ruler = kit.ruler;
    let (inset, small, low) = (ruler.framed_inset(), ruler.small(), ruler.block().1);
    Sweep {
        start: inset + ruler.body.text_width(word) + ruler.gap,
        end: width as i32 - inset,
        top: row_top(kit, height as i32) + ruler.large() + (small - low) / 2,
    }
}

fn to_row_name(item: &Item) -> String {
    let Some(at) = item.palette else {
        return item.name.clone();
    };
    let mut name = String::new();
    write_palette_name(at, &mut name);
    name
}

pub(super) fn paint_icon_line(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item) {
    let (side, roles) = (kit.ruler.line_icon_side(), kit.roles);
    plate.fill(plate.bounds(), roles.background);
    if let Some(icon) = item.icon {
        let at = ((plate.width() - side) / 2, (plate.height() - side) / 2);
        kit.paint_icon(plate, icon, at, side, (item.ink(&roles), roles.background));
    }
}

pub(super) fn paint_row(kit: &Kit<'_>, plate: &mut Surface<'_>, item: &Item, style: RowStyle) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    plate.fill(plate.bounds(), roles.background);
    let (width, height) = (plate.width(), plate.height());
    let ground = if style.is_carded { paint_card(plate, &roles) } else { roles.background };
    let inset = ruler.framed_inset();
    let (large, small, disc) = (ruler.large(), ruler.small(), ruler.small());
    let top = row_top(kit, height);
    let ink = item.ink(&roles);
    let mut left = inset;
    if item.is_recording {
        kit.paint_icon(plate, DISC, (left, top + (large - disc) / 2), disc, (roles.alert, ground));
        left += disc + ruler.gap;
    }
    let side = ruler.line_icon_side();
    if let Some(icon) = item.icon.filter(|_| style.has_icons) {
        let mark = if style.is_led && !item.is_faint { roles.mark } else { ink };
        kit.paint_icon(plate, icon, (left, (height - side) / 2), side, (mark, ground));
        left += side + ruler.gap;
    }
    let has_robot = item.palette.is_some() && style.is_led;
    if has_robot {
        let robot_side = round(side as f32 * PHI);
        kit.paint_icon(plate, DRAWING, (left, (height - robot_side) / 2), robot_side, (roles.robot, ground));
        left += robot_side + ruler.gap;
    }
    let mut right = width - inset;
    if item.has_lead && style.has_leads {
        kit.paint_icon(plate, CHEVRON, (right - side, (height - side) / 2), side, (roles.text_dim, ground));
    }
    if item.keeps_lead_room() && style.has_leads {
        right -= side + ruler.gap;
    }
    if !has_robot {
        right = paint_blocks(kit, plate, item, (right, height / 2), None);
    }
    let room = right - left;
    match item.role {
        Role::Figure | Role::Primary(_) => {
            paint_field(kit, plate, &ruler.body, (left, top), &item.name, room, roles.text_dim);
            paint_field(kit, plate, &ruler.heading, (left, top + small), &item.word, room, ink);
        }
        Role::Entry => {
            paint_field(kit, plate, &ruler.heading, (left, top), &item.name, room, ink);
            let text = [item.word.as_str(), item.text.as_str()].join(" ");
            let (under, plain) = (cut(text.trim(), cells(room, &ruler.body)), Lettering::default());
            plate.text(plate.bounds(), &ruler.body, (left, top + large), under, roles.text_dim, 1, plain);
        }
        Role::Plain | Role::Action => {
            let is_alone = item.word.is_empty() && item.text.is_empty() && item.palette.is_none();
            let top = if is_alone { (height - large) / 2 } else { top };
            paint_field(kit, plate, &ruler.heading, (left, top), &to_row_name(item), room, ink);
            if item.palette.is_some() {
                paint_sample(kit, plate, &roles, (left, top + large), room, !has_robot);
            } else {
                paint_under(kit, plate, item, (left, top + large), room, roles.text_dim);
            }
        }
    }
}
