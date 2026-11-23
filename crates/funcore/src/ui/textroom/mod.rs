pub(super) mod talk;

use super::kit::{IconPlace, Kit, find_glyph};
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::domain::paper::markdown::{Blocks, Span};
use crate::domain::paper::rows::{self, Kind, Row};
use crate::gfx::icon::Icon;
use crate::gfx::measures::PHI;
use crate::gfx::motion::Edge;
use crate::gfx::pen::Pen;
use crate::gfx::store::{Placement, SOLE, SoleSpot, Store};
use crate::gfx::{Lettering, Picture, Rect, Surface};
use alloc::vec::Vec;
use core::ops::Range;
use talk::TalkStyle;

const MIC: &str = icon!("mic");
pub(super) const ICONS: [&str; 1] = [MIC];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct RoomStyle {
    pub(super) talk: TalkStyle,
    pub(super) is_at_foot: bool,
    pub(super) has_disc: bool,
    pub(super) has_head: bool,
}

#[derive(Default)]
pub(super) struct TextRows {
    rows: Vec<Row>,
    pub(super) tops: Vec<i32>,
    pub(super) height: i32,
    blocks: Vec<usize>,
    offset: usize,
    previous: Option<bool>,
}

impl TextRows {
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.tops.clear();
        self.blocks.clear();
        (self.offset, self.previous, self.height) = (0, None, 0);
    }

    pub(super) const fn is_done(&self, text: &str) -> bool {
        self.offset >= text.len()
    }

    // The block that stands at the top of a room scrolled to `at`: the first whose row begins there or below, else the last.
    pub(super) fn top_block(&self, at: i32) -> usize {
        let is_visible = |row: &usize| self.tops.get(*row).is_some_and(|top| *top >= at);
        self.blocks.iter().position(is_visible).unwrap_or(self.blocks.len().saturating_sub(1))
    }

    pub(super) fn add_more(
        &mut self,
        text: &str,
        (cells, heading_cells): (usize, usize),
        (line, heading): (i32, i32),
        target: i32,
    ) -> bool {
        let start_count = self.rows.len();
        let mut blocks = Blocks::new(text, self.offset);
        while self.height < target.max(1) {
            let Some((block, end)) = blocks.next() else {
                self.offset = text.len();
                break;
            };
            let first = self.rows.len();
            rows::lay_block(&block, self.previous, cells.max(1), heading_cells.max(1), &mut self.rows);
            self.blocks.push(first + usize::from(self.rows.get(first).is_some_and(|row| row.kind == Kind::Gap)));
            for number in first..self.rows.len() {
                self.tops.push(self.height);
                let is_before_heading =
                    self.rows.get(number + 1).is_some_and(|next| matches!(next.kind, Kind::Heading(_)));
                let is_gap = self.rows[number].kind == Kind::Gap;
                self.height +=
                    if is_gap && is_before_heading { line } else { self.rows[number].kind.height(line, heading) };
            }
            (self.offset, self.previous) = (end, Some(block.is_item()));
        }
        self.rows.len() > start_count
    }
}

fn scroll_band(words: &mut Vec<u32>, (width, height): (usize, usize), distance: Option<i32>) -> Range<usize> {
    match distance.filter(|distance| (distance.unsigned_abs() as usize) < height) {
        Some(distance) if distance > 0 => {
            words.copy_within(distance as usize * width.., 0);
            height - distance as usize..height
        }
        Some(distance) if distance < 0 => {
            let distance = distance.unsigned_abs() as usize;
            words.copy_within(..(height - distance) * width, distance * width);
            0..distance
        }
        _ => {
            words.clear();
            words.resize(width * height, 0);
            0..height
        }
    }
}

#[derive(Default)]
pub(super) struct RoomPlate<K> {
    room: Store<Picture>,
    pub(super) key: Option<K>,
    buffer: Vec<u32>,
    is_redraw: bool,
    landings: u32,
}

impl<K> RoomPlate<K> {
    pub(super) fn is_sized(&self, (width, height): (usize, usize)) -> bool {
        self.room.get(SOLE).is_some_and(|room| room.0.words.len() == width * height)
    }

    pub(super) fn make_room(&mut self, size: (usize, usize)) {
        if self.room.get(SOLE).is_none() {
            self.room.reserve(1);
        }
        if !self.is_sized(size) {
            self.reserve(size.0 * size.1);
        }
    }

    fn reserve(&mut self, words: usize) {
        if self.buffer.capacity() < words
            && let Some(spare) = self.room.spare(SOLE)
        {
            self.buffer = spare.words;
        }
        self.buffer.clear();
        self.buffer.reserve(words);
    }

    pub(super) const fn land(&mut self) {
        self.landings = self.landings.wrapping_add(1);
    }

    pub(super) const fn as_room(&self) -> RoomPicture<'_> {
        RoomPicture { room: &self.room, landings: self.landings }
    }
}

// What a room's plate does whatever its picture is kept by.
pub(super) trait Plate {
    fn renew(&mut self, size: (usize, usize), is_visible: bool);
    fn advance(&mut self, now_ns: u64);
    fn is_resting(&self) -> bool;
    fn set_cutting(&mut self, does_cut: bool);
}

impl<K> Plate for RoomPlate<K> {
    fn renew(&mut self, (width, height): (usize, usize), is_visible: bool) {
        self.key = None;
        if is_visible {
            self.is_redraw = true;
            self.reserve(width * height);
        }
    }

    fn advance(&mut self, now_ns: u64) {
        self.room.advance(now_ns, &SoleSpot(self.landings));
    }

    fn is_resting(&self) -> bool {
        self.room.is_resting()
    }

    fn set_cutting(&mut self, does_cut: bool) {
        if self.room.does_cut() != does_cut {
            self.room = if does_cut { Store::with_cuts() } else { Store::default() };
            self.key = None;
        }
    }
}

impl<K: PartialEq> RoomPlate<K> {
    pub(super) fn ready(
        &mut self,
        kit: &Kit<'_>,
        (key, distance, change_top): (K, Option<i32>, Option<usize>),
        paint: impl Fn(&mut Surface<'_>, i32),
    ) {
        let (room, (width, height)) = (kit.ruler.room(), kit.ruler.room_size());
        let (has_picture, is_sized, change_top) =
            (self.room.get(SOLE).is_some(), self.is_sized((width, height)), change_top.unwrap_or(height));
        if self.key.as_ref() == Some(&key) && is_sized && change_top >= height {
            return;
        }
        let ground = kit.format.pack(kit.roles.background);
        if self.is_redraw || !is_sized {
            let mut words = core::mem::take(&mut self.buffer);
            words.clear();
            words.resize(width * height, ground);
            if let Some(mut plate) = Surface::new(&mut words, width as u32, height as u32, width, kit.format) {
                paint(&mut plate, room.y);
            }
            let (sole, picture) = (SoleSpot(self.landings), Picture { words, width });
            let is_whole = !(core::mem::take(&mut self.is_redraw) && has_picture);
            self.room.put(kit.ids, &sole, (SOLE, None), picture, is_whole);
        } else if let Some(picture) = self.room.repaint(kit.ids, SOLE) {
            let new_rows = if distance == Some(0) {
                height..height
            } else {
                scroll_band(&mut picture.words, (width, height), distance)
            };
            let low = if new_rows.end == height { new_rows.start.min(change_top) } else { change_top };
            let high = if new_rows.start == 0 && new_rows.end < height { new_rows.end.min(low) } else { 0 };
            for band in [0..high, low..height].into_iter().filter(|band| !band.is_empty()) {
                let rows = &mut picture.words[band.start * width..band.end * width];
                rows.fill(ground);
                if let Some(mut plate) = Surface::new(rows, width as u32, band.len() as u32, width, kit.format) {
                    paint(&mut plate, room.y + band.start as i32);
                }
            }
        }
        self.key = Some(key);
    }
}

#[derive(Clone, Copy)]
pub(super) struct RoomPicture<'a> {
    room: &'a Store<Picture>,
    landings: u32,
}

impl RoomPicture<'_> {
    pub(super) fn draw(self, pen: &mut Pen<'_, '_>, room: Rect) {
        self.room.draw(pen, (SOLE, Placement::Shows(self.landings)), (room, room), |_, _, _, _| {});
    }
}

#[derive(Default)]
pub(super) struct TextRoom<K> {
    pub(super) at: i32,
    pub(super) scroll: Edge,
    pub(super) plate: RoomPlate<K>,
}

impl<K> TextRoom<K> {
    pub(super) fn scroll_parts(&mut self) -> (&mut i32, &mut Edge) {
        (&mut self.at, &mut self.scroll)
    }
}

pub(super) fn max_scroll(height: i32, room: Rect) -> i32 {
    (height - room.h).max(0)
}

pub(super) fn draw_scroll_mark(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, (room, height, offset): (Rect, i32, i32)) {
    if height <= room.h {
        return;
    }
    let width = kit.ruler.lines().0;
    let (height, room_height) = (height as f32, room.h as f32);
    let (top, length) = (round(room_height * offset.max(0) as f32 / height), round(room_height * room_height / height));
    let left = room.right() + (kit.ruler.gap - width) / 2;
    pen.fill(Rect::new(left, room.y + top, width, length.min(room.h - top)), kit.roles.text_dim);
}

pub(super) fn draw_disc(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, (icon, disc): (&Icon, Rect), is_lit: bool) {
    let roles = kit.roles;
    let colour = if is_lit { roles.glow } else { roles.rule };
    let (x, y, side) = (disc.x as f32, disc.y as f32, disc.w as f32);
    kit.draw_icon(pen, icon, IconPlace { left: x, top: y, side }, (colour, roles.background));
    if let Some(mic) = find_glyph(kit.glyphs, MIC) {
        let mic_side = side / PHI;
        let place = IconPlace { left: x + (side - mic_side) / 2.0, top: y + (side - mic_side) / 2.0, side: mic_side };
        kit.draw_icon(pen, mic, place, (roles.text, colour));
    }
}

fn paint_row(
    surface: &mut Surface<'_>,
    kit: &Kit<'_>,
    row: &Row,
    (left, y, right): (i32, i32, i32),
    colour: impl Fn(&Span) -> Color,
) {
    let ruler = kit.ruler;
    match row.kind {
        Kind::Gap => {}
        Kind::Rule => {
            let (height, line) = (ruler.lines().0, ruler.small());
            surface.fill(Rect::new(left, y + (line / 2 - height) / 2, right - left, height), kit.roles.mark);
        }
        Kind::Heading(_) | Kind::Text => {
            let font = if row.kind == Kind::Text { &ruler.body } else { &ruler.heading };
            let mut x = left + row.indent as i32 * font.width as i32;
            for span in &row.spans {
                let lettering = Lettering { is_strong: span.is_strong, is_stressed: span.is_stressed };
                x = surface.text(surface.bounds(), font, (x, y), &span.text, colour(span), 1, lettering);
            }
        }
    }
}

pub(super) fn paint_rows(
    surface: &mut Surface<'_>,
    kit: &Kit<'_>,
    text_rows: &TextRows,
    first: i32,
    (shift, offset): (i32, i32),
) {
    let (room, height) = (kit.ruler.under_bar(), kit.ruler.large());
    let (mark, content) = (kit.roles.mark, kit.roles.content);
    for (row, top) in text_rows.rows.iter().zip(&text_rows.tops) {
        let y = first + top - offset - shift;
        if y + height <= room.y - shift {
            continue;
        }
        if y >= surface.height() {
            break;
        }
        paint_row(
            surface,
            kit,
            row,
            (room.x, y, room.right()),
            |span| if row.is_marked(span) { mark } else { content },
        );
    }
}

pub(super) fn paint_artwork_over(
    surface: &mut Surface<'_>,
    kit: &Kit<'_>,
    (artwork, height): (Option<&Picture>, i32),
    text_rows: &TextRows,
    (shift, offset): (i32, i32),
) {
    let room = kit.ruler.under_bar();
    if let Some(picture) = artwork {
        let x = room.x + (room.w - picture.width as i32) / 2;
        surface.blit(surface.bounds(), x, room.y - offset - shift, picture.width, &picture.words);
    }
    paint_rows(surface, kit, text_rows, room.y + height, (shift, offset));
}
