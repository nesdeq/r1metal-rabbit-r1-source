use super::kit::{Kit, paint_picture};
use super::ruler::{Ruler, SENTENCE_LINES};
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::font::Font;
use crate::gfx::glyph;
use crate::gfx::key::Key;
use crate::gfx::pen::Pen;
use crate::gfx::target::Target;
use crate::gfx::wrap::{self, Rows, Step};
use crate::gfx::{Lettering, Picture, Rect, Surface};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

const TICK_REST_NS: u64 = 2_618_033_989;
const TICK_TRAVEL_NS: u64 = 1_618_033_989;
const CONNECTORS: [char; 7] = [',', ';', ':', '/', '+', '-', '&'];

pub(super) fn cells(room: i32, font: &Font) -> usize {
    (room / (font.width as i32).max(1)).max(0) as usize
}

pub(super) fn cut(text: &str, room: usize) -> &str {
    let Some(end) = glyph::overflow_at(text, room) else {
        return text;
    };
    let head = &text[..end];
    if !text.trim().contains(' ') {
        return head;
    }
    let word_end = |words: &str| words.trim_end_matches(|mark: char| mark == ' ' || CONNECTORS.contains(&mark)).len();
    let is_at_word_end = text[end..].starts_with(' ');
    let words = if is_at_word_end { head } else { head.rfind(' ').map_or(head, |at| &head[..at]) };
    let words = &words[..word_end(words)];
    match words.rfind(' ') {
        Some(at) if words[at + 1..].chars().count() == 1 => &words[..word_end(&words[..at])],
        _ => words,
    }
}

pub(super) fn sentence_lines(text: &str, room: usize, mut line: impl FnMut(usize, &str)) {
    let shape = Rows { width: room.max(1), first_indent: 0, indent: 0, is_empty_word_text: true };
    let mut run: Option<(usize, usize)> = None;
    wrap::walk(text.split_whitespace(), shape, |step| match step {
        Step::Piece { word, .. } => {
            let at = word.as_ptr() as usize - text.as_ptr() as usize;
            run = Some((run.map_or(at, |run| run.0), at + word.len()));
        }
        Step::RowEnd => {
            if let Some((start, end)) = run.take() {
                line(start, &text[start..end]);
            }
        }
    });
    if let Some((start, end)) = run {
        line(start, &text[start..end]);
    }
}

pub(super) fn sentence_count(text: &str, room: usize) -> usize {
    let mut lines = 0;
    sentence_lines(text, room, |_, _| lines += 1);
    lines
}

pub(super) fn draw_sentence(kit: &Kit<'_>, pen: &mut Pen<'_, '_>, text: &str, (x, top): (i32, i32), room: usize) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (mut y, mut lines) = (top, 0);
    sentence_lines(text, room, |_, line| {
        if lines < SENTENCE_LINES {
            pen.text(None, &ruler.body, (x, y), line, roles.content, 1);
            y += ruler.small();
        }
        lines += 1;
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TextSize {
    Large,
    Small,
    Scaled(i32),
}

impl TextSize {
    fn from_font(font: &Font, ruler: &Ruler) -> TextSize {
        if font.id == ruler.heading.id { TextSize::Large } else { TextSize::Small }
    }

    fn font(self, ruler: &Ruler) -> &Font {
        if self == TextSize::Small { &ruler.body } else { &ruler.heading }
    }

    const fn scale(self) -> i32 {
        if let TextSize::Scaled(scale) = self { scale } else { 1 }
    }
}

#[derive(Clone, Debug)]
struct Ticker {
    room: Rect,
    text: String,
    size: TextSize,
    colour: Color,
}

#[derive(Clone, Default, Debug)]
pub(super) struct Tickers {
    slots: Vec<Ticker>,
    count: usize,
}

impl Tickers {
    // The fields in use, in writing order.
    fn all(&self) -> &[Ticker] {
        &self.slots[..self.count]
    }

    fn push(&mut self, room: Rect, text: &str, size: TextSize, colour: Color) {
        match self.slots.get_mut(self.count) {
            Some(slot) => {
                slot.text.clear();
                slot.text.push_str(text);
                slot.room = room;
                slot.size = size;
                slot.colour = colour;
            }
            None => self.slots.push(Ticker { room, text: text.into(), size, colour }),
        }
        self.count += 1;
    }

    // Empties the list, keeping its slots.
    fn clear(&mut self) {
        self.count = 0;
    }
}

#[derive(Default)]
pub(super) struct TickerClock {
    tickers: RefCell<Tickers>,
    pub(super) start_ns: u64,
    pub(super) view: u64,
    pub(super) has_ticked: bool,
    pub(super) is_seen: Cell<bool>,
    pub(super) tick_ns: u64,
    pub(super) draw_ns: u64,
}

impl TickerClock {
    fn phase(&self, now_ns: u64) -> (u64, u64) {
        let (elapsed, period) = (now_ns.saturating_sub(self.start_ns), TICK_REST_NS + TICK_TRAVEL_NS);
        (elapsed / period, elapsed % period)
    }

    fn share(&self, now_ns: u64) -> f32 {
        let (leg, elapsed) = self.phase(now_ns);
        let travel = (elapsed.saturating_sub(TICK_REST_NS) as f32 / TICK_TRAVEL_NS as f32).min(1.0);
        let smooth = travel * travel * (3.0 - 2.0 * travel);
        if leg % 2 == 0 { smooth } else { 1.0 - smooth }
    }

    pub(super) fn is_moving(&self) -> bool {
        self.has_ticked && self.phase(self.tick_ns).1 >= TICK_REST_NS
    }

    pub(super) fn due(&self) -> Option<u64> {
        let (leg, elapsed) = self.phase(self.tick_ns);
        (self.has_ticked && elapsed < TICK_REST_NS)
            .then(|| self.start_ns + leg * (TICK_REST_NS + TICK_TRAVEL_NS) + TICK_REST_NS)
    }

    fn offset(&self, width: i32, room: i32) -> i32 {
        self.is_seen.set(true);
        round((width - room).max(0) as f32 * self.share(self.draw_ns))
    }
}

fn write_field<T: Target>(
    target: &mut T,
    (font, scale): (&Font, i32),
    (x, y): (i32, i32),
    text: &str,
    (room, within): (i32, Option<Rect>),
    colour: Color,
    tick: impl FnOnce(&mut T, Rect),
) -> i32 {
    if font.text_width(text) * scale <= room {
        return target.write(within, font, (x, y), text, colour, scale);
    }
    let room = Rect::new(x, y, room.max(0), font.height as i32 * scale);
    tick(target, room);
    room.right()
}

pub(super) fn paint_with_tickers(
    kit: &Kit<'_>,
    (words, mut slots): (Vec<u32>, Tickers),
    size: (usize, usize),
    paint: impl FnOnce(&mut Surface<'_>),
) -> (Picture, Tickers) {
    slots.clear();
    *kit.ticking.tickers.borrow_mut() = slots;
    let picture = paint_picture(words, kit.format, size, paint);
    (picture, core::mem::take(&mut *kit.ticking.tickers.borrow_mut()))
}

pub(super) fn paint_field(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    font: &Font,
    at: (i32, i32),
    text: &str,
    room: i32,
    colour: Color,
) -> i32 {
    write_field(plate, (font, 1), at, text, (room, None), colour, |_, room| {
        kit.ticking.tickers.borrow_mut().push(room, text, TextSize::from_font(font, kit.ruler), colour);
    })
}

pub(super) fn paint_scaled(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    scale: i32,
    at: (i32, i32),
    text: &str,
    room: i32,
    colour: Color,
) -> i32 {
    write_field(plate, (&kit.ruler.heading, scale), at, text, (room, None), colour, |_, room| {
        kit.ticking.tickers.borrow_mut().push(room, text, TextSize::Scaled(scale), colour);
    })
}

pub(super) fn paint_lines(
    kit: &Kit<'_>,
    plate: &mut Surface<'_>,
    (x, top): (i32, i32),
    (width, height): (i32, i32),
    text: &str,
    colour: Color,
) {
    let (body, small) = (&kit.ruler.body, kit.ruler.small());
    let room = cells(width, body);
    let max_lines = (height / small.max(1)).max(0) as usize;
    let count = sentence_count(text, room);
    let (mut line, mut y) = (0, top);
    sentence_lines(text, room, |start, row| {
        if line + 1 == max_lines && count > max_lines {
            plate.text(plate.bounds(), body, (x, y), cut(&text[start..], room), colour, 1, Lettering::default());
        } else if line < max_lines {
            plate.text(plate.bounds(), body, (x, y), row, colour, 1, Lettering::default());
            y += small;
        }
        line += 1;
    });
}

fn draw_ticking(
    kit: &Kit<'_>,
    target: &mut impl Target,
    size: TextSize,
    (room, within): (Rect, Rect),
    text: &str,
    colour: Color,
) {
    let Some(visible) = room.clip(within) else {
        return;
    };
    let (font, scale) = (size.font(kit.ruler), size.scale());
    let offset = kit.ticking.offset(font.text_width(text) * scale, room.w);
    target.write(Some(visible), font, (room.x - offset, room.y), text, colour, scale);
}

pub(super) fn draw_field(
    kit: &Kit<'_>,
    target: &mut impl Target,
    font: &Font,
    at: (i32, i32),
    text: &str,
    room: i32,
    colour: Color,
) {
    write_field(target, (font, 1), at, text, (room, None), colour, |target, room| {
        draw_ticking(kit, target, TextSize::from_font(font, kit.ruler), (room, room), text, colour);
    });
}

pub(super) fn draw_field_within(
    kit: &Kit<'_>,
    pen: &mut Pen<'_, '_>,
    at: (i32, i32),
    text: &str,
    room: i32,
    colour: Color,
    within: Rect,
) {
    write_field(pen, (&kit.ruler.body, 1), at, text, (room, Some(within)), colour, |pen, room| {
        draw_ticking(kit, pen, TextSize::Small, (room, within), text, colour);
    });
}

pub(super) fn draw_tickers(kit: &Kit<'_>, pen: &mut Pen<'_, '_>, tickers: &Tickers, (x, y): (i32, i32), within: Rect) {
    for ticker in tickers.all() {
        let room = Rect::new(x + ticker.room.x, y + ticker.room.y, ticker.room.w, ticker.room.h);
        draw_ticking(kit, pen, ticker.size, (room, within), &ticker.text, ticker.colour);
    }
}

pub(super) fn add_ticking_key(ticking: &TickerClock, key: Key, font: &Font, text: &str, room: i32) -> Key {
    let width = font.text_width(text);
    let key = key.bytes(text.as_bytes()).word(font.id).number(room);
    if width <= room { key } else { key.number(ticking.offset(width, room.max(0))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_at_words() {
        assert_eq!(cut("one two", 20), "one two", "what fits stays whole");
        assert_eq!(cut("one two three four", 9), "one two", "cut before the word that does not fit");
        assert_eq!(cut("abcdefghijklmnop", 9), "abcdefghi", "one word is cut where the room ends");
        assert_eq!(
            cut("https://example.com/a/long/path is here", 20),
            "https://example.com",
            "a first word that outgrows the room is cut too, not dropped"
        );
    }
}
