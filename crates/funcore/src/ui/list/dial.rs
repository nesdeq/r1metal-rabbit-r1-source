use super::LinePicture;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::Rect;
use crate::gfx::motion::Edge;
use crate::gfx::palette::{self, Roles};
use crate::gfx::pen::Pen;
use crate::gfx::store::{Placement, SOLE, SoleSpot, Store};
use crate::ui::field::{draw_field, draw_tickers, paint_with_tickers};
use crate::ui::item::{FIGURE_SCALE, Item, paint_band};
use crate::ui::kit::{IconPlace, Kit, draw_rule, find_glyph, mix_colour};
use crate::ui::ruler::Ruler;
use alloc::borrow::Cow;

const REACH: usize = 3;

pub(in crate::ui) fn figure_band(ruler: &Ruler) -> i32 {
    FIGURE_SCALE * ruler.large() + 2 * ruler.small() + 2 * ruler.gap_phi()
}

pub(in crate::ui) fn line_band(ruler: &Ruler) -> i32 {
    round(ruler.visible()) + 2 * ruler.gap_phi2()
}

fn offset_of(ruler: &Ruler, band: i32, distance: f32) -> f32 {
    let (half, step) = (band as f32 / 2.0, ruler.line_icon_side() as f32);
    let offset = (half + step / 2.0) * distance.abs().min(1.0) + step * (distance.abs() - 1.0).max(0.0);
    offset * distance.signum()
}

fn colour_at(roles: &Roles, distance: f32) -> Color {
    let reach = distance.abs();
    let (start, end, share) = if reach < 2.0 {
        (roles.text_dim, roles.rule, (reach - 1.0).max(0.0))
    } else {
        (roles.rule, roles.surface(), (reach - 2.0).min(1.0))
    };
    mix_colour(start, end, share)
}

fn draw_roll_line(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    item: &Item,
    (distance, centre): (f32, i32),
    (area, step): (Rect, i32),
) {
    if distance.abs() > REACH as f32 + 0.5 || centre - step / 2 < area.y || centre + step / 2 > area.bottom() {
        return;
    }
    let colour = colour_at(&kit.roles, distance);
    let (ruler, gap) = (kit.ruler, kit.ruler.gap);
    let mut left = gap + ruler.framed_inset();
    let (height, cell) = (ruler.large(), ruler.cell());
    if let Some(icon) = item.icon {
        if let Some(glyph) = find_glyph(kit.glyphs, icon) {
            let place = IconPlace::at((left, centre - height / 2), height);
            kit.draw_icon(pen, glyph, place, (colour, kit.roles.background));
        }
        left += height + gap;
    }
    let (right, y) = (ruler.width - gap - ruler.framed_inset(), centre - height / 2);
    let mut x = left;
    if let Some(letter) = item.palette.map(palette::letter) {
        x = pen.text(None, &ruler.heading, (x, y), letter.encode_utf8(&mut [0u8; 4]), colour, 1) + cell;
    }
    draw_field(kit, pen, &ruler.heading, (x, y), &item.name, right - x, colour);
}

pub(in crate::ui) struct Dial<L> {
    pub(in crate::ui) roll: Edge,
    band: Store<LinePicture>,
    band_line: Option<(L, usize)>,
}

impl<L> Default for Dial<L> {
    fn default() -> Self {
        Dial { roll: Edge::default(), band: Store::default(), band_line: None }
    }
}

impl<L: Copy + PartialEq> Dial<L> {
    pub(in crate::ui) fn clear_band(&mut self) {
        self.band.clear();
        self.band_line = None;
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64) {
        self.band.advance(now_ns, &SoleSpot(0));
    }

    pub(in crate::ui) const fn is_resting(&self) -> bool {
        self.band.is_resting()
    }

    pub(in crate::ui) fn band_item(&self) -> usize {
        round(self.roll.at).max(0) as usize
    }

    pub(in crate::ui) fn is_in_band(&self, place: (L, usize)) -> bool {
        self.band_line == Some(place)
    }

    pub(in crate::ui) fn take_band(
        &mut self,
        kit: &Kit<'_>,
        item: &Item,
        (place, size): ((L, usize), (usize, usize)),
        key: u64,
        is_still: bool,
    ) {
        let is_new = !self.is_in_band(place);
        if !self.band.holds(SOLE, key) {
            let line = self.paint_selection(kit, item, size);
            self.band.put(kit.ids, &SoleSpot(0), (SOLE, Some(key)), line, is_new || is_still);
        }
        self.band_line = Some(place);
    }

    fn paint_selection(&mut self, kit: &Kit<'_>, item: &Item, size: (usize, usize)) -> LinePicture {
        let LinePicture { picture, tickers: slots, .. } = self.band.spare(SOLE).unwrap_or_default();
        let (picture, tickers) =
            paint_with_tickers(kit, (picture.words, slots), size, |plate| paint_band(kit, plate, item));
        LinePicture { picture, tickers, reversed: None }
    }

    pub(in crate::ui) fn draw<'a>(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        (list, count, band): (L, usize, i32),
        area: Rect,
        item_at: impl Fn(usize) -> Option<Cow<'a, Item>>,
        draw_over: impl Fn(&mut Pen<'_, '_>, usize, (i32, i32)),
    ) {
        let (ruler, step, gap) = (kit.ruler, kit.ruler.line_icon_side(), kit.ruler.gap);
        let middle = area.y + area.h / 2;
        let (top, bottom) = (middle - band / 2, middle + band / 2);
        let at = self.roll.at;
        let first = (at - REACH as f32 - 1.0).max(0.0) as usize;
        let last = ((at + REACH as f32 + 1.0) as usize).min(count.saturating_sub(1));
        let width = ruler.width - 2 * gap;
        for index in (first..=last).take_while(|index| *index < count) {
            let distance = index as f32 - at;
            let centre = middle + round(offset_of(ruler, band, distance));
            if distance.abs() < 0.5 {
                if self.is_in_band((list, index)) {
                    let place = Rect::new(gap, centre - band / 2, width, band);
                    let spot = (SOLE, Placement::Shows(0));
                    self.band.draw(pen, spot, (place, area), |pen, line, at, within| {
                        draw_tickers(kit, pen, &line.tickers, at, within);
                        draw_over(pen, index, at);
                    });
                }
                continue;
            }
            if let Some(item) = item_at(index) {
                draw_roll_line(pen, kit, &item, (distance, centre), (area, step));
            }
        }
        draw_rule(pen, kit, top);
        draw_rule(pen, kit, bottom);
    }
}
