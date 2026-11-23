use crate::gfx::Rect;
use crate::gfx::pen::Pen;

#[derive(Clone, Copy)]
pub(in crate::ui) struct ScreenPicture<'a> {
    pub(in crate::ui) words: &'a [u32],
    pub(in crate::ui) width: usize,
    pub(in crate::ui) id: u64,
}

impl ScreenPicture<'_> {
    pub(in crate::ui) fn draw(self, pen: &mut Pen<'_, '_>, top: i32) {
        pen.blit(pen.bounds(), 0, top, self.width, self.words, self.id);
    }

    pub(in crate::ui) fn draw_rolled(self, pen: &mut Pen<'_, '_>, area: Rect) {
        pen.stretch(area, area, self.width, self.words, self.id);
    }
}
