//! Documents on PDF pages in the interface's look: an entry's summary, a rundown, the manual.

use super::markdown;
use super::pdf::Pdf;
use super::rows::{self, Kind, Row};
use super::sheet::Subject;
use crate::base::math::round;
use crate::base::time::Moment;
use crate::gfx::Lettering;
use crate::gfx::font::{spleen_12x24, spleen_16x32};
use crate::gfx::glyph;
use crate::gfx::icon::{Icon, Point};
use crate::gfx::measures;
use crate::gfx::palette::Roles;
use alloc::format;
use alloc::vec::Vec;

const PAGE_POINTS: (f32, f32) = (595.276, 841.89);
const POINTS_PER_PIXEL: f32 = 0.6;
const MARGIN_GAPS: i32 = 8;
const ROBOT: &str = include_str!("../../../assets/icons/robot.svg");

fn lay_on_pages(rows: &[Row], (line, large): (i32, i32), top: i32, bottom: i32) -> Vec<Vec<(i32, &Row)>> {
    let height = |row: &Row| row.kind.height(line, large);
    let is_head = |row: &Row| matches!(row.kind, Kind::Heading(_) | Kind::Gap);
    let (mut pages, mut page, mut y) = (Vec::new(), Vec::new(), top);
    for (index, row) in rows.iter().enumerate() {
        let next_height: i32 = if matches!(row.kind, Kind::Heading(_)) {
            let rest = &rows[index + 1..];
            let end = rest.iter().position(|row| !is_head(row)).map_or(rest.len(), |text| text + 1);
            rest[..end].iter().map(height).sum()
        } else {
            0
        };
        if !page.is_empty() && y + height(row) + next_height > bottom {
            pages.push(core::mem::take(&mut page));
            y = top;
        }
        if !page.is_empty() || row.kind != Kind::Gap {
            page.push((y, row));
            y += height(row);
        }
    }
    pages.push(page);
    pages
}

fn place_robot(band: f32, width: f32, margin: i32, gap: i32) -> Option<Vec<Vec<(f32, f32)>>> {
    let robot = Icon::from_svg(ROBOT).ok()?;
    let (low, high) = robot.extent()?;
    let scale = measures::visible(band) / (high.y - low.y);
    let left = width - margin as f32 - (high.x - low.x) * scale;
    let top = (margin + gap) as f32;
    let place = |point: &Point| (left + (point.x - low.x) * scale, top + (point.y - low.y) * scale);
    Some(robot.contours().iter().map(|outline| outline.iter().map(place).collect()).collect())
}

/// What a document's pages tell beside its text: its title, the bar's name and the line under it, and the file its footer names.
#[derive(Clone, Copy, Debug)]
pub struct Head<'a> {
    pub title: &'a str,
    pub name: &'a str,
    pub line: &'a str,
    pub source: &'a str,
}

/// A summary or a rundown as a PDF file; `created` is local time and minutes ahead of UTC.
pub fn to_pages(subject: Subject<'_>, text: &str, created: Option<(u64, i16)>, roles: &Roles) -> Vec<u8> {
    let (title, name) = (subject.to_title(), subject.to_bar_name());
    let (line, source) = (subject.to_bar_line(), subject.to_source_file());
    to_document(Head { title: &title, name: &name, line: &line, source: &source }, text, created, roles)
}

/// A document of Markdown as a PDF file; `created` is local time and minutes ahead of UTC.
pub fn to_document(head: Head<'_>, text: &str, created: Option<(u64, i16)>, roles: &Roles) -> Vec<u8> {
    let (heading, small_font) = (spleen_16x32(), spleen_12x24());
    let (band, gap) = measures::measure(&heading, &small_font);
    let bar = measures::bar(band, gap);
    let (cell, line) = (small_font.width as i32, small_font.height as i32);
    let (large_cell, large_line) = (heading.width as i32, heading.height as i32);
    let (width, height) = (PAGE_POINTS.0 / POINTS_PER_PIXEL, PAGE_POINTS.1 / POINTS_PER_PIXEL);
    let margin = MARGIN_GAPS * gap;
    let rule_height = measures::thin_line(band);
    let text_width = round(width) - 2 * margin;
    let rows = rows::lay_out(&markdown::parse(text), (text_width / cell) as usize, (text_width / large_cell) as usize);
    let (top, foot) = (margin + bar + gap, round(height) - margin - line);
    let pages = lay_on_pages(&rows, (line, large_line), top, foot - gap);
    let robot = place_robot(band, width, margin, gap);
    let mut pdf = Pdf::new(width, height, POINTS_PER_PIXEL);
    let (large, small) = (pdf.add_face(heading), pdf.add_face(small_font));
    let at = |x: i32, y: i32| (x as f32, y as f32);
    for (index, page) in pages.iter().enumerate() {
        pdf.add_page();
        pdf.fill_rect(roles.background, (0.0, 0.0, width, height));
        let name_y = margin + (bar - large_line - line) / 2;
        pdf.add_text(large, roles.text, at(margin, name_y), head.name);
        pdf.add_text(small, roles.text_dim, at(margin, name_y + large_line), head.line);
        pdf.fill_shape(roles.robot, robot.as_deref().unwrap_or_default());
        for &(y, row) in page {
            match row.kind {
                Kind::Gap => {}
                Kind::Rule => {
                    let rule_y = y + (line / 2 - rule_height) / 2;
                    pdf.fill_rect(roles.mark, (margin as f32, rule_y as f32, text_width as f32, rule_height as f32));
                }
                Kind::Heading(_) | Kind::Text => {
                    let is_text = row.kind == Kind::Text;
                    let (face, step) = if is_text { (small, cell) } else { (large, large_cell) };
                    let mut left = margin + row.indent as i32 * step;
                    for span in &row.spans {
                        let lettering = Lettering { is_strong: span.is_strong, is_stressed: span.is_stressed };
                        let variant = pdf.variant_of(face, lettering);
                        let colour = if row.is_marked(span) { roles.mark } else { roles.content };
                        pdf.add_text(variant, colour, at(left, y), &span.text);
                        left += glyph::cells_of(&span.text) as i32 * step;
                    }
                }
            }
        }
        let label = format!("{} of {}", index + 1, pages.len());
        pdf.add_text(small, roles.text_dim, at(margin, foot), head.source);
        pdf.add_text(small, roles.text_dim, at(round(width) - margin - label.len() as i32 * cell, foot), &label);
    }
    let created = created.map(|(local, offset)| (Moment::from_seconds(local), offset));
    pdf.into_file(head.title, created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::pixel::Color;
    use crate::gfx::palette::{PALETTES, STANDARD};
    use alloc::string::String;

    const STEM: &str = "20261012_190328";
    const NOTE: Subject<'static> = Subject::Note(STEM, 42);
    const ROLES: Roles = Roles::of(&PALETTES[STANDARD]);

    #[test]
    fn heading_stays_with_its_text() {
        let paragraph = "Words that fill one row.\n\n";
        let rows_of = |text: &str| rows::lay_out(&markdown::parse(text), 24, 24);
        let page = |rows: &[Row]| -> Vec<Vec<Kind>> {
            lay_on_pages(rows, (10, 20), 0, 100)
                .iter()
                .map(|page| page.iter().map(|(_, row)| row.kind).collect())
                .collect()
        };
        let text = [paragraph.repeat(4).as_str(), "## A heading of two rows here\n\nIt goes on.\n"].concat();
        let pages = page(&rows_of(&text));
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1], [Kind::Heading(2), Kind::Heading(2), Kind::Gap, Kind::Text]);
        let one_page =
            page(&rows_of(&[paragraph.repeat(3).as_str(), "## A heading of two rows here\n\nIt goes on.\n"].concat()));
        assert_eq!(one_page.len(), 1);
    }

    fn long_summary() -> String {
        let section = "## Decided on the **first** day\n\nThe plan for K\u{E4}se and Br\u{F6}tchen stands, and \u{201C}nothing\u{201D} of it changes \u{2013} says **Anna**, who has the last word here.\n\n- Go ahead with it as it was laid out on Friday by everyone who was there\n- **Ben** calls the bakery\n  1. first thing in the morning\n\n---\n\n";
        core::iter::repeat_n(section, 14).collect()
    }

    #[test]
    fn page_bar_and_foot() {
        let file = to_pages(NOTE, &long_summary(), Some((1_791_833_600, 120)), &ROLES);
        let text = String::from_utf8_lossy(&file);
        let count = |pattern: &str| text.matches(pattern).count();
        assert!(file.starts_with(b"%PDF-1.4\n") && text.contains("/MediaBox [0 0 595.28 841.89]"));
        assert_eq!(count("/Type /Page /Parent"), 3);
        assert_eq!(count("0.000 0.000 0.000 rg\n0.00 0.00 595.28 841.89 re f\n"), 3);
        assert_eq!(count("h\nf\n"), 3);
        assert!(count("BT /F0 19.20 Tf 38.40 ") >= 3);
        assert!(text.contains("/CreationDate (D:20261012193320+02'00')"));
        let short = to_pages(NOTE, "", None, &ROLES);
        assert_eq!(String::from_utf8_lossy(&short).matches("/Type /Page /Parent").count(), 1);
    }

    #[test]
    fn spans_follow_by_cells() {
        let file = to_pages(NOTE, "**\u{65E5}\u{672C}** x", None, &ROLES);
        let text = String::from_utf8_lossy(&file);
        // Where the run of four unknown characters is set across the page, and the run set after it.
        let mut runs =
            text.lines().filter(|line| line.starts_with("BT ")).skip_while(|line| !line.contains("<20202020> Tj"));
        let mut set_at = || runs.next()?.split(' ').nth(4)?.parse::<f32>().ok();
        let (wide, after) = (set_at(), set_at());
        let cell = spleen_12x24().width as f32 * POINTS_PER_PIXEL;
        assert_eq!(
            wide.zip(after).map(|(wide, after)| round((after - wide) / cell)),
            Some(4),
            "two characters of two cells each are four unknown ones, and what follows stands four cells on"
        );
    }

    fn to_rgb_text(colour: Color) -> String {
        let share = |channel: u8| f32::from(channel) / 255.0;
        format!("{:.3} {:.3} {:.3}", share(colour.r()), share(colour.g()), share(colour.b()))
    }

    #[test]
    fn palette_colours() {
        let green = Roles::of(&PALETTES[14]);
        let file = to_pages(NOTE, &long_summary(), None, &green);
        let text = String::from_utf8_lossy(&file);
        let count = |colour| text.matches(&format!("{} rg\n", to_rgb_text(colour))).count();
        assert!([green.text, green.text_dim, green.content, green.mark].iter().all(|colour| count(*colour) > 0));
        assert_eq!([ROLES.text, ROLES.text_dim, ROLES.mark].map(count), [0, 0, 0]);
    }
}
