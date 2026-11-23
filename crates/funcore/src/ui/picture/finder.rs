use crate::base::math::round;
use crate::gfx::icon::Icon;
use crate::gfx::key::Key;
use crate::gfx::motion::Edge;
use crate::gfx::pen::Pen;
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::gauge::BlockRow;
use crate::ui::kit::{Canvas, IconPlace, Kit, draw_frame, draw_plate};

pub(in crate::ui) fn draw_finder(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (picture, id): (&[u32], u64),
    area: Rect,
    shutter: f32,
) {
    let (ruler, ground) = (kit.ruler, kit.roles.background);
    let (width, height) = (area.w.max(0) as usize, area.h.max(0) as usize);
    if width > 0 && picture.len() == width * height {
        let rim = round(shutter.clamp(0.0, 1.0) * ruler.rim());
        let open = Rect::new(area.x + rim, area.y + rim, (area.w - 2 * rim).max(0), (area.h - 2 * rim).max(0));
        pen.fill(Rect::new(0, 0, ruler.width, open.y), ground);
        pen.fill(Rect::new(0, open.bottom(), ruler.width, ruler.height - open.bottom()), ground);
        pen.fill(Rect::new(0, open.y, open.x, open.h), ground);
        pen.fill(Rect::new(open.right(), open.y, ruler.width - open.right(), open.h), ground);
        pen.blit(open, area.x, area.y, width, picture, id);
    } else {
        pen.fill(pen.bounds(), ground);
    }
}

pub(in crate::ui) fn draw_picture(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, (picture, id): (&[u32], u64), area: Rect) {
    let (width, height) = (area.w.max(0) as usize, area.h.max(0) as usize);
    if width > 0 && picture.len() == width * height {
        pen.blit(area, area.x, area.y, width, picture, id);
    } else {
        draw_frame(pen, area, kit.ruler.lines().0, kit.roles.rule);
    }
}

fn paint_run_time(plate: &mut Surface<'_>, kit: &Kit<'_>, disc: &Icon, time: &str) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (cell, line, gap) = (ruler.cell(), ruler.large(), ruler.gap);
    plate.fill(plate.bounds(), roles.background);
    kit.draw_icon(plate, disc, IconPlace::at((gap, (line - cell) / 2), cell), (roles.alert, roles.background));
    plate.text(plate.bounds(), &ruler.heading, (2 * gap + cell, 0), time, roles.text, 1, Lettering::default());
}

pub(in crate::ui) fn draw_run_time(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    disc: &Icon,
    time: &str,
    (within, quarters): (Rect, u8),
) {
    let (ruler, roles, inset) = (kit.ruler, kit.roles, kit.ruler.gap);
    let (cell, line) = (ruler.cell(), ruler.large());
    let width = 3 * inset + cell + time.chars().count() as i32 * cell;
    let key = Key::default().bytes(time.as_bytes()).word(ruler.heading.id).number(inset);
    let key = key.colour(roles.background).colour(roles.alert).colour(roles.text);
    draw_plate(pen, (Canvas::RunTime, within), (inset, inset), (width, line), (key, quarters), |plate| {
        paint_run_time(plate, kit, disc, time);
    });
}

pub(in crate::ui) fn draw_note(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    note: &str,
    (area, quarters): (Rect, u8),
    slide: f32,
) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (inset, line) = (ruler.gap, ruler.small());
    let drop = round(slide.clamp(0.0, 1.0) * ruler.note_drop());
    let foot = area.view_size(quarters).1 - inset - line + drop;
    let key = Key::default().bytes(note.as_bytes()).word(ruler.body.id);
    let key = key.colour(roles.background).colour(roles.text);
    let size = (ruler.body.text_width(note), line);
    draw_plate(pen, (Canvas::Note, area), (inset, foot), size, (key, quarters), |plate| {
        plate.fill(plate.bounds(), roles.background);
        plate.text(plate.bounds(), &ruler.body, (0, 0), note, roles.text, 1, Lettering::default());
    });
}

pub(in crate::ui) fn draw_progress(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, row: Rect, scale: f32, blocks: &[Edge]) {
    let (width, height, gap) = kit.ruler.block();
    let row = BlockRow { first: Rect::new(row.x, row.y, width, height), step: width + gap };
    row.draw_lit(pen, blocks.iter().map(|block| block.at), (Some(kit.roles.rule), scale), kit.roles.text);
}
