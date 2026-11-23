use super::field::{add_ticking_key, draw_field, draw_field_within};
use super::kit::{Canvas, Kit, draw_plate, draw_rule};
use super::ruler::RULE;
use crate::base::pixel::Color;
use crate::gfx::key::Key;
use crate::gfx::pen::Pen;
use crate::gfx::refresh::REFRESH_NS;
use crate::gfx::store::Unfold;
use crate::gfx::{Lettering, Rect};
use alloc::string::String;
use core::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BarText {
    Plain,
    Trail,
    Speech,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct BarStyle {
    pub(super) text: BarText,
    pub(super) has_figure: bool,
    pub(super) has_selection: bool,
    pub(super) has_pager: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Typing {
    Done,
    Due,
    Start(u64),
}

pub(super) const fn letter_count(start_ns: u64, now_ns: u64) -> usize {
    (now_ns.saturating_sub(start_ns) / REFRESH_NS) as usize
}

#[derive(Clone, Copy)]
pub(super) struct WordPlace<P> {
    pub(super) place: P,
    pub(super) is_beneath: bool,
    pub(super) may_unfold: bool,
}

#[derive(Default)]
pub(super) struct BarWord<P> {
    pub(super) current: String,
    pub(super) previous: String,
    pub(super) unfold: Unfold,
    pub(super) place: Option<P>,
    pub(super) is_drawn: Cell<bool>,
}

fn is_same_but_figures(a: &str, b: &str) -> bool {
    a.chars().filter(|letter| !letter.is_ascii_digit()).eq(b.chars().filter(|letter| !letter.is_ascii_digit()))
}

impl<P: Copy + PartialEq> BarWord<P> {
    fn does_unfold(&self, text: &str, place: WordPlace<P>) -> bool {
        let is_new_word = !is_same_but_figures(text, &self.current);
        self.place == Some(place.place) && place.may_unfold && self.unfold.is_whole() && is_new_word
    }

    fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        (at, room, colour): ((i32, i32), i32, Color),
        text: &str,
        place: WordPlace<P>,
    ) {
        let ruler = kit.ruler;
        self.is_drawn.set(true);
        if place.is_beneath || self.place != Some(place.place) {
            return draw_field(kit, pen, &ruler.body, at, text, room, colour);
        }
        if !self.unfold.is_whole() {
            let field = Rect::new(at.0, at.1, room.max(0), ruler.small());
            draw_field_within(kit, pen, at, &self.previous, room, colour, field);
            let open_area = self.unfold.open_area(field);
            pen.fill(open_area, kit.roles.background);
            return draw_field_within(kit, pen, at, text, room, colour, open_area);
        }
        let word = if self.does_unfold(text, place) { self.current.as_str() } else { text };
        draw_field(kit, pen, &ruler.body, at, word, room, colour);
    }

    pub(super) fn note_written(&mut self, text: &str, place: WordPlace<P>) {
        if !self.is_drawn.get() {
            self.place = None;
            return;
        }
        if self.does_unfold(text, place) {
            core::mem::swap(&mut self.previous, &mut self.current);
            self.current.clear();
            self.current.push_str(text);
            self.unfold.begin();
        } else {
            if self.place != Some(place.place) {
                self.unfold.land();
            }
            if self.current != *text {
                self.current.clear();
                self.current.push_str(text);
            }
        }
        self.place = Some(place.place);
    }
}

pub(super) struct Bar<'a> {
    pub(super) name: &'a str,
    pub(super) text: &'a str,
    pub(super) typing: Option<&'a str>,
    pub(super) room: i32,
}

impl Bar<'_> {
    pub(super) fn draw<P: Copy + PartialEq>(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        style: BarText,
        word: &BarWord<P>,
        place: WordPlace<P>,
    ) {
        let (ruler, roles, room) = (kit.ruler, kit.roles, self.room);
        let (left, top) = (ruler.under_bar().x, ruler.bar_top());
        let at = (left, top + ruler.large());
        pen.text(None, &ruler.heading, (left, top), self.name, roles.text, 1);
        match (style, self.typing) {
            (BarText::Trail, _) => {
                draw_field(kit, pen, &ruler.body, at, self.text, room, roles.text_dim);
                draw_rule(pen, kit, ruler.bar() - RULE);
            }
            (BarText::Speech, Some(typing)) => draw_field(kit, pen, &ruler.body, at, typing, room, roles.content),
            (BarText::Speech, None) => word.draw(pen, kit, (at, room, roles.content), self.text, place),
            (BarText::Plain, _) => word.draw(pen, kit, (at, room, roles.text_dim), self.text, place),
        }
    }
}

pub(super) fn draw_turned_bar(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (name, text): (&str, &str),
    robot_width: i32,
    quarters: u8,
) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (screen, top) = (Rect::new(0, 0, ruler.width, ruler.height), ruler.bar_top());
    let (width, height) = (screen.view_size(quarters).0, ruler.bar());
    let room = width - 3 * ruler.gap - robot_width;
    let key = Key::default().bytes(name.as_bytes()).word(ruler.heading.id).number(ruler.gap).number(top);
    let colours = key.colour(roles.background).colour(roles.text);
    let key = add_ticking_key(kit.ticking, colours, &ruler.body, text, room).colour(roles.text_dim);
    draw_plate(pen, (Canvas::Header, screen), (0, 0), (width, height), (key, quarters), |plate| {
        let (gap, text_top) = (ruler.gap, top + ruler.large());
        plate.fill(plate.bounds(), roles.background);
        plate.text(plate.bounds(), &ruler.heading, (gap, top), name, roles.text, 1, Lettering::default());
        draw_field(kit, plate, &ruler.body, (gap, text_top), text, room, roles.text_dim);
    });
}

pub(super) struct Status<'a> {
    pub(super) state: &'a str,
    pub(super) words: Option<&'a str>,
    pub(super) clock: Option<(&'a str, &'a str)>,
}

impl Status<'_> {
    pub(super) fn draw(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>) {
        let (ruler, roles, height) = (kit.ruler, kit.roles, kit.ruler.status_height());
        let (gap, small_cell, small) = (ruler.gap, ruler.small_cell(), ruler.small());
        let top = ruler.height - height;
        pen.fill(Rect::new(0, top, ruler.width, height), roles.rule);
        let y = top + (height - small) / 2;
        let state_width = ruler.body.text_width(self.state);
        let room = ruler.width - 3 * gap - state_width;
        match (self.words, self.clock) {
            (Some(words), _) => draw_field(kit, pen, &ruler.body, (gap, y), words, room, roles.text),
            (None, Some((time, day))) => {
                let day_left = pen.text(None, &ruler.body, (gap, y), time, roles.text, 1) + small_cell;
                draw_field(kit, pen, &ruler.body, (day_left, y), day, room - (day_left - gap), roles.text);
            }
            (None, None) => {}
        }
        pen.text(None, &ruler.body, (ruler.width - gap - state_width, y), self.state, roles.text, 1);
    }
}
