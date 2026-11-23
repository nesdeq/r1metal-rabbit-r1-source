use super::font::Font;
use super::key::Key;
use super::mask::{Mask, Strip};
use super::region::Region;
use super::stamp::Stamp;
use super::{Lettering, Rect, Stretching, Surface};
use crate::base::pixel::{Color, PixelFormat};
use crate::base::sampling::{Turn, turn_words};
use alloc::vec::Vec;

const FILL: u64 = 1;
const BLIT: u64 = 2;
const STRETCH: u64 = 3;
const TEXT: u64 = 4;
const STAMP: u64 = 5;
const COVER: u64 = 6;
const OVER: u64 = 7;

/// A frame's outcome: new, by its number, or the one on screen again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    New(u64),
    Same(u64),
}

#[derive(Clone, Copy, Debug)]
struct Op {
    bounds: Rect,
    key: Key,
    strips: (usize, usize),
}

#[derive(Default)]
struct Canvas {
    words: Vec<u32>,
    unturned: Vec<u32>,
    content: Option<(u64, usize, usize)>,
}

#[derive(Default)]
pub(crate) struct DisplayList {
    mask: Mask,
    ops: Vec<Op>,
    last_ops: Vec<Op>,
    strips: Vec<Strip>,
    last_strips: Vec<Strip>,
    changes: Vec<(u64, Region)>,
    spare: Vec<Region>,
    within: Region,
    stamps: Region,
    direct: Region,
    shadow_parts: Region,
    shadow: Vec<u32>,
    serial: u64,
    lag: u64,
    size: Rect,
    stretching: Stretching,
    canvases: Vec<Canvas>,
}

impl DisplayList {
    pub(crate) fn new(width: usize, height: usize, points: usize) -> DisplayList {
        let mut mask = Mask::new(width, height);
        mask.reserve(points);
        DisplayList {
            mask,
            last_strips: Vec::with_capacity(width * height),
            shadow: alloc::vec![0; width * height],
            ..DisplayList::offscreen(width, height)
        }
    }

    pub(crate) fn offscreen(width: usize, height: usize) -> DisplayList {
        let size = Rect::new(0, 0, width as i32, height as i32);
        DisplayList {
            strips: Vec::with_capacity(width * height),
            within: Region::new(size),
            stamps: Region::new(size),
            direct: Region::new(size),
            shadow_parts: Region::new(size),
            size,
            ..DisplayList::default()
        }
    }

    pub(crate) fn swap_buffers(&mut self, other: &mut DisplayList) {
        core::mem::swap(&mut self.mask, &mut other.mask);
        core::mem::swap(&mut self.shadow, &mut other.shadow);
    }

    pub(crate) fn draw_still(&mut self, surface: &mut Surface<'_>, mut draw: impl FnMut(&mut Pen<'_, '_>)) {
        debug_assert!(surface.bounds() == self.size, "a list draws on pages of the size it was made for");
        debug_assert!(
            self.shadow.len() >= (surface.width().max(0) * surface.height().max(0)) as usize,
            "a still frame is drawn with a picture of the screen it holds"
        );
        self.ops.clear();
        self.last_ops.clear();
        self.strips.clear();
        self.stamps.clear();
        draw(&mut Pen { list: self, surface, is_drawing: false, next: 0, shift: 0 });
        self.within.clear();
        self.within.add_whole();
        self.split();
        self.draw_page(surface, &mut draw);
    }

    // The drawing pass, and what was drawn into the picture in cached memory, under stamps and outlines that mix, copied onto the page.
    fn draw_page(&mut self, surface: &mut Surface<'_>, draw: &mut impl FnMut(&mut Pen<'_, '_>)) {
        let (width, height) = (surface.width().max(0) as usize, surface.height().max(0) as usize);
        draw(&mut Pen { list: self, surface, is_drawing: true, next: 0, shift: 0 });
        for part in self.shadow_parts.parts(surface.bounds()) {
            surface.blit(part, 0, 0, width, &self.shadow[..width * height]);
        }
    }

    fn canvas_mut(&mut self, slot: usize) -> &mut Canvas {
        if self.canvases.len() <= slot {
            self.canvases.resize_with(slot + 1, Canvas::default);
        }
        &mut self.canvases[slot]
    }

    #[cfg(test)]
    fn drawn_within(&self) -> usize {
        self.within.pixels()
    }

    pub(crate) fn reserve_canvas(&mut self, slot: usize, words: usize, is_turned: bool) {
        let canvas = self.canvas_mut(slot);
        canvas.words.reserve(words.saturating_sub(canvas.words.len()));
        if is_turned {
            canvas.unturned.reserve(words.saturating_sub(canvas.unturned.len()));
        }
    }

    pub(crate) fn frame(
        &mut self,
        surface: &mut Surface<'_>,
        back_frame: Option<u64>,
        mut draw: impl FnMut(&mut Pen<'_, '_>),
    ) -> FrameKind {
        debug_assert!(surface.bounds() == self.size, "a list draws on pages of the size it was made for");
        self.begin();
        draw(&mut Pen { list: self, surface, is_drawing: false, next: 0, shift: 0 });
        let region = self.find_changes();
        let kind = if back_frame.is_none() || !region.is_empty() {
            self.note(region);
            FrameKind::New(self.serial)
        } else {
            self.spare.push(region);
            FrameKind::Same(self.serial)
        };
        // A page that shows the newest frame already has nothing to draw.
        if back_frame == Some(self.serial) {
            self.within.clear();
            return kind;
        }
        self.place(back_frame);
        if !self.direct.has_bands() && !self.shadow_parts.has_bands() {
            return kind;
        }
        self.draw_page(surface, &mut draw);
        kind
    }

    fn begin(&mut self) {
        core::mem::swap(&mut self.ops, &mut self.last_ops);
        core::mem::swap(&mut self.strips, &mut self.last_strips);
        self.ops.clear();
        self.strips.clear();
        self.stamps.clear();
    }

    fn find_changes(&mut self) -> Region {
        let mut region = self.spare.pop().unwrap_or_else(|| Region::new(self.size));
        region.clear();
        for at in 0..self.ops.len().max(self.last_ops.len()) {
            match (self.ops.get(at), self.last_ops.get(at)) {
                // Two things that draw the same pixels in the same place.
                (Some(now), Some(last)) if now.bounds == last.bounds && now.key == last.key => {}
                (now, last) => {
                    now.into_iter().chain(last).for_each(|op| region.add(op.bounds));
                }
            }
        }
        region
    }

    fn note(&mut self, region: Region) {
        self.serial += 1;
        self.changes.push((self.serial, region));
    }

    fn place(&mut self, back_frame: Option<u64>) {
        // The page drawn into shows the oldest frame of all pages: nothing needs what came before it.
        if let Some(back) = back_frame.filter(|back| *back < self.serial) {
            self.lag = self.lag.max(self.serial - back);
        }
        while self.changes.first().is_some_and(|(serial, _)| *serial + self.lag <= self.serial) {
            let (_, region) = self.changes.remove(0);
            self.spare.push(region);
        }
        self.within.clear();
        let known = back_frame
            .filter(|back| *back < self.serial && self.changes.first().is_some_and(|(serial, _)| *serial <= back + 1));
        match known {
            Some(back) => {
                for (_, region) in self.changes.iter().filter(|(serial, _)| *serial > back) {
                    self.within.add_region(region);
                }
            }
            // A page whose picture is not known is drawn whole.
            None => self.within.add_whole(),
        }
        self.split();
    }

    fn split(&mut self) {
        self.direct.set_outside(&self.within, &self.stamps);
        self.shadow_parts.set_inside(&self.within, &self.stamps);
        self.direct.find_bands();
        self.shadow_parts.find_bands();
    }
}

pub(crate) struct Pen<'p, 's> {
    list: &'p mut DisplayList,
    surface: &'p mut Surface<'s>,
    is_drawing: bool,
    next: usize,
    shift: i32,
}

impl Pen<'_, '_> {
    pub(crate) fn bounds(&self) -> Rect {
        self.surface.bounds()
    }

    pub(crate) const fn shift(&mut self, by: i32) {
        self.shift += by;
    }

    const fn to_shifted(&self, rect: Rect) -> Rect {
        Rect::new(rect.x, rect.y + self.shift, rect.w, rect.h)
    }

    fn to_visible(&self, drawn: Rect, within: Rect) -> Rect {
        self.surface.to_visible(drawn, within).unwrap_or_default()
    }

    fn draw(
        &mut self,
        bounds: Rect,
        to_key: impl FnOnce() -> Key,
        draw: impl FnMut(&mut Surface<'_>, Rect, &mut Stretching, &[Strip]),
    ) {
        if self.is_drawing {
            self.parts(draw);
        } else {
            self.list.ops.push(Op { bounds, key: to_key(), strips: (0, 0) });
        }
    }

    fn parts(&mut self, mut draw: impl FnMut(&mut Surface<'_>, Rect, &mut Stretching, &[Strip])) {
        let op = self.list.ops.get(self.next).copied();
        debug_assert!(op.is_some(), "a frame draws what it listed, in the same order");
        self.next += 1;
        let Some(Op { bounds, strips: (from, to), .. }) = op else {
            return;
        };
        let DisplayList { direct, shadow_parts, shadow, stretching, strips, .. } = &mut *self.list;
        let strips = &strips[from..to];
        for part in direct.parts(bounds) {
            draw(self.surface, part, stretching, strips);
        }
        if !shadow_parts.has_bands() {
            return;
        }
        let (width, height, format) =
            (self.surface.width() as u32, self.surface.height() as u32, self.surface.format());
        if let Some(mut picture) = Surface::new(shadow, width, height, width as usize, format) {
            for part in shadow_parts.parts(bounds) {
                draw(&mut picture, part, stretching, strips);
            }
        }
    }

    pub(crate) fn fill(&mut self, rect: Rect, colour: Color) {
        let bounds = self.to_visible(self.to_shifted(rect), self.bounds());
        let key = || Key::default().word(FILL).colour(colour);
        self.draw(bounds, key, |surface, part, _, _| surface.fill(part, colour));
    }

    pub(crate) fn blit(&mut self, within: Rect, x: i32, y: i32, width: usize, words: &[u32], id: u64) {
        let (within, y) = (self.to_shifted(within), y + self.shift);
        let height = words.len().checked_div(width).unwrap_or(0) as i32;
        let bounds = self.to_visible(Rect::new(x, y, width as i32, height), within);
        let key = || Key::default().word(BLIT).word(id).number(x).number(y).word(width as u64).word(words.len() as u64);
        self.draw(bounds, key, |surface, part, _, _| surface.blit(part, x, y, width, words));
    }

    pub(crate) fn stretch(&mut self, area: Rect, within: Rect, width: usize, words: &[u32], id: u64) {
        let (area, within) = (self.to_shifted(area), self.to_shifted(within));
        let key = || Key::default().word(STRETCH).word(id).rect(area).word(width as u64).word(words.len() as u64);
        let bounds = self.to_visible(area, within);
        self.draw(bounds, key, |surface, part, stretching, _| surface.stretch(stretching, area, part, width, words));
    }

    fn is_next_visible(&self) -> bool {
        let list = &*self.list;
        list.ops.get(self.next).is_some_and(|op| {
            list.direct.parts(op.bounds).next().is_some() || list.shadow_parts.parts(op.bounds).next().is_some()
        })
    }

    fn draw_canvas(
        &mut self,
        slot: usize,
        (bounds, to_key): (Rect, impl FnOnce() -> Key),
        content: (u64, usize, usize),
        paint: impl FnOnce(&mut Canvas, PixelFormat),
        mut draw: impl FnMut(&mut Surface<'_>, Rect, &mut Stretching, &[u32]),
    ) {
        if !self.is_drawing {
            return self.list.ops.push(Op { bounds, key: to_key(), strips: (0, 0) });
        }
        let mut canvas = core::mem::take(self.list.canvas_mut(slot));
        if self.is_next_visible() && canvas.content != Some(content) {
            paint(&mut canvas, self.surface.format());
            canvas.content = Some(content);
        }
        self.parts(|surface, part, stretching, _| draw(surface, part, stretching, &canvas.words));
        self.list.canvases[slot] = canvas;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plate(
        &mut self,
        slot: usize,
        within: Rect,
        (x, y): (i32, i32),
        (width, height): (usize, usize),
        quarters: u8,
        id: u64,
        paint: impl FnOnce(&mut Surface<'_>),
    ) {
        let at = within.place(quarters, x, y, width as i32, height as i32);
        let (left, top, across, down) = (at.x, at.y + self.shift, at.w.max(0) as usize, at.h.max(0) as usize);
        let bounds = self.to_visible(Rect::new(left, top, at.w, at.h), self.to_shifted(self.bounds()));
        let length = (across * down) as u64;
        let key = || Key::default().word(BLIT).word(id).number(left).number(top).word(across as u64).word(length);
        let paint_canvas = |canvas: &mut Canvas, format: PixelFormat| {
            let is_turned = !quarters.is_multiple_of(4);
            let words = if is_turned { &mut canvas.unturned } else { &mut canvas.words };
            words.clear();
            words.resize(width * height, 0);
            if let Some(mut plate) = Surface::new(words, width as u32, height as u32, width, format) {
                paint(&mut plate);
            }
            if is_turned {
                turn_words(&canvas.unturned, width, Turn { quarters, is_mirrored: false }, &mut canvas.words);
            }
        };
        self.draw_canvas(slot, (bounds, key), (id, across, down), paint_canvas, |surface, part, _, words| {
            surface.blit(part, left, top, across, words)
        });
    }

    pub(crate) fn stretch_canvas(
        &mut self,
        slot: usize,
        (area, within): (Rect, Rect),
        (width, height): (usize, usize),
        id: u64,
        paint: impl FnOnce(&mut Vec<u32>, PixelFormat),
    ) {
        let (area, within, length) = (self.to_shifted(area), self.to_shifted(within), width * height);
        let key = || Key::default().word(STRETCH).word(id).rect(area).word(width as u64).word(length as u64);
        let bounds = self.to_visible(area, within);
        let paint_canvas = |canvas: &mut Canvas, format: PixelFormat| {
            canvas.words.clear();
            canvas.words.resize(length, 0);
            paint(&mut canvas.words, format);
        };
        self.draw_canvas(slot, (bounds, key), (id, width, height), paint_canvas, |surface, part, stretching, words| {
            surface.stretch(stretching, area, part, width, words)
        });
    }

    pub(crate) fn text(
        &mut self,
        within: Option<Rect>,
        font: &Font,
        (x, y): (i32, i32),
        text: &str,
        colour: Color,
        scale: i32,
    ) -> i32 {
        // Text given no area is cut at the page's edges alone, wherever the frame is shifted to.
        let within = within.map_or(self.bounds(), |within| self.to_shifted(within));
        let (scale, y) = (scale.max(1), y + self.shift);
        let width = font.text_width(text) * scale;
        let bounds = self.to_visible(Rect::new(x, y, width, font.height as i32 * scale), within);
        let key = || {
            let key = Key::default().word(TEXT).word(font.id).number(x).number(y).bytes(text.as_bytes());
            key.colour(colour).word(u64::from(scale.unsigned_abs()))
        };
        self.draw(bounds, key, |surface, part, _, _| {
            surface.text(part, font, (x, y), text, colour, scale, Lettering::default());
        });
        x + width
    }

    pub(crate) fn stamp(&mut self, stamp: &Stamp, within: Rect, x: i32, y: i32, colour: Color) {
        let (within, y, side) = (self.to_shifted(within), y + self.shift, stamp.side as i32);
        let bounds = self.to_visible(Rect::new(x, y, side, side), within);
        if !self.is_drawing {
            self.list.stamps.add(bounds);
        }
        let key = || Key::default().word(STAMP).word(stamp.id).number(x).number(y).colour(colour);
        self.draw(bounds, key, |surface, part, _, _| surface.stamp(stamp, part, x, y, colour));
    }

    pub(crate) fn cover(&mut self, ink: Color, ground: Color, trace: impl FnOnce(&mut Mask)) {
        if !self.is_drawing {
            self.list_outlines(|key| key.word(COVER).colour(ink).colour(ground), trace);
            return;
        }
        // The shades are made for an outline that has a part to draw, and once.
        let mut shades = None;
        self.parts(|surface, part, _, strips| {
            let shades = shades.get_or_insert_with(|| surface.shades(ink, ground));
            surface.paint(strips, part, shades);
        });
    }

    pub(crate) fn cover_over(&mut self, ink: Color, trace: impl FnOnce(&mut Mask)) {
        if !self.is_drawing {
            let bounds = self.list_outlines(|key| key.word(OVER).colour(ink), trace);
            self.list.stamps.add(bounds);
            return;
        }
        self.parts(|surface, part, _, strips| surface.paint_over(strips, part, ink));
    }

    fn list_outlines(&mut self, to_key: impl FnOnce(Key) -> Key, trace: impl FnOnce(&mut Mask)) -> Rect {
        let list = &mut *self.list;
        list.mask.clear();
        list.mask.shift(self.shift as f32);
        trace(&mut list.mask);
        let key = to_key(list.mask.key);
        let start = list.strips.len();
        let bounds = match list.last_ops.iter().find(|op| op.key == key) {
            Some(last) => {
                list.strips.extend_from_slice(&list.last_strips[last.strips.0..last.strips.1]);
                last.bounds
            }
            None => list.mask.strips(&mut list.strips).unwrap_or_default(),
        };
        list.ops.push(Op { bounds, key, strips: (start, list.strips.len()) });
        bounds
    }
}

#[cfg(test)]
mod tests {
    use super::super::panel::Ring;
    use super::super::support::{SQUARE, speckle};
    use super::*;
    use alloc::vec;
    use core::cell::Cell;

    const WIDTH: usize = 64;
    const HEIGHT: usize = 48;

    fn on_pages(
        list: &mut DisplayList,
        frames: i32,
        blank: u32,
        mut scene: impl FnMut(&mut Pen<'_, '_>, i32),
        mut check: impl FnMut(i32, &[u32]),
    ) {
        let mut pages = vec![vec![blank; WIDTH * HEIGHT]; 3];
        let mut ring = Ring::with_pages(pages.len() as u32, pages.len() as u32 - 1);
        for step in 0..frames {
            let (back, format) = (ring.back() as usize, PixelFormat::XRGB8888);
            let mut surface = Surface::new(&mut pages[back], WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
            match list.frame(&mut surface, ring.back_frame(), |pen| scene(pen, step)) {
                FrameKind::New(frame) => {
                    ring.set_back_frame(frame);
                    ring.advance();
                }
                FrameKind::Same(frame) => {
                    assert!(
                        pages[back] == pages[ring.front() as usize],
                        "frame {step}: the page drawn into shows what the screen shows"
                    );
                    ring.skip_back(frame);
                }
            }
            check(step, &pages[ring.front() as usize]);
        }
    }

    fn whole(scene: impl FnMut(&mut Pen<'_, '_>)) -> Vec<u32> {
        let mut whole = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut whole, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        DisplayList::new(WIDTH, HEIGHT, 64).frame(&mut surface, None, scene);
        whole
    }

    fn scene(pen: &mut Pen<'_, '_>, font: &Font, step: i32) {
        let ground = Color(0x10_2030);
        pen.fill(pen.bounds(), ground);
        pen.fill(Rect::new(4 + step, 4, 9, 9), Color(0xFF_E47A));
        pen.text(None, font, (2, 20 + step % 3), "ab", Color(0xFF_B000), 1);
        pen.cover(Color(0xF0_6A00), ground, |mask| {
            let corners = [(40.0, 10.0), (55.5, 13.0), (50.0, 30.25), (38.0, 25.0)];
            mask.outline(corners.into_iter());
        });
        pen.cover(Color(0x55_9A00), ground, |mask| {
            let at = 2.0 + (step % 4) as f32 * 0.37;
            mask.outline([(at, 36.0), (at + 20.0, 38.0), (at + 9.0, 46.0)].into_iter());
        });
    }

    #[test]
    fn partial_frames_equal_whole() {
        let font = crate::gfx::font::spleen_12x24();
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        on_pages(
            &mut list,
            20,
            0,
            |pen, step| scene(pen, &font, step),
            |step, page| {
                assert!(
                    page == whole(|pen| scene(pen, &font, step)),
                    "frame {step}, drawn on a page that showed the frame three before"
                );
            },
        );
        let drawn_within = list.drawn_within();
        assert!(
            drawn_within < WIDTH * HEIGHT / 2,
            "and the last was drawn within what changed, {drawn_within} pixels of {}",
            WIDTH * HEIGHT
        );
    }

    #[test]
    fn unchanged_frame_keeps_serial() {
        let font = crate::gfx::font::spleen_12x24();
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        let shown = whole(|pen| scene(pen, &font, 2));
        let (passes, mut counts) = (Cell::new(0), Vec::new());
        let counted = |pen: &mut Pen<'_, '_>, step: i32| {
            passes.set(passes.get() + 1);
            scene(pen, &font, if step < 9 { 2 } else { 3 });
        };
        on_pages(&mut list, 10, 0, counted, |step, page| {
            counts.push(passes.replace(0));
            assert!(step == 9 || page == shown, "frame {step}: the screen shows the frame drawn whole");
        });
        assert_eq!(
            counts,
            [2, 2, 2, 2, 2, 1, 1, 1, 1, 2],
            "a page whose frame is not known is drawn whatever changed, and one whose changes since are not kept is drawn whole; once every page shows the frame on screen nothing is drawn until something changes"
        );
        let mut page = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut page, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        let FrameKind::New(first) = list.frame(&mut surface, None, |pen| scene(pen, &font, 2)) else {
            panic!("a page whose frame is not known is drawn")
        };
        let same = |list: &mut DisplayList, surface: &mut Surface<'_>| {
            list.frame(surface, Some(first), |pen| scene(pen, &font, 2))
        };
        assert_eq!(same(&mut list, &mut surface), FrameKind::Same(first), "the same frame again is the one on screen");
        let state = (list.serial, list.changes.len(), list.spare.len());
        assert!((0..3).all(|_| same(&mut list, &mut surface) == FrameKind::Same(first)));
        assert_eq!((list.serial, list.changes.len(), list.spare.len()), state, "and takes nothing");
        assert_eq!(list.frame(&mut surface, Some(first), |pen| scene(pen, &font, 3)), FrameKind::New(first + 1));
        assert!(page == whole(|pen| scene(pen, &font, 3)), "the next that changes something is drawn over it");
    }

    #[test]
    fn lagging_page_catches_up() {
        let font = crate::gfx::font::spleen_12x24();
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        let resting = |pen: &mut Pen<'_, '_>, step: i32| scene(pen, &font, step.min(5));
        on_pages(&mut list, 14, 0, resting, |step, page| {
            assert!(page == whole(|pen| resting(pen, step)), "frame {step} on screen is the frame drawn whole");
        });
    }

    #[test]
    fn cover_over_mixes() {
        let format = PixelFormat::XRGB8888;
        let (ground, gold, ember) = (Color(0x10_2030), Color(0xFF_E47A), Color(0xF0_6A00));
        let scene = |pen: &mut Pen<'_, '_>, step: i32| {
            pen.fill(pen.bounds(), ground);
            let lit = ember.toward(gold, (step % 5) as u32, 4);
            pen.cover(lit, ground, |mask| {
                mask.outline([(10.5, 4.0), (16.5, 4.0), (16.5, 40.0), (10.5, 40.0)].into_iter())
            });
            let at = 4.0 + step as f32 * 0.7;
            pen.cover_over(ember, |mask| {
                mask.outline([(at, 20.25), (at + 12.0, 20.25), (at + 12.0, 30.75), (at, 30.75)].into_iter());
            });
        };
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        on_pages(&mut list, 20, 0, &scene, |step, page| {
            let mut truth = whole(|pen| scene(pen, step));
            assert!(page == truth, "frame {step}, drawn on a page that showed the frame three before");
            let surface = Surface::new(&mut truth, WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
            let (lit, edge) = (ember.toward(gold, (step % 5) as u32, 4), surface.pixel(13, 20).unwrap());
            let between = |channel: fn(Color) -> u8| {
                let (one, other) = (channel(lit), channel(ember));
                (one.min(other).saturating_sub(1)..=one.max(other).saturating_add(1)).contains(&channel(edge))
            };
            assert!(
                surface.pixel(13, 10) == Some(lit) && between(Color::r) && between(Color::g) && between(Color::b),
                "frame {step}: where the square's edge lies over the bar the two mix, and nothing of the ground shows: {edge:?}"
            );
        });
    }

    #[test]
    fn shifted_frame_moves_rows() {
        let font = crate::gfx::font::spleen_12x24();
        let format = PixelFormat::XRGB8888;
        let drawn = |shift: i32| {
            let mut page = vec![0u32; WIDTH * HEIGHT];
            let mut surface = Surface::new(&mut page, WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
            DisplayList::new(WIDTH, HEIGHT, 64).frame(&mut surface, None, |pen| {
                pen.shift(shift);
                scene(pen, &font, 3);
                pen.shift(-shift);
                pen.fill(Rect::new(0, 0, 2, 2), Color(0x55_9A00));
            });
            page
        };
        let reference = drawn(0);
        for shift in [-30, -7, 11, 40] {
            let shifted = drawn(shift);
            for row in 0..HEIGHT as i32 {
                let line = &shifted[row as usize * WIDTH..][..WIDTH];
                let from = row - shift;
                let expected: Vec<u32> = if (0..HEIGHT as i32).contains(&from) {
                    reference[from as usize * WIDTH..][..WIDTH].to_vec()
                } else {
                    vec![0; WIDTH]
                };
                assert_eq!(line[2..], expected[2..], "row {row} of the frame shifted by {shift}");
            }
            assert!(shifted[0] == format.pack(Color(0x55_9A00)), "and what comes after it lies where it is given");
        }
    }

    #[test]
    fn stamp_darkens_frame() {
        let stamp = Stamp::new(&crate::gfx::icon::Icon::from_svg(SQUARE).unwrap(), 10.0, 3);
        let format = PixelFormat::XRGB8888;
        let picture: Vec<u32> = (0..30 * 20).map(speckle).collect();
        let frame = |step: i32| {
            let ground = Color(0x10_2030);
            let line = Rect::new(0, 21 + step % 2, WIDTH as i32, 2);
            let at = (8 + step, 10);
            (ground, line, at)
        };
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        let drawn = |pen: &mut Pen<'_, '_>, step: i32| {
            let (ground, line, (x, y)) = frame(step);
            pen.fill(pen.bounds(), ground);
            pen.blit(pen.bounds(), x, y, 30, &picture, step as u64);
            pen.stamp(&stamp, pen.bounds(), x + 4, y + 4, Color(0xFF_B000));
            pen.fill(line, Color(0xF0_6A00));
        };
        on_pages(&mut list, 9, 0x00FF_FFFF, drawn, |step, page| {
            let (ground, line, (x, y)) = frame(step);
            let mut truth = vec![0u32; WIDTH * HEIGHT];
            let mut surface = Surface::new(&mut truth, WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
            surface.fill(surface.bounds(), ground);
            surface.blit(surface.bounds(), x, y, 30, &picture);
            surface.stamp(&stamp, surface.bounds(), x + 4, y + 4, Color(0xFF_B000));
            surface.fill(line, Color(0xF0_6A00));
            assert!(page == truth, "frame {step}, on a page that showed the frame three before");
        });
    }

    #[test]
    fn still_with_swapped_buffers() {
        let stamp = Stamp::new(&crate::gfx::icon::Icon::from_svg(SQUARE).unwrap(), 10.0, 3);
        let font = crate::gfx::font::spleen_12x24();
        let stamped = |pen: &mut Pen<'_, '_>| {
            scene(pen, &font, 2);
            pen.stamp(&stamp, pen.bounds(), 30, 8, Color(0xFF_B000));
            pen.cover_over(Color(0xF0_6A00), |mask| {
                mask.outline([(28.0, 6.0), (44.5, 6.0), (44.5, 20.25), (28.0, 20.25)].into_iter());
            });
        };
        let (mut frames, mut still) = (DisplayList::new(WIDTH, HEIGHT, 64), DisplayList::offscreen(WIDTH, HEIGHT));
        assert!(still.shadow.is_empty() && still.last_strips.capacity() == 0, "it holds no picture or history");
        let mut page = vec![0x00FF_FFFF; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut page, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        for _ in 0..2 {
            still.swap_buffers(&mut frames);
            still.draw_still(&mut surface, stamped);
            still.swap_buffers(&mut frames);
        }
        assert!(page == whole(stamped), "drawn whole, over whatever the page held");
        assert!(frames.shadow.len() == WIDTH * HEIGHT && still.shadow.is_empty(), "and what was traded is given back");
    }

    #[test]
    fn resting_outline_keeps_key() {
        let font = crate::gfx::font::spleen_12x24();
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        let mut page = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut page, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        list.frame(&mut surface, None, |pen| scene(pen, &font, 0));
        let disc = list.ops.iter().find(|op| op.strips.1 > op.strips.0).map(|op| (op.key, op.bounds)).unwrap();
        list.frame(&mut surface, None, |pen| scene(pen, &font, 4));
        assert_eq!(
            list.ops.iter().find(|op| op.strips.1 > op.strips.0).map(|op| (op.key, op.bounds)),
            Some(disc),
            "the disc that did not move is the same drawing"
        );
        assert!(list.ops.iter().filter(|op| op.strips.1 > op.strips.0).count() == 2);
    }

    #[test]
    fn plate_painted_on_change() {
        let painted = Cell::new(0);
        let scene = |pen: &mut Pen<'_, '_>, step: i32, is_counted: bool| {
            pen.fill(pen.bounds(), Color(0x10_2030));
            let colour = if step < 6 { 0x00FF_E47A } else { 0x00F0_6A00 };
            pen.plate(0, pen.bounds(), (4, 4), (20, 10), 0, u64::from(colour), |plate| {
                painted.set(painted.get() + u32::from(is_counted));
                plate.fill(plate.bounds(), Color(colour));
            });
            let square = if step < 9 { Rect::new(34 + step, 30, 6, 6) } else { Rect::new(10, 8, 6, 6) };
            pen.fill(square, Color(0x55_9A00));
        };
        let mut list = DisplayList::new(WIDTH, HEIGHT, 64);
        on_pages(
            &mut list,
            12,
            0,
            |pen, step| scene(pen, step, true),
            |step, page| {
                assert!(page == whole(|pen| scene(pen, step, false)), "frame {step}, on a page that showed one before");
            },
        );
        assert_eq!(
            painted.get(),
            2,
            "painted for its first colour and for its second, and copied on pages that lagged and under the square"
        );
    }

    #[test]
    fn turned_plate_and_stretched_canvas() {
        let format = PixelFormat::XRGB8888;
        let picture: Vec<u32> = (0..12 * 5).map(speckle).collect();
        let within = Rect::new(6, 3, 50, 40);
        for quarters in 0..4u8 {
            let drawn = whole(|pen| {
                pen.plate(0, within, (7, 2), (12, 5), quarters, u64::from(quarters), |plate| {
                    plate.blit(plate.bounds(), 0, 0, 12, &picture)
                })
            });
            let (mut truth, mut turned) = (vec![0u32; WIDTH * HEIGHT], Vec::new());
            let across = turn_words(&picture, 12, Turn { quarters, is_mirrored: false }, &mut turned);
            let at = within.place(quarters, 7, 2, 12, 5);
            let mut surface = Surface::new(&mut truth, WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
            surface.blit(surface.bounds(), at.x, at.y, across, &turned);
            assert!(drawn == truth, "{quarters} quarters: the picture turned where its viewer sees (7, 2)");
        }
        let area = Rect::new(3, 5, 41, 17);
        let stretched = whole(|pen| {
            let bounds = pen.bounds();
            pen.stretch_canvas(1, (area, bounds), (12, 5), 7, |words, _| words.copy_from_slice(&picture));
        });
        let mut truth = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut truth, WIDTH as u32, HEIGHT as u32, WIDTH, format).unwrap();
        surface.stretch(&mut Stretching::default(), area, surface.bounds(), 12, &picture);
        assert!(stretched == truth, "and a canvas stretched is its picture stretched");
    }
}
