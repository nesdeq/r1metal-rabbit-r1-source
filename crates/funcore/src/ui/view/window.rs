use super::lines::List;
use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::motion::Edge;
use crate::gfx::pen::Pen;
use crate::ui::Ui;
use crate::ui::picture::screen::ScreenPicture;
use crate::ui::robot::Seat;
use crate::ui::screen::Screen;
use crate::ui::screen::bar::BarWords;

pub(in crate::ui) struct Window {
    pub(in crate::ui) name: &'static str,
    pub(in crate::ui) beneath: Option<Screen>,
    pub(in crate::ui) is_open: bool,
    pub(in crate::ui) rise: Edge,
    pub(in crate::ui) is_closing: bool,
}

impl Window {
    pub(in crate::ui) const fn new(name: &'static str) -> Window {
        Window { name, beneath: None, is_open: false, rise: Edge::still(0.0), is_closing: false }
    }

    pub(in crate::ui) fn rise_over(&mut self, beneath: Option<Screen>, height: i32) {
        (self.beneath, self.is_open, self.is_closing) = (beneath, true, false);
        self.rise = Edge::still(height as f32);
    }

    pub(in crate::ui) const fn aim(&self, height: i32) -> f32 {
        if self.is_closing { height as f32 } else { 0.0 }
    }

    fn is_gone(&self, height: i32) -> bool {
        self.is_closing && self.rise == Edge::still(height as f32)
    }
}

impl Ui {
    pub(in crate::ui) fn open_window(&self) -> Option<&Window> {
        match self.screen {
            Some(Screen::Safe) => Some(&self.safe.window),
            Some(Screen::Key) => Some(&self.keying.window),
            Some(Screen::Ring) => Some(&self.bells.window),
            Some(Screen::Incoming) => Some(&self.calls.window),
            _ => None,
        }
    }

    // The name a window's bar shows: a ringing call's whose it is, any other window's its own.
    fn window_name<'a>(&'a self, window: &'a Window) -> &'a str {
        match self.screen {
            Some(Screen::Incoming) => self.cut_title(self.call_title()),
            _ => window.name,
        }
    }

    pub(in crate::ui) fn list_under(&self) -> Option<List> {
        match self.open_window()?.beneath {
            Some(screen) => List::of(screen),
            None => self.menu_list(),
        }
    }

    pub(in crate::ui) fn does_window_move(&self) -> bool {
        self.open_window().is_some_and(|window| window.rise != Edge::still(window.aim(self.ruler.height)))
    }

    pub(in crate::ui) fn end_window(&mut self) {
        let (gone, height) = (self.screen, self.ruler.height);
        let window = match gone {
            Some(Screen::Safe) => &mut self.safe.window,
            Some(Screen::Key) => &mut self.keying.window,
            Some(Screen::Ring) => &mut self.bells.window,
            Some(Screen::Incoming) => &mut self.calls.window,
            _ => return,
        };
        if !window.is_gone(height) {
            return;
        }
        (window.is_open, self.screen) = (false, window.beneath);
        match gone {
            Some(Screen::Safe) => self.safe_gone(),
            Some(Screen::Ring) => self.ring_gone(),
            Some(Screen::Incoming) => self.incoming_gone(),
            _ => self.key_gone(),
        }
    }

    pub(in crate::ui) fn window_seat(&self) -> Seat {
        match self.open_window() {
            Some(window) if window.is_closing && window.beneath.is_none() => self.home_seat(),
            _ => self.bar_seat(),
        }
    }

    pub(in crate::ui) fn ready_beneath(&mut self, now_ns: u64) {
        let Some(beneath) = self.open_window().filter(|window| round(window.rise.at) > 0).map(|window| window.beneath)
        else {
            return;
        };
        let front = core::mem::replace(&mut self.screen, beneath);
        self.is_beneath = true;
        self.ready_room();
        let under = self.draw_offscreen(|ui, pen| {
            ui.draw_screen(pen, now_ns);
            ui.draw_rail(pen);
        });
        (self.is_beneath, self.screen) = (false, front);
        if under != self.offscreen.under {
            self.offscreen.under_id = self.ids.next();
        }
        self.offscreen.spare = core::mem::replace(&mut self.offscreen.under, under);
    }

    pub(in crate::ui) fn draw_window(
        &self,
        pen: &mut Pen<'_, '_>,
        words: &BarWords<'_>,
        content: impl FnOnce(&Ui, &mut Pen<'_, '_>),
    ) {
        let Some(window) = self.open_window() else {
            return;
        };
        let (drop, name, background) = (round(window.rise.at), self.window_name(window), self.roles.background);
        if drop > 0 {
            let width = self.ruler.width.max(0) as usize;
            ScreenPicture { words: &self.offscreen.under, width, id: self.offscreen.under_id }.draw(pen, 0);
        }
        pen.shift(drop);
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.height), background);
        content(self, pen);
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), background);
        self.draw_bar(pen, name, words);
        pen.shift(-drop);
    }
}
