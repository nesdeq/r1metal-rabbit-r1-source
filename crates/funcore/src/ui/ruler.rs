use crate::base::math::{round, sqrt};
use crate::gfx::Rect;
use crate::gfx::font::{self, Font};
use crate::gfx::measures::{self, PHI, PHI2, PHI3};

pub(super) const LIVE_SHARE: f32 = 5.0 / 6.0;
pub(super) const HOME_SCALE: f32 = PHI;
pub(super) const RULE: i32 = 1;
pub(super) const SENTENCE_LINES: usize = 2;

pub(super) fn diagonal(width: f32, height: f32) -> f32 {
    sqrt(width * width + height * height)
}

pub(super) struct Ruler {
    pub(super) heading: Font,
    pub(super) body: Font,
    spare_font: Font,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) band: f32,
    pub(super) gap: i32,
    pub(super) robot: (f32, f32),
    has_status: bool,
    pub(super) left: i32,
}

impl Ruler {
    pub(super) fn new(width: i32, height: i32, robot: (f32, f32)) -> Ruler {
        let (heading, body) = (font::spleen_16x32(), font::spleen_12x24());
        let (band, gap) = measures::measure(&heading, &body);
        Ruler { spare_font: body.clone(), heading, body, width, height, band, gap, robot, has_status: false, left: 0 }
    }

    pub(super) fn set_look(&mut self, is_one_size: bool, has_status: bool, has_rail: bool) {
        if is_one_size != (self.heading.id == self.body.id) {
            core::mem::swap(&mut self.heading, &mut self.spare_font);
        }
        self.has_status = has_status;
        self.left = if has_rail { self.rail_side() + 2 * self.gap } else { 0 };
    }

    pub(super) const fn status_height(&self) -> i32 {
        if self.has_status { self.small() + self.gap } else { 0 }
    }

    pub(super) const fn pane_width(&self) -> i32 {
        self.width - self.left
    }

    pub(super) const fn centre_left(&self, width: i32) -> i32 {
        self.left + (self.pane_width() - width) / 2
    }

    pub(super) fn under_bar(&self) -> Rect {
        let (gap, bar) = (self.gap, self.bar());
        let (left, height) = (self.left + gap, (self.height - bar - gap - self.status_height()).max(0));
        Rect::new(left, bar, (self.pane_width() - 2 * gap).max(0), height)
    }

    pub(super) fn room(&self) -> Rect {
        let bar = self.bar();
        Rect::new(0, bar, self.width, (self.height - bar - self.status_height()).max(0))
    }

    pub(super) fn room_size(&self) -> (usize, usize) {
        let room = self.room();
        (room.w.max(0) as usize, room.h.max(0) as usize)
    }

    pub(super) fn secret_cells(&self) -> usize {
        ((self.pane_width() - 3 * self.gap - self.large()) / self.cell()).max(1) as usize
    }

    pub(super) fn text_cells(&self) -> ((usize, usize), (i32, i32)) {
        let room = self.under_bar().w;
        let cells = |width: u32| (room / (width as i32).max(1)).max(1) as usize;
        ((cells(self.body.width), cells(self.heading.width)), (self.small(), self.large()))
    }

    pub(super) const fn large(&self) -> i32 {
        self.heading.height as i32
    }

    pub(super) const fn small(&self) -> i32 {
        self.body.height as i32
    }

    pub(super) const fn cell(&self) -> i32 {
        self.heading.width as i32
    }

    pub(super) const fn small_cell(&self) -> i32 {
        self.body.width as i32
    }

    pub(super) fn gap_phi(&self) -> i32 {
        round(self.gap as f32 * PHI)
    }

    pub(super) fn gap_phi2(&self) -> i32 {
        round(self.gap as f32 * PHI2)
    }

    pub(super) fn visible(&self) -> f32 {
        measures::visible(self.band)
    }

    pub(super) fn lines(&self) -> (i32, i32) {
        (measures::thin_line(self.band), round(self.band / (PHI3 * PHI2)).max(1))
    }

    pub(super) fn rim(&self) -> f32 {
        self.band / PHI2
    }

    pub(super) fn block(&self) -> (i32, i32, i32) {
        (round(self.rim()), round(self.band / PHI3), self.lines().1)
    }

    pub(super) fn bar(&self) -> i32 {
        measures::bar(self.band, self.gap)
    }

    pub(super) fn bar_top(&self) -> i32 {
        (self.bar() - self.large() - self.small()) / 2
    }

    pub(super) fn note_drop(&self) -> f32 {
        (self.small() + self.gap) as f32
    }

    pub(super) fn line_icon_side(&self) -> i32 {
        self.large() + self.gap
    }

    pub(super) fn rail_side(&self) -> i32 {
        self.line_icon_side() + 2 * self.framed_inset()
    }

    // The rail's foot under its icons: the battery as tall as WiFi over the bell, the time, a gap under each.
    pub(super) const fn rail_foot(&self) -> i32 {
        3 * self.small() + 2 * self.gap
    }

    pub(super) fn stage_side(&self) -> f32 {
        self.band * PHI / LIVE_SHARE * PHI2
    }

    pub(super) fn bar_robot_width(&self) -> f32 {
        self.visible() * self.robot.0 / self.robot.1
    }

    pub(super) fn robot_margin(&self) -> i32 {
        let (width, height) = self.robot;
        let size = self.stage_side();
        round(diagonal(size * HOME_SCALE * width, size * HOME_SCALE * height) / 2.0 - size / 2.0)
    }

    pub(super) fn home_box(&self) -> (f32, f32) {
        let height = self.visible() * PHI;
        (height * self.robot.0 / self.robot.1, height)
    }

    pub(super) fn home_side(&self) -> i32 {
        let (width, height) = self.home_box();
        round(diagonal(width, height))
    }

    pub(super) fn square_foot(&self) -> i32 {
        self.gap + self.home_side() + self.gap_phi()
    }

    pub(super) fn framed_inset(&self) -> i32 {
        self.lines().1 + self.gap
    }

    pub(super) fn card_inset(&self) -> i32 {
        self.lines().0 + self.gap
    }

    pub(super) fn tile_height(&self) -> i32 {
        2 * self.card_inset() + self.line_icon_side() + self.large() + self.small()
    }

    pub(super) fn card_icon(&self, room: i32, text_height: i32) -> i32 {
        round(text_height as f32 * PHI).min(room).max(0)
    }

    pub(super) fn card_gauge_block(&self) -> (i32, i32) {
        (round(self.rim() / PHI), self.lines().0)
    }

    pub(super) fn typed_left(&self) -> i32 {
        self.gap * 3 / 2
    }

    pub(super) fn tick_height(&self) -> i32 {
        round(self.band / PHI3)
    }

    pub(super) const fn time_height(&self, scale: i32) -> i32 {
        scale * self.large() + self.small() + 2 * self.gap
    }

    pub(super) const fn foot_top(&self) -> i32 {
        self.height - self.gap - self.small() - self.large()
    }

    pub(super) const fn top_line_height(&self) -> i32 {
        self.large() + 2 * self.gap
    }
}
