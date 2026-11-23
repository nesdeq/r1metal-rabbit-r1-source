/// A pixel area by its top left corner and size.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub(crate) const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub(crate) const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub(crate) const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub(crate) const fn middle(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub(crate) const fn centre(&self, w: i32, h: i32) -> Rect {
        Rect::new(self.x + (self.w - w) / 2, self.y + (self.h - h) / 2, w, h)
    }

    pub(crate) const fn view_size(&self, quarters: u8) -> (i32, i32) {
        turned_size((self.w, self.h), quarters)
    }

    pub(crate) const fn place(&self, quarters: u8, x: i32, y: i32, w: i32, h: i32) -> Rect {
        match quarters % 4 {
            0 => Rect::new(self.x + x, self.y + y, w, h),
            1 => Rect::new(self.right() - y - h, self.y + x, h, w),
            2 => Rect::new(self.right() - x - w, self.bottom() - y - h, w, h),
            _ => Rect::new(self.x + y, self.bottom() - x - w, h, w),
        }
    }

    pub(crate) fn clip(&self, to: Rect) -> Option<Rect> {
        let x = self.x.max(to.x);
        let y = self.y.max(to.y);
        let w = self.right().min(to.right()) - x;
        let h = self.bottom().min(to.bottom()) - y;
        (w > 0 && h > 0).then_some(Rect { x, y, w, h })
    }

    #[cfg(test)]
    fn around(&self, other: Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }
}

pub(crate) const fn turned_size<T: Copy>((across, down): (T, T), quarters: u8) -> (T, T) {
    if quarters % 2 == 1 { (down, across) } else { (across, down) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_turned_for_viewer() {
        let area = Rect::new(60, 0, 360, 640);
        assert_eq!(
            [0, 1, 2, 3].map(|quarters| area.view_size(quarters)),
            [(360, 640), (640, 360), (360, 640), (640, 360)]
        );
        assert_eq!(area.place(0, 8, 8, 120, 32), Rect::new(68, 8, 120, 32));
        assert_eq!(
            area.place(1, 8, 8, 120, 32),
            Rect::new(380, 8, 32, 120),
            "the viewer's top left is the screen's top right"
        );
        assert_eq!(area.place(2, 8, 8, 120, 32), Rect::new(292, 600, 120, 32));
        assert_eq!(
            area.place(3, 8, 8, 120, 32),
            Rect::new(68, 512, 32, 120),
            "the viewer's top left is the screen's bottom left"
        );
        assert_eq!(area.place(1, 0, 0, 640, 360), area, "what fills the area as seen fills it on the screen");
    }

    #[test]
    fn clip_and_around() {
        let (first, second) = (Rect::new(0, 0, 96, 96), Rect::new(48, 48, 96, 96));
        assert_eq!(first.clip(second), Some(Rect::new(48, 48, 48, 48)));
        assert_eq!(first.clip(Rect::new(96, 0, 10, 10)), None, "side by side is not meeting");
        assert_eq!(first.around(second), Rect::new(0, 0, 144, 144));
        assert_eq!(second.around(Rect::new(10, 200, 5, 5)), Rect::new(10, 48, 134, 157));
    }
}
