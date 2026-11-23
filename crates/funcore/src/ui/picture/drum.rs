use crate::base::math::round;
use crate::domain::gallery::Thumbnail;
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::gfx::measures::{PHI, PHI2, PHI3};
use crate::gfx::pen::Pen;
use crate::gfx::store::{Placement, Placements, Spot, Store};
use crate::ui::grid::Grid;
use crate::ui::kit::Kit;
use crate::ui::ruler::Ruler;

const RINGS: usize = 2;
pub(in crate::ui) const REACH: usize = RINGS + 1;
const DRUM: u64 = 0;

struct Around(usize);

impl Placements for Around {
    fn placement(&self, spot: Spot) -> Placement {
        if spot.sheet == DRUM && spot.item.abs_diff(self.0) <= REACH { Placement::Shows(0) } else { Placement::Gone }
    }

    fn landings(&self) -> u32 {
        0
    }
}

const fn on_drum(item: usize) -> Spot {
    Spot { sheet: DRUM, item }
}

pub(in crate::ui) fn band(ruler: &Ruler) -> Rect {
    let room = ruler.under_bar();
    let height = round(room.h as f32 / PHI);
    Rect::new(room.x, room.y + (room.h - height) / 2, room.w, height)
}

fn ring(ruler: &Ruler, offset: f32) -> Option<Rect> {
    let distance = offset.abs();
    if distance >= REACH as f32 {
        return None;
    }
    let (band, gap) = (band(ruler), ruler.gap as f32);
    let (band_width, band_height) = (band.w as f32, band.h as f32);
    let side = (ruler.under_bar().h as f32 - band_height) / 2.0;
    let first = (side - 2.0 * gap) / PHI;
    let heights = [band_height, first, first / PHI, 0.0];
    let middles = [
        0.0,
        band_height / 2.0 + gap + heights[1] / 2.0,
        band_height / 2.0 + 2.0 * gap + heights[1] + heights[2] / 2.0,
        band_height / 2.0 + side,
    ];
    let (ring, share) = (distance as usize, distance - (distance as usize) as f32);
    let mix = |values: [f32; REACH + 1]| values[ring] + (values[ring + 1] - values[ring]) * share;
    let (middle, height) = (mix(middles), mix(heights));
    let width = band_width - 2.0 * distance * band_width / (PHI3 * PHI2);
    let top = band.y as f32 + band_height / 2.0 + offset.signum() * middle - height / 2.0;
    let left = band.x as f32 + (band_width - width) / 2.0;
    let (x, y) = (round(left), round(top));
    let area = Rect::new(x, y, round(left + width) - x, round(top + height) - y);
    (area.w > 0 && area.h > 0).then_some(area)
}

pub(in crate::ui) struct Drum {
    store: Store<Thumbnail>,
}

impl Drum {
    pub(in crate::ui) fn new() -> Drum {
        Drum { store: Store::with_cuts() }
    }

    pub(in crate::ui) fn thumbnail(&self, item: usize) -> Option<(&Thumbnail, u64)> {
        self.store.get(on_drum(item))
    }

    pub(in crate::ui) fn take(&mut self, ids: &Ids, selection: usize, item: usize, thumbnail: Thumbnail) -> bool {
        self.store.put(ids, &Around(selection), (on_drum(item), None), thumbnail, false)
    }

    pub(in crate::ui) fn trim(&mut self, selection: usize) {
        self.store.prune(&Around(selection));
    }

    pub(in crate::ui) fn clear(&mut self) {
        self.store.clear();
    }

    pub(in crate::ui) fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        grid: &Grid,
        at: f32,
        mut draw_over: impl FnMut(&mut Pen<'_, '_>, Rect),
    ) {
        let Some(count) = grid.items else {
            return;
        };
        let (selection, room) = (grid.selection, kit.ruler.room());
        let first = (at.max(0.0) as usize).saturating_sub(REACH);
        let mut near: [Option<usize>; 2 * REACH + 1] = [None; 2 * REACH + 1];
        for (slot, item) in (first..count).take(near.len()).enumerate() {
            near[slot] = Some(item);
        }
        near.sort_by(|a, b| {
            let distance = |item: &Option<usize>| item.map_or(f32::MAX, |item| (item as f32 - at).abs());
            distance(b).total_cmp(&distance(a))
        });
        let is_resting = at == selection as f32;
        for item in near.into_iter().flatten() {
            let is_marked = is_resting && item == selection;
            let area = if is_marked { grid.selection_area() } else { ring(kit.ruler, item as f32 - at) };
            let (Some(area), Some((thumbnail, id))) = (area, self.thumbnail(item)) else {
                continue;
            };
            let (picture, Some(within)) = (&thumbnail.picture, area.clip(room)) else {
                continue;
            };
            let height = picture.height() as i32;
            if height == 0 {
                continue;
            }
            let (width, height) = (picture.width as f32, height as f32);
            let scale = (area.w as f32 / width).max(area.h as f32 / height);
            let (scaled_width, scaled_height) = (round(width * scale), round(height * scale));
            let cover = area.centre(scaled_width, scaled_height);
            if (scaled_width, scaled_height) == (picture.width as i32, height as i32) {
                pen.blit(within, cover.x, cover.y, picture.width, &picture.words, id);
            } else {
                pen.stretch(cover, within, picture.width, &picture.words, id);
            }
            if is_marked {
                draw_over(pen, area);
            }
        }
    }
}
