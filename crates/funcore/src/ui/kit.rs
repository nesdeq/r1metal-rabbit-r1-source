use super::field::TickerClock;
use super::ruler::{RULE, Ruler};
use crate::base::math::round;
use crate::base::pixel::{Color, PixelFormat};
use crate::gfx::icon::{Icon, Point};
use crate::gfx::key::{Ids, Key};
use crate::gfx::mask::{Mask, SHADES};
use crate::gfx::palette::Roles;
use crate::gfx::pen::Pen;
use crate::gfx::target::Target;
use crate::gfx::{Picture, Rect, Surface};
use alloc::vec::Vec;
use core::cell::RefCell;

#[derive(Clone, Copy)]
pub(super) enum Canvas {
    Note,
    RunTime,
    Header,
    Large,
    Opening,
}

pub(super) fn mix_rect(start: Rect, end: Rect, share: f32) -> Rect {
    let mix = |start: i32, end: i32| round(start as f32 + (end - start) as f32 * share);
    let (left, top) = (mix(start.x, end.x), mix(start.y, end.y));
    Rect::new(left, top, mix(start.right(), end.right()) - left, mix(start.bottom(), end.bottom()) - top)
}

pub(super) fn mix_colour(start: Color, end: Color, share: f32) -> Color {
    let steps = (SHADES - 1) as u32;
    start.toward(end, round(share * steps as f32) as u32, steps)
}

pub(super) fn read_icon(svg: &str) -> Icon {
    Icon::from_svg(svg).expect("embedded icon is valid")
}

pub(super) fn trace(mask: &mut Mask, icon: &Icon, left: f32, top: f32, side: f32) {
    let to_screen = |point: &Point| (left + point.x * side, top + point.y * side);
    for contour in icon.contours() {
        mask.outline(contour.iter().map(to_screen));
    }
}

#[derive(Clone, Copy)]
pub(super) struct IconPlace {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) side: f32,
}

impl IconPlace {
    pub(super) fn at((x, y): (i32, i32), side: i32) -> IconPlace {
        IconPlace { left: x as f32, top: y as f32, side: side as f32 }
    }
}

pub(super) fn find_glyph<'a>(glyphs: &'a [(&'static str, Icon)], svg: &str) -> Option<&'a Icon> {
    glyphs.iter().find(|entry| core::ptr::eq(entry.0, svg) || entry.0 == svg).map(|entry| &entry.1)
}

pub(super) fn read_glyphs(files: impl IntoIterator<Item = &'static str>) -> Vec<(&'static str, Icon)> {
    let mut glyphs: Vec<(&'static str, Icon)> = Vec::new();
    for file in files {
        if find_glyph(&glyphs, file).is_none() {
            glyphs.push((file, read_icon(file)));
        }
    }
    glyphs
}

#[derive(Clone, Copy)]
pub(super) struct Kit<'a> {
    pub(super) ruler: &'a Ruler,
    pub(super) roles: Roles,
    pub(super) glyphs: &'a [(&'static str, Icon)],
    pub(super) mask: &'a RefCell<Mask>,
    pub(super) ticking: &'a TickerClock,
    pub(super) format: PixelFormat,
    pub(super) ids: &'a Ids,
}

impl Kit<'_> {
    pub(super) const fn with_roles(self, roles: Roles) -> Self {
        Kit { roles, ..self }
    }

    pub(super) fn draw_icon(&self, target: &mut impl Target, icon: &Icon, place: IconPlace, colours: (Color, Color)) {
        target.cover(self.mask, colours, |mask| trace(mask, icon, place.left, place.top, place.side));
    }

    pub(super) fn paint_icon(
        &self,
        plate: &mut Surface<'_>,
        svg: &str,
        at: (i32, i32),
        side: i32,
        colours: (Color, Color),
    ) {
        if let Some(icon) = find_glyph(self.glyphs, svg) {
            self.draw_icon(plate, icon, IconPlace::at(at, side), colours);
        }
    }
}

pub(super) fn draw_plate(
    pen: &mut Pen<'_, '_>,
    (canvas, within): (Canvas, Rect),
    at: (i32, i32),
    (width, height): (i32, i32),
    (key, quarters): (Key, u8),
    paint: impl FnOnce(&mut Surface<'_>),
) {
    if width <= 0 || height <= 0 {
        return;
    }
    let (size, id) = ((width as usize, height as usize), key.word(u64::from(quarters)).value());
    pen.plate(canvas as usize, within, at, size, quarters, id, paint);
}

pub(super) fn paint_picture(
    mut words: Vec<u32>,
    format: PixelFormat,
    (width, height): (usize, usize),
    paint: impl FnOnce(&mut Surface<'_>),
) -> Picture {
    words.clear();
    words.resize(width * height, 0);
    if let Some(mut plate) = Surface::new(&mut words, width as u32, height as u32, width, format) {
        paint(&mut plate);
    }
    Picture { words, width }
}

pub(super) fn draw_card(target: &mut impl Target, area: Rect, colour: Color) {
    let strips = [Rect::new(area.x + 1, area.y, area.w - 2, area.h), Rect::new(area.x, area.y + 1, area.w, area.h - 2)];
    strips.into_iter().for_each(|strip| target.fill(strip, colour));
}

pub(super) fn paint_card(plate: &mut Surface<'_>, roles: &Roles) -> Color {
    let bounds = plate.bounds();
    plate.fill(bounds, roles.background);
    draw_card(plate, bounds, roles.surface());
    roles.surface()
}

const fn frame_edges(area: Rect, width: i32) -> [Rect; 4] {
    [
        Rect::new(area.x, area.y, area.w, width),
        Rect::new(area.x, area.bottom() - width, area.w, width),
        Rect::new(area.x, area.y, width, area.h),
        Rect::new(area.right() - width, area.y, width, area.h),
    ]
}

pub(super) fn draw_frame(target: &mut impl Target, area: Rect, width: i32, colour: Color) {
    frame_edges(area, width).into_iter().for_each(|strip| target.fill(strip, colour));
}

pub(super) fn draw_rule(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, y: i32) {
    pen.fill(Rect::new(0, y, kit.ruler.width, RULE), kit.roles.rule);
}

pub(super) fn draw_upright_rule(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, x: i32) {
    pen.fill(Rect::new(x, 0, RULE, kit.ruler.height), kit.roles.rule);
}
