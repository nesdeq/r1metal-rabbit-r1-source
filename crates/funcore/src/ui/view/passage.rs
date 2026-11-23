use crate::base::math::round;
use crate::gfx::key::Ids;
use crate::gfx::motion::Edge;
use crate::gfx::pen::{DisplayList, Pen};
use crate::gfx::refresh::REFRESH_NS;
use crate::gfx::{Rect, Surface};
use crate::ui::bar::Typing;
use crate::ui::layout::Motion;
use crate::ui::picture::screen::ScreenPicture;
use crate::ui::screen::Screen;
use crate::ui::view::rooms::TextRoomId;
use crate::ui::{FORWARD, Ui};
use alloc::vec::Vec;

#[derive(Clone, Copy)]
pub(in crate::ui) struct Passage {
    pub(in crate::ui) way: f32,
    pub(in crate::ui) motion: Motion,
    pub(in crate::ui) at: Edge,
    id: u64,
    frame: u64,
    pub(in crate::ui) start_ns: Option<u64>,
}

pub(in crate::ui) struct Offscreen {
    picture: Vec<u32>,
    pub(in crate::ui) spare: Vec<u32>,
    pub(in crate::ui) under: Vec<u32>,
    pub(in crate::ui) under_id: u64,
    pub(in crate::ui) display_list: DisplayList,
}

impl Offscreen {
    pub(in crate::ui) fn new(width: usize, height: usize, display_list: DisplayList, ids: &Ids) -> Offscreen {
        let screen = || alloc::vec![0; width * height];
        Offscreen { picture: screen(), spare: screen(), under: screen(), under_id: ids.next(), display_list }
    }
}

impl Ui {
    pub(in crate::ui) fn printed_rows(&self, start_ns: u64, now_ns: u64) -> i32 {
        let lines = now_ns.saturating_sub(start_ns) / REFRESH_NS + 1;
        (lines as i32).saturating_mul(self.ruler.line_icon_side().max(1))
    }

    pub(in crate::ui) fn draw_scene(&self, pen: &mut Pen<'_, '_>, now_ns: u64) {
        let Some(passage) = self.passage else {
            return self.draw_screen(pen, now_ns);
        };
        let (width, height, row_width) = (self.ruler.width, self.ruler.height, self.ruler.width.max(0) as usize);
        let at = round(passage.at.at);
        let arrival = at - round(passage.way) * height;
        let previous = ScreenPicture { words: &self.offscreen.picture, width: row_width, id: passage.id };
        match passage.motion {
            Motion::Print => {
                self.draw_screen(pen, now_ns);
                let printed = self.printed_rows(passage.start_ns.unwrap_or(now_ns), now_ns).min(height);
                pen.fill(Rect::new(0, printed, width, height - printed), self.roles.background);
            }
            Motion::Deal if passage.way == FORWARD => {
                previous.draw(pen, 0);
                self.draw_shifted(pen, arrival, now_ns);
            }
            Motion::Deal => {
                self.draw_screen(pen, now_ns);
                previous.draw(pen, at);
            }
            Motion::Roll => {
                self.draw_shifted(pen, arrival, now_ns);
                let area = if passage.way == FORWARD {
                    Rect::new(0, 0, width, height + at)
                } else {
                    Rect::new(0, at, width, height - at)
                };
                previous.draw_rolled(pen, area);
            }
            Motion::Slide | Motion::Typed => {
                self.draw_shifted(pen, arrival, now_ns);
                previous.draw(pen, at);
            }
        }
    }

    fn draw_shifted(&self, pen: &mut Pen<'_, '_>, shift: i32, now_ns: u64) {
        pen.shift(shift);
        self.draw_screen(pen, now_ns);
        pen.shift(-shift);
    }

    pub(in crate::ui) fn depart(&mut self, way: f32) {
        if self.is_dark || self.passage.is_some_and(|passage| passage.frame == self.frame) {
            return;
        }
        let motion = self.look().motion;
        if motion == Motion::Typed {
            self.typing = Typing::Due;
        }
        let id = self.ids.next();
        let passage = Passage { way, motion, at: Edge::still(0.0), id, frame: self.frame, start_ns: None };
        if motion == Motion::Print {
            self.passage = Some(passage);
            return;
        }
        self.ready(self.frame_ns);
        let previous = self.draw_offscreen(|ui, pen| ui.draw_scene(pen, ui.frame_ns));
        self.offscreen.spare = core::mem::replace(&mut self.offscreen.picture, previous);
        self.passage = Some(passage);
    }

    pub(in crate::ui) fn draw_offscreen(&mut self, mut draw: impl FnMut(&Ui, &mut Pen<'_, '_>)) -> Vec<u32> {
        let (width, height) = (self.ruler.width.max(0) as usize, self.ruler.height.max(0) as usize);
        let mut list = core::mem::take(&mut self.offscreen.display_list);
        let mut words = core::mem::take(&mut self.offscreen.spare);
        words.resize(width * height, 0);
        if let Some(mut surface) = Surface::new(&mut words, width as u32, height as u32, width, self.format) {
            list.swap_buffers(&mut self.display_list);
            let ui = &*self;
            list.draw_still(&mut surface, |pen| draw(ui, pen));
            list.swap_buffers(&mut self.display_list);
        }
        self.offscreen.display_list = list;
        words
    }

    pub(in crate::ui) fn land_screen(&mut self) {
        self.dial.roll = Edge::still(self.dial_aim().unwrap_or(self.dial.roll.at));
        if let Some(grid) = self.screen_grid_mut() {
            grid.land();
        }
        match (self.screen, self.shown_room()) {
            (Some(Screen::Keyboard | Screen::Pin), _) => {
                if let Some(id) = self.keys_up() {
                    self.keyboard_mut(id).land();
                }
            }
            (_, Some(TextRoomId::Reader)) => self.land_reader(),
            (_, Some(TextRoomId::Talk)) => self.land_talk(),
            (_, Some(TextRoomId::Chat)) => self.land_chat(),
            (_, Some(TextRoomId::Page(paged))) => self.land_paged(paged),
            (_, Some(TextRoomId::Conversation)) => self.land_conversation(),
            (_, None) => {}
        }
    }
}
