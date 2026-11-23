use super::field::draw_field;
use super::kit::{IconPlace, Kit, draw_card};
use super::ruler::Ruler;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::base::time::NANOS_PER_SECOND;
use crate::gfx::Rect;
use crate::gfx::icon::Icon;
use crate::gfx::measures::PHI2;
use crate::gfx::motion::Edge;
use crate::gfx::palette::Roles;
use crate::gfx::pen::Pen;
use crate::gfx::target::Target;

const OPEN: &str = "[";
const CLOSE: &str = "]";
const SWEEP_PIXELS_PER_SECOND: u64 = 240;

pub(crate) struct Gauge<'a> {
    pub(super) icon: &'a Icon,
    pub(super) name: &'a str,
    pub(super) value: &'a str,
    pub(super) blocks: &'a [Edge],
    pub(super) is_set: bool,
    pub(super) sway: f32,
}

#[derive(Clone, Copy)]
pub(super) struct NamePlace {
    pub(super) at: (i32, i32),
    pub(super) room: i32,
}

#[derive(Clone, Copy)]
pub(super) struct ValuePlace {
    pub(super) at: (i32, i32),
    pub(super) scale: i32,
}

#[derive(Clone, Copy)]
pub(super) struct BlockRow {
    pub(super) first: Rect,
    pub(super) step: i32,
}

impl BlockRow {
    fn block(self, at: usize) -> Rect {
        Rect { x: self.first.x + at as i32 * self.step, ..self.first }
    }

    pub(super) fn fill(self, target: &mut impl Target, colours: impl IntoIterator<Item = Option<Color>>) {
        for (at, colour) in colours.into_iter().enumerate() {
            if let Some(colour) = colour {
                target.fill(self.block(at), colour);
            }
        }
    }

    pub(super) fn draw_lit(
        self,
        target: &mut impl Target,
        shares: impl Iterator<Item = f32>,
        (ground, scale): (Option<Color>, f32),
        colour: Color,
    ) {
        for (at, share) in shares.enumerate() {
            let area = self.block(at);
            if let Some(ground) = ground {
                target.fill(scale_area(area, scale), ground);
            }
            target.fill(scale_area(area, scale * share), colour);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Brackets {
    pub(super) left: i32,
    pub(super) right: i32,
    pub(super) y: i32,
}

#[derive(Clone, Copy, Default)]
pub(super) struct GaugePlace {
    pub(super) card: Option<Rect>,
    pub(super) icon: Option<IconPlace>,
    pub(super) is_icon_lit: bool,
    pub(super) name: Option<NamePlace>,
    pub(super) value: Option<ValuePlace>,
    pub(super) blocks: Option<BlockRow>,
    pub(super) brackets: Option<Brackets>,
}

fn scale_area(area: Rect, share: f32) -> Rect {
    let (width, height) = (round(area.w as f32 * share.max(0.0)), round(area.h as f32 * share.max(0.0)));
    Rect::new(area.x + (area.w - width).div_euclid(2), area.y + (area.h - height).div_euclid(2), width, height)
}

pub(super) fn lit_blocks<const BLOCKS: usize>(count: usize) -> [f32; BLOCKS] {
    core::array::from_fn(|block| if block < count { 1.0 } else { 0.0 })
}

fn draw_ground(pen: &mut Pen<'_, '_>, roles: &Roles, card: Option<Rect>) -> Color {
    let Some(card) = card else {
        return roles.background;
    };
    draw_card(pen, card, roles.surface());
    roles.surface()
}

impl Gauge<'_> {
    const fn colour(&self, roles: &Roles) -> Color {
        if self.is_set { roles.text } else { roles.text_dim }
    }

    pub(super) fn draw_blocks(&self, pen: &mut Pen<'_, '_>, roles: &Roles, row: BlockRow) {
        let first = Rect { x: row.first.x + round(self.sway), ..row.first };
        let shares = self.blocks.iter().map(|block| block.at);
        BlockRow { first, ..row }.draw_lit(pen, shares, (Some(roles.rule), 1.0), self.colour(roles));
    }

    pub(super) fn draw(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, place: &GaugePlace) {
        let (ruler, roles, colour) = (kit.ruler, &kit.roles, self.colour(&kit.roles));
        let ground = draw_ground(pen, roles, place.card);
        if let Some(icon) = place.icon {
            kit.draw_icon(pen, self.icon, icon, (if place.is_icon_lit { roles.text } else { colour }, ground));
        }
        if let Some(NamePlace { at, room }) = place.name {
            draw_field(kit, pen, &ruler.body, at, self.name, room, roles.text_dim);
        }
        if let Some(ValuePlace { at, scale }) = place.value {
            pen.text(None, &ruler.heading, at, self.value, colour, scale);
        }
        if let Some(row) = place.blocks {
            self.draw_blocks(pen, roles, row);
        }
        if let Some(Brackets { left, right, y }) = place.brackets {
            pen.text(None, &ruler.body, (left, y), OPEN, roles.text_dim, 1);
            pen.text(None, &ruler.body, (right, y), CLOSE, roles.text_dim, 1);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Sweep {
    pub(super) start: i32,
    pub(super) end: i32,
    pub(super) top: i32,
}

impl Sweep {
    pub(super) fn draw(self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, now_ns: u64, colour: Color) {
        let ruler = kit.ruler;
        let (block, low, _) = ruler.block();
        let travel = (self.end - self.start - block).max(1) as u64;
        let pixels = (u128::from(now_ns) * u128::from(SWEEP_PIXELS_PER_SECOND) / u128::from(NANOS_PER_SECOND)) as u64;
        let at = Rect::new(self.start + (pixels % travel) as i32, self.top, block, low);
        if at.y >= ruler.bar() {
            pen.fill(at, colour);
        }
    }
}

// The part of a scale's time that glows while it is set: its hours, its minutes, or its date from one byte to another.
#[derive(Clone, Copy)]
pub(super) enum Glow {
    Hour,
    Minute,
    Date((usize, usize)),
}

pub(super) struct Scale<'a> {
    pub(super) icon: &'a Icon,
    pub(super) ticks: &'a [i16],
    pub(super) mark: f32,
    pub(super) glow: Option<Glow>,
    pub(super) is_set: bool,
    pub(super) time: Option<&'a str>,
    pub(super) date: &'a str,
}

#[derive(Clone, Copy, Default)]
pub(super) struct ClockPlace {
    pub(super) card: Option<Rect>,
    pub(super) icon: Option<IconPlace>,
    pub(super) scale_top: i32,
    pub(super) scale_left: Option<i32>,
    pub(super) tick_gap: i32,
    pub(super) time_top: i32,
    pub(super) time_scale: i32,
    pub(super) time_left: Option<i32>,
}

impl ClockPlace {
    pub(super) fn tick_row(&self, ruler: &Ruler, ticks: i32) -> (i32, i32, i32) {
        let width = round(ruler.band / (PHI2 * PHI2));
        let row = ticks * (width + self.tick_gap) - self.tick_gap;
        (self.scale_left.unwrap_or(ruler.centre_left(row)), width, width + self.tick_gap)
    }
}

impl Scale<'_> {
    pub(super) fn draw(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, place: &ClockPlace) {
        let (ruler, roles) = (kit.ruler, &kit.roles);
        let ground = draw_ground(pen, roles, place.card);
        if let Some(icon) = place.icon {
            kit.draw_icon(pen, self.icon, icon, (roles.text, ground));
        }
        let (first, width, step) = place.tick_row(ruler, self.ticks.len() as i32);
        let (top, height) = (place.scale_top, ruler.tick_height());
        for (at, offset) in self.ticks.iter().enumerate() {
            let colour = if *offset == 0 { roles.text_dim } else { roles.rule };
            pen.fill(Rect::new(first + at as i32 * step, top, width, height), colour);
        }
        let mark = if self.glow.is_none() && self.is_set { roles.glow } else { roles.text };
        pen.fill(Rect::new(round(self.mark), top, width, height), mark);
        if let Some(time) = self.time {
            self.draw_time(pen, kit, place, time);
        }
    }

    fn draw_time(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, place: &ClockPlace, time: &str) {
        let (ruler, roles) = (kit.ruler, &kit.roles);
        let (scale, top, date) = (place.time_scale, place.time_top, self.date);
        let (cell, small_cell) = (ruler.cell() * scale, ruler.small_cell());
        let date_top = top + ruler.large() * scale;
        let left = place.time_left.unwrap_or(ruler.centre_left(time.len() as i32 * cell));
        let start = place.time_left.unwrap_or(ruler.centre_left(date.len() as i32 * small_cell));
        let is_time_part = matches!(self.glow, Some(Glow::Hour | Glow::Minute));
        let colour = if is_time_part { roles.text_dim } else { roles.text };
        pen.text(None, &ruler.heading, (left, top), time, colour, scale);
        pen.text(None, &ruler.body, (start, date_top), date, roles.text_dim, 1);
        let (hours, minutes) = time.split_once(':').unwrap_or((time, ""));
        match self.glow {
            Some(Glow::Hour) => {
                pen.text(None, &ruler.heading, (left, top), hours, roles.glow, scale);
            }
            Some(Glow::Minute) => {
                let x = left + (time.len() - minutes.len()) as i32 * cell;
                pen.text(None, &ruler.heading, (x, top), minutes, roles.glow, scale);
            }
            Some(Glow::Date((at, end))) => {
                if let Some(word) = date.get(at..end) {
                    pen.text(None, &ruler.body, (start + at as i32 * small_cell, date_top), word, roles.glow, 1);
                }
            }
            None => {}
        }
    }
}
