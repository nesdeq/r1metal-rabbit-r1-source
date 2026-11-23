use super::paint_row;
use crate::domain::paper::dates::to_edition_name;
use crate::domain::paper::rows::{self, Kind, Row};
use crate::domain::signal::pictures::Photo;
use crate::gfx::font::Font;
use crate::gfx::glyph;
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::kit::{Kit, draw_card};
use alloc::string::String;
use alloc::vec::Vec;

const SPEAKER_MARK: &str = "\u{203A} ";
const YOU: &str = "you\u{203A} ";
const DAY_RULE: &str = "\u{2500}\u{2500}";
const TRANSLATION_MARK: &str = "\u{203A}";
// The share of the room's width a card takes at most.
const CARD_SHARE: (i32, i32) = (5, 6);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum TalkStyle {
    Log,
    Sides,
    Cards,
    Named,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum LineKind {
    Day,
    You,
    Robot,
    Aside,
    Note,
    Meter,
    Cursor,
    Picture,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(in crate::ui) struct Line {
    pub(in crate::ui) kind: LineKind,
    row: Row,
    pub(in crate::ui) top: i32,
    is_opening: bool,
    // The file of the photo a line stands for, and how tall it stands.
    picture: Option<(String, i32)>,
}

pub(in crate::ui) fn to_speaker(name: &str) -> String {
    [name.to_lowercase().as_str(), SPEAKER_MARK].concat()
}

fn speaker_of(kind: LineKind, speaker: &str) -> &str {
    if matches!(kind, LineKind::You | LineKind::Note | LineKind::Cursor) { YOU } else { speaker }
}

#[derive(Clone, Copy)]
pub(in crate::ui) struct TalkForm<'a> {
    pub(in crate::ui) kit: Kit<'a>,
    pub(in crate::ui) style: TalkStyle,
}

impl TalkForm<'_> {
    pub(in crate::ui) fn speaker_width(&self, kind: LineKind, speaker: &str) -> i32 {
        self.kit.ruler.body.text_width(speaker_of(kind, speaker))
    }

    fn card(&self) -> (i32, i32) {
        (self.kit.ruler.gap, self.kit.ruler.under_bar().w * CARD_SHARE.0 / CARD_SHARE.1)
    }

    pub(in crate::ui) fn cells(&self, kind: LineKind, speaker: &str) -> (usize, usize) {
        let ruler = self.kit.ruler;
        let room = match self.style {
            TalkStyle::Log | TalkStyle::Sides => ruler.under_bar().w,
            TalkStyle::Cards => self.card().1 - 2 * self.card().0,
            TalkStyle::Named => ruler.under_bar().w - self.speaker_width(kind, speaker),
        };
        let cells = |font: &Font| (room / (font.width as i32).max(1)).max(1) as usize;
        (cells(&ruler.body), cells(&ruler.heading))
    }

    fn row_height(&self, row: &Row) -> i32 {
        row.kind.height(self.kit.ruler.small(), self.kit.ruler.large())
    }

    fn line_height(&self, line: &Line) -> i32 {
        line.picture.as_ref().map_or_else(|| self.row_height(&line.row), |(_, height)| *height)
    }

    fn row_width(&self, row: &Row) -> i32 {
        let ruler = self.kit.ruler;
        let step = if row.kind == Kind::Text { ruler.small_cell() } else { ruler.cell() };
        let letters: usize = row.spans.iter().map(|span| glyph::cells_of(&span.text)).sum();
        (row.indent + letters) as i32 * step
    }

    pub(in crate::ui) fn push(&self, lines: &mut Vec<Line>, top: &mut i32, kind: LineKind, row: Row) {
        let height = self.row_height(&row);
        let is_opening = row.kind != Kind::Gap && lines.last().is_none_or(|last| last.row.kind == Kind::Gap);
        lines.push(Line { kind, row, top: *top, is_opening, picture: None });
        *top += height;
    }

    // Adds a photo standing whole under the lines there are, by its file and how tall it stands, with a gap over it; `word` stands in its place until its picture is there.
    pub(in crate::ui) fn push_picture(
        &self,
        lines: &mut Vec<Line>,
        top: &mut i32,
        (file, word): (&str, &str),
        height: i32,
    ) {
        self.push(lines, top, LineKind::Picture, Row::gap());
        let line = Line {
            kind: LineKind::Picture,
            row: Row::plain(String::from(word), true),
            top: *top,
            is_opening: false,
            picture: Some((file.into(), height)),
        };
        lines.push(line);
        *top += height;
    }

    // Adds the row the microphone's level is drawn on, with a gap over it.
    pub(in crate::ui) fn push_meter(&self, lines: &mut Vec<Line>, top: &mut i32) {
        self.push(lines, top, LineKind::Robot, Row::gap());
        self.push(lines, top, LineKind::Meter, Row::plain(String::new(), false));
    }

    pub(in crate::ui) fn push_prompt(&self, lines: &mut Vec<Line>, top: &mut i32) {
        if self.style == TalkStyle::Named {
            self.push(lines, top, LineKind::Robot, Row::gap());
            self.push(lines, top, LineKind::Cursor, Row::plain(String::from(" "), false));
        }
    }

    pub(in crate::ui) fn lay(
        &self,
        text: &str,
        (kind, speaker): (LineKind, &str),
        is_stressed: bool,
        lines: &mut Vec<Line>,
        top: &mut i32,
    ) {
        self.push(lines, top, kind, Row::gap());
        for part in text.split('\n') {
            for line in rows::wrap_lines(part, self.cells(kind, speaker).0, 0) {
                self.push(lines, top, kind, Row::plain(line, is_stressed));
            }
        }
    }

    pub(in crate::ui) fn lay_day(&self, day: &str, lines: &mut Vec<Line>, top: &mut i32) {
        if !lines.is_empty() {
            self.push(lines, top, LineKind::Day, Row::gap());
        }
        self.push(lines, top, LineKind::Day, Row::plain(to_edition_name(day), false));
    }

    pub(in crate::ui) fn paint(
        &self,
        surface: &mut Surface<'_>,
        (lines, speaker, pictures): (&[Line], &str, &[Photo]),
        room: Rect,
        origin: i32,
        offset: i32,
    ) {
        let (kit, roles, style) = (&self.kit, self.kit.roles, self.style);
        let (body, cell) = (&kit.ruler.body, kit.ruler.small_cell());
        let (whole, plain) = (surface.bounds(), Lettering::default());
        if style == TalkStyle::Cards {
            self.paint_cards(surface, lines, room, origin - offset);
        }
        let pad = self.card().0;
        for line in lines {
            let y = origin + line.top - offset;
            if y + self.line_height(line) <= 0 {
                continue;
            }
            if y >= surface.height() {
                break;
            }
            if let Some((file, _)) = &line.picture {
                let picture = pictures.iter().find(|(held, _)| held == file).and_then(|(_, picture)| picture.as_ref());
                match picture {
                    Some(picture) => {
                        let x = room.x + (room.w - picture.width as i32) / 2;
                        surface.blit(whole, x, y, picture.width, &picture.words);
                    }
                    None => {
                        let left = room.x + (room.w - self.row_width(&line.row)) / 2;
                        let top = y + (self.line_height(line) - self.row_height(&line.row)) / 2;
                        paint_row(surface, kit, &line.row, (left, top, room.right()), |_| roles.content_dim);
                    }
                }
                continue;
            }
            let row_width = self.row_width(&line.row);
            let left = match (style, line.kind) {
                (TalkStyle::Sides | TalkStyle::Cards, LineKind::Day) => room.x + (room.w - row_width) / 2,
                (TalkStyle::Sides, LineKind::You | LineKind::Note) => room.right() - row_width,
                (TalkStyle::Cards, LineKind::You | LineKind::Note) => room.right() - row_width - pad,
                (TalkStyle::Cards, LineKind::Robot | LineKind::Aside) => room.x + pad,
                (TalkStyle::Named, LineKind::Day) => room.x + (DAY_RULE.chars().count() as i32 + 1) * cell,
                (TalkStyle::Named, kind) => room.x + self.speaker_width(kind, speaker),
                _ => room.x,
            };
            let is_speech = matches!(line.kind, LineKind::You | LineKind::Robot | LineKind::Cursor);
            if style == TalkStyle::Named && line.is_opening && is_speech {
                let colour = if line.kind == LineKind::Robot { roles.content } else { roles.content_dim };
                surface.text(whole, body, (room.x, y), speaker_of(line.kind, speaker), colour, 1, plain);
            }
            if style == TalkStyle::Named && line.kind == LineKind::Day && line.row.kind != Kind::Gap {
                surface.text(whole, body, (room.x, y), DAY_RULE, roles.rule, 1, plain);
                surface.text(whole, body, (left + row_width + cell, y), DAY_RULE, roles.rule, 1, plain);
            }
            if line.kind != LineKind::Meter {
                paint_row(surface, kit, &line.row, (left, y, room.right()), |span| match line.kind {
                    LineKind::Day => roles.text_dim,
                    LineKind::You if style == TalkStyle::Cards => roles.content,
                    LineKind::You | LineKind::Aside | LineKind::Note | LineKind::Cursor => roles.content_dim,
                    LineKind::Robot if line.row.is_marked(span) => roles.mark,
                    LineKind::Robot | LineKind::Meter | LineKind::Picture => roles.content,
                });
            }
        }
    }

    fn paint_cards(&self, surface: &mut Surface<'_>, lines: &[Line], room: Rect, top: i32) {
        let (pad, roles) = (self.card().0, self.kit.roles);
        let mut at = 0;
        while at < lines.len() {
            let kind = lines[at].kind;
            if !matches!(kind, LineKind::You | LineKind::Robot) || lines[at].row.kind == Kind::Gap {
                at += 1;
                continue;
            }
            let first = at;
            while at < lines.len() && lines[at].kind == kind && lines[at].row.kind != Kind::Gap {
                at += 1;
            }
            let rows = &lines[first..at];
            let (start, end) = (rows[0].top, rows[rows.len() - 1].top + self.row_height(&rows[rows.len() - 1].row));
            let (y, height) = (top + start - pad / 2, end - start + pad);
            if y + height <= 0 {
                continue;
            }
            if y >= surface.height() {
                break;
            }
            let width = rows.iter().map(|line| self.row_width(&line.row)).max().unwrap_or(0) + 2 * pad;
            let x = if kind == LineKind::You { room.right() - width } else { room.x };
            let colour = if kind == LineKind::You { roles.content_surface() } else { roles.surface() };
            draw_card(surface, Rect::new(x, y, width, height), colour);
        }
    }

    pub(in crate::ui) fn paint_remarks(
        &self,
        surface: &mut Surface<'_>,
        remarks: &[(String, bool)],
        (shift, offset): (i32, i32),
    ) {
        let (roles, ruler) = (self.kit.roles, self.kit.ruler);
        let (room, line, cell) = (ruler.under_bar(), ruler.small(), ruler.small_cell());
        let (whole, plain) = (surface.bounds(), Lettering::default());
        for (row, (text, is_translation)) in remarks.iter().enumerate() {
            let top = room.y + row as i32 * line - offset - shift;
            if top + line <= 0 {
                continue;
            }
            if top >= surface.height() {
                break;
            }
            let colour = if *is_translation { roles.content } else { roles.content_dim };
            let is_opening = *is_translation && row.checked_sub(1).is_none_or(|previous| !remarks[previous].1);
            match (self.style, *is_translation) {
                (TalkStyle::Sides, true) => {
                    let text = text.trim_start();
                    let left = room.right() - ruler.body.text_width(text);
                    surface.text(whole, &ruler.body, (left, top), text, colour, 1, plain);
                }
                (TalkStyle::Cards, true) => {
                    let width = ruler.body.text_width(text.trim_end());
                    surface.fill(Rect::new(room.x, top, width + cell, line), roles.content_surface());
                    surface.text(whole, &ruler.body, (room.x, top), text, colour, 1, plain);
                }
                (TalkStyle::Named, true) if is_opening => {
                    surface.text(whole, &ruler.body, (room.x, top), TRANSLATION_MARK, roles.text_dim, 1, plain);
                    surface.text(whole, &ruler.body, (room.x, top), text, colour, 1, plain);
                }
                _ => {
                    surface.text(whole, &ruler.body, (room.x, top), text, colour, 1, plain);
                }
            }
        }
    }
}
