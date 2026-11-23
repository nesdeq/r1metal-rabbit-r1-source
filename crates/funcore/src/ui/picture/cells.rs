use crate::gfx::Rect;
use crate::gfx::pen::Pen;
use crate::gfx::store::{AsPicture, Store};
use crate::ui::field::draw_field;
use crate::ui::grid::Grid;
use crate::ui::kit::{Kit, draw_frame};
use crate::ui::list::{Mark, draw_more};

fn draw_items<T: AsPicture>(
    pen: &mut Pen<'_, '_>,
    grid: &Grid,
    store: &Store<T>,
    mut draw_over: impl FnMut(&mut Pen<'_, '_>, &T, (i32, i32), Rect),
) {
    for cell in grid.cells().filter(|cell| cell.has_item) {
        store.draw(pen, (cell.spot, cell.placement), (cell.area, cell.area), &mut draw_over);
    }
}

pub(in crate::ui) fn draw_cells<T: AsPicture>(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (grid, store): (&Grid, &Store<T>),
    (has_arrow, mark): (bool, Mark),
    draw_over: impl FnMut(&mut Pen<'_, '_>, &T, (i32, i32), Rect),
) {
    draw_more(pen, kit, grid, has_arrow);
    draw_items(pen, grid, store, draw_over);
    if let (Some(frame), Mark::Frame(width)) = (grid.frame_area(), mark) {
        draw_frame(pen, frame, width, kit.roles.glow);
    }
}

pub(in crate::ui) fn draw_captioned<T: AsPicture>(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (grid, store): (&Grid, &Store<T>),
    has_arrow: bool,
    caption: &str,
) {
    draw_more(pen, kit, grid, has_arrow);
    draw_items(pen, grid, store, |_, _, _, _| {});
    let Some(frame) = grid.frame_area() else {
        return;
    };
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (small, gap) = (ruler.small(), ruler.gap);
    let band = Rect::new(frame.x, frame.y, frame.w, small + gap / 2);
    pen.fill(band, roles.text);
    let at = (band.x + gap / 2, band.y + gap / 4);
    draw_field(kit, pen, &ruler.body, at, caption, band.w - gap, roles.background);
}
