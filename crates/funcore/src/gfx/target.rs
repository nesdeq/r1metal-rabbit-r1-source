use super::font::Font;
use super::mask::Mask;
use super::pen::Pen;
use super::{Lettering, Rect, Surface};
use crate::base::pixel::Color;
use core::cell::RefCell;

pub(crate) trait Target {
    fn fill(&mut self, rect: Rect, colour: Color);
    fn write(
        &mut self,
        within: Option<Rect>,
        font: &Font,
        at: (i32, i32),
        text: &str,
        colour: Color,
        scale: i32,
    ) -> i32;
    fn cover(&mut self, mask: &RefCell<Mask>, colours: (Color, Color), trace: impl FnOnce(&mut Mask));
}

impl Target for Pen<'_, '_> {
    fn fill(&mut self, rect: Rect, colour: Color) {
        Pen::fill(self, rect, colour);
    }

    fn write(
        &mut self,
        within: Option<Rect>,
        font: &Font,
        at: (i32, i32),
        text: &str,
        colour: Color,
        scale: i32,
    ) -> i32 {
        self.text(within, font, at, text, colour, scale)
    }

    fn cover(&mut self, _: &RefCell<Mask>, (ink, ground): (Color, Color), trace: impl FnOnce(&mut Mask)) {
        Pen::cover(self, ink, ground, trace);
    }
}

impl Target for Surface<'_> {
    fn fill(&mut self, rect: Rect, colour: Color) {
        Surface::fill(self, rect, colour);
    }

    fn write(
        &mut self,
        within: Option<Rect>,
        font: &Font,
        at: (i32, i32),
        text: &str,
        colour: Color,
        scale: i32,
    ) -> i32 {
        self.text(within.unwrap_or(self.bounds()), font, at, text, colour, scale, Lettering::default())
    }

    fn cover(&mut self, mask: &RefCell<Mask>, (ink, ground): (Color, Color), trace: impl FnOnce(&mut Mask)) {
        let mut mask = mask.borrow_mut();
        mask.clear();
        trace(&mut mask);
        Surface::cover(self, 0, 0, &mut mask, ink, ground);
    }
}
