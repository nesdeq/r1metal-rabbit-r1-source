use crate::domain::gallery::{Kind, Stamp as StampKind, Thumbnail};
use crate::gfx::pen::Pen;
use crate::gfx::stamp::Stamp;
use crate::gfx::store::{Placement, Store};
use crate::gfx::{Picture, Rect};
use crate::ui::grid::Grid;
use crate::ui::kit::Kit;

pub(in crate::ui) fn picture_area(area: Rect, picture: &Picture) -> Rect {
    area.centre(picture.width as i32, picture.height() as i32).clip(area).unwrap_or(area)
}

pub(in crate::ui) struct Stamps {
    trash: Stamp,
    redraw: Stamp,
    send: Stamp,
    pub(in crate::ui) armed: Option<(usize, StampKind)>,
}

impl Stamps {
    pub(in crate::ui) fn new(stamp: impl Fn(&str) -> Stamp) -> Stamps {
        Stamps {
            trash: stamp(icon!("delete")),
            redraw: stamp(icon!("frame_reload")),
            send: stamp(icon!("send")),
            armed: None,
        }
    }

    const fn stamp(&self, kind: StampKind) -> &Stamp {
        match kind {
            StampKind::Trash => &self.trash,
            StampKind::Redraw => &self.redraw,
            StampKind::Send => &self.send,
        }
    }

    fn place(&self, gap: i32, area: Rect, kind: StampKind) -> (i32, i32) {
        let side = self.stamp(kind).side as i32;
        let top = area.bottom() - gap - side;
        match kind {
            StampKind::Trash => (area.right() - gap - side, top),
            StampKind::Redraw => (area.x + gap, top),
            StampKind::Send => (area.x + (area.w - side) / 2, top),
        }
    }

    pub(in crate::ui) fn touched(
        &self,
        gap: i32,
        kind: Kind,
        area: Rect,
        (start, at): ((i32, i32), (i32, i32)),
    ) -> Option<StampKind> {
        kind.stamps().iter().copied().find(|stamp| {
            let ((x, y), reach) = (self.place(gap, area, *stamp), self.stamp(*stamp).side as i32 + 2 * gap);
            let target = Rect::new(x - gap, y - gap, reach, reach);
            target.contains(start.0, start.1) && target.contains(at.0, at.1)
        })
    }

    pub(in crate::ui) fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        kind: Kind,
        (area, within): (Rect, Rect),
        item: Option<usize>,
    ) {
        for &stamp in kind.stamps() {
            let (x, y) = self.place(kit.ruler.gap, area, stamp);
            let is_armed = item.is_some_and(|item| self.armed == Some((item, stamp)));
            let colour = if is_armed { kit.roles.alert } else { kit.roles.text };
            pen.stamp(self.stamp(stamp), within, x, y, colour);
        }
    }

    pub(in crate::ui) fn draw_on_cells(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        kind: Kind,
        (grid, store): (&Grid, &Store<Thumbnail>),
    ) {
        for cell in grid.cells().filter(|cell| cell.has_item) {
            let small = store.get(cell.spot).filter(|thumbnail| thumbnail.0.picture.width > 0);
            let (Some((small, _)), Some(within)) = (small, store.visible_area(cell.spot, cell.placement, cell.area))
            else {
                continue;
            };
            let item = (cell.placement != Placement::Leaves).then_some(cell.spot.item);
            self.draw(pen, kit, kind, (picture_area(cell.area, &small.picture), within), item);
        }
    }
}
