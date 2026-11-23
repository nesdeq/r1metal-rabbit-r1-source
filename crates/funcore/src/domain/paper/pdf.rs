use crate::base::pixel::Color;
use crate::base::time::MINUTES_PER_HOUR;
use crate::base::time::Moment;
use crate::gfx::Lettering;
use crate::gfx::font::Font;
use crate::gfx::font::LEAN;
use crate::gfx::glyph;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const HEAD: &[u8] = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n";
const CATALOG: usize = 1;
const PAGES: usize = 2;
const INFO: usize = 3;
const FIRST_CODE: usize = 0x20;
const CODES: usize = 0x100 - FIRST_CODE;
const CODES_PER_BLOCK: usize = 100;
const CHANNEL_MAX: f32 = 255.0;
const PRODUCER: &str = "r1metal";

const UNKNOWN: char = '?';

struct Face {
    font: Font,
    characters: Vec<char>,
    base: usize,
    lettering: Lettering,
}

impl Face {
    fn new(font: Font, base: usize, lettering: Lettering) -> Face {
        Face { font, characters: Vec::from([UNKNOWN]), base, lettering }
    }

    fn code(&mut self, character: char) -> u8 {
        let character = if self.font.can_draw(character) { character } else { UNKNOWN };
        let position = self.characters.iter().position(|other| *other == character);
        let index = position.or_else(|| {
            (self.characters.len() < CODES).then(|| {
                self.characters.push(character);
                self.characters.len() - 1
            })
        });
        (FIRST_CODE + index.unwrap_or(0)) as u8
    }
}

fn to_rectangles(font: &Font, character: char) -> Vec<(u32, u32, u32, u32)> {
    let (width, height, row_bytes) = (font.width, font.height, font.row_bytes.max(1));
    let is_lit = |row: &[u8], column: u32| row[column as usize / 8] & (0x80 >> (column % 8)) != 0;
    // Runs that go on from the row above: left, right, and the row they began in.
    let mut open: Vec<(u32, u32, u32)> = Vec::new();
    let mut rectangles = Vec::new();
    let glyph = font.glyph(character);
    for line in 0..=height {
        let row = glyph.chunks(row_bytes).nth(line as usize).filter(|_| line < height);
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut column = 0;
        while let Some(row) = row.filter(|_| column < width) {
            if !is_lit(row, column) {
                column += 1;
                continue;
            }
            let left = column;
            while column < width && is_lit(row, column) {
                column += 1;
            }
            runs.push((left, column));
        }
        // A run the row above had too goes on; one it does not have ends there, as a rectangle.
        open.retain(|&(left, right, top)| {
            let goes_on = runs.contains(&(left, right));
            if !goes_on {
                rectangles.push((left, height - line, right - left, line - top));
            }
            goes_on
        });
        for (left, right) in runs {
            if !open.iter().any(|&(start, end, _)| (start, end) == (left, right)) {
                open.push((left, right, line));
            }
        }
    }
    rectangles
}

pub(super) struct Pdf {
    size: (f32, f32),
    scale: f32,
    faces: Vec<Face>,
    pages: Vec<String>,
}

impl Pdf {
    pub(super) fn new(width: f32, height: f32, scale: f32) -> Pdf {
        Pdf { size: (width * scale, height * scale), scale, faces: Vec::new(), pages: Vec::new() }
    }

    pub(super) fn add_face(&mut self, font: Font) -> usize {
        let number = self.faces.len();
        self.faces.push(Face::new(font, number, Lettering::default()));
        number
    }

    pub(super) fn variant_of(&mut self, face: usize, lettering: Lettering) -> usize {
        let base = self.faces[face].base;
        let existing = self.faces.iter().position(|other| (other.base, other.lettering) == (base, lettering));
        existing.unwrap_or_else(|| {
            let font = self.faces[base].font.clone();
            self.faces.push(Face::new(font, base, lettering));
            self.faces.len() - 1
        })
    }

    pub(super) fn add_page(&mut self) {
        self.pages.push(String::new());
    }

    fn set_colour(&mut self, colour: Color) -> Option<&mut String> {
        let page = self.pages.last_mut()?;
        let share = |channel: u8| f32::from(channel) / CHANNEL_MAX;
        let _ = writeln!(page, "{:.3} {:.3} {:.3} rg", share(colour.r()), share(colour.g()), share(colour.b()));
        Some(page)
    }

    fn to_point(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale, self.size.1 - y * self.scale)
    }

    pub(super) fn fill_rect(&mut self, colour: Color, (left, top, width, height): (f32, f32, f32, f32)) {
        let ((x, y), scale) = (self.to_point(left, top + height), self.scale);
        if let Some(page) = self.set_colour(colour) {
            let _ = writeln!(page, "{x:.2} {y:.2} {:.2} {:.2} re f", width * scale, height * scale);
        }
    }

    pub(super) fn fill_shape(&mut self, colour: Color, outlines: &[Vec<(f32, f32)>]) {
        let points: Vec<Vec<(f32, f32)>> =
            outlines.iter().map(|outline| outline.iter().map(|&(x, y)| self.to_point(x, y)).collect()).collect();
        let Some(page) = self.set_colour(colour) else {
            return;
        };
        for outline in points.iter().filter(|outline| !outline.is_empty()) {
            for (number, (x, y)) in outline.iter().enumerate() {
                let _ = writeln!(page, "{x:.2} {y:.2} {}", if number == 0 { 'm' } else { 'l' });
            }
            page.push_str("h\n");
        }
        page.push_str("f\n");
    }

    pub(super) fn add_text(&mut self, face: usize, colour: Color, (x, y): (f32, f32), text: &str) {
        let Some(entry) = self.faces.get_mut(face).filter(|_| !self.pages.is_empty() && !text.is_empty()) else {
            return;
        };
        let height = entry.font.height as f32;
        // Each run of characters takes the cells the screen gives what it draws as, by which the rows were measured: a letter its own, what is drawn as nothing none, and a drawing or an emoji, which no face here draws, the unknown character in each of its cells.
        let mut codes = String::new();
        for (letter, cells) in glyph::letters(text) {
            let code = entry.code(letter.unwrap_or(UNKNOWN));
            for _ in 0..cells {
                let _ = write!(codes, "{code:02X}");
            }
        }
        let ((x, y), size) = (self.to_point(x, y + height), height * self.scale);
        if let Some(page) = self.set_colour(colour) {
            let _ = writeln!(page, "BT /F{face} {size:.2} Tf {x:.2} {y:.2} Td <{codes}> Tj ET");
        }
    }

    pub(super) fn into_file(self, title: &str, created: Option<(Moment, i16)>) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = alloc::vec![Vec::new(); INFO];
        let stream = |data: &[u8]| {
            [format!("<< /Length {} >>\nstream\n", data.len()).as_bytes(), data, &b"\nendstream"[..]].concat()
        };
        let mut fonts = String::new();
        for (number, face) in self.faces.iter().enumerate() {
            let (width, height, first) = (face.font.width, face.font.height, objects.len() + 1);
            let _ = write!(fonts, "/F{number} {first} 0 R ");
            // The font, the table of its characters, and behind them what draws each character.
            let (table, glyphs) = (first + 1, first + 2);
            let names: String = (0..face.characters.len()).map(|index| format!("/g{index} ")).collect();
            let procedures: String =
                (0..face.characters.len()).map(|index| format!("/g{index} {} 0 R ", glyphs + index)).collect();
            let widths: String = face.characters.iter().map(|_| format!("{width} ")).collect();
            let last = FIRST_CODE + face.characters.len() - 1;
            // A strong face has every stroke a pixel wider; a stressed one leans about the middle of its height, as the screen's does.
            let (extra_width, size) = (u32::from(face.lettering.is_strong), 1.0 / height as f32);
            let lean = if face.lettering.is_stressed {
                format!("{:.6} {size:.6} {:.6}", size / LEAN as f32, -0.5 / LEAN as f32)
            } else {
                format!("0 {size:.6} 0")
            };
            objects.push(
                format!(
                    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 {} {height}] /FontMatrix [{size:.6} 0 {lean} 0] /CharProcs << {procedures}>> /Encoding << /Type /Encoding /Differences [{FIRST_CODE} {names}] >> /FirstChar {FIRST_CODE} /LastChar {last} /Widths [{widths}] /Resources << /ProcSet [/PDF] >> /ToUnicode {table} 0 R >>",
                    width + extra_width
                )
                .into_bytes(),
            );
            objects.push(stream(to_unicode_map(&face.characters).as_bytes()));
            for &character in &face.characters {
                let mut glyph = format!("{width} 0 0 0 {} {height} d1\n", width + extra_width);
                let rectangles = to_rectangles(&face.font, character);
                for (left, bottom, box_width, box_height) in &rectangles {
                    let _ = writeln!(glyph, "{left} {bottom} {} {box_height} re", box_width + extra_width);
                }
                glyph.push_str(if rectangles.is_empty() { "" } else { "f" });
                objects.push(stream(glyph.as_bytes()));
            }
        }
        let mut kids = String::new();
        for page in &self.pages {
            let number = objects.len() + 1;
            let _ = write!(kids, "{number} 0 R ");
            objects.push(format!("<< /Type /Page /Parent {PAGES} 0 R /Contents {} 0 R >>", number + 1).into_bytes());
            objects.push(stream(page.as_bytes()));
        }
        objects[CATALOG - 1] = format!("<< /Type /Catalog /Pages {PAGES} 0 R >>").into_bytes();
        objects[PAGES - 1] = format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << {fonts}>> >> >>",
            self.pages.len(),
            self.size.0,
            self.size.1
        )
        .into_bytes();
        let title_text: String = title.encode_utf16().map(|unit| format!("{unit:04X}")).collect();
        let date_entry = created.map_or_else(String::new, |(moment, offset)| {
            let (sign, minutes) = (if offset < 0 { '-' } else { '+' }, u64::from(offset.unsigned_abs()));
            format!(
                " /CreationDate (D:{:04}{:02}{:02}{:02}{:02}{:02}{sign}{:02}'{:02}')",
                moment.year,
                moment.month,
                moment.day,
                moment.hour,
                moment.minute,
                moment.second,
                minutes / MINUTES_PER_HOUR,
                minutes % MINUTES_PER_HOUR
            )
        });
        objects[INFO - 1] = format!("<< /Title <FEFF{title_text}> /Producer ({PRODUCER}){date_entry} >>").into_bytes();
        let mut file = HEAD.to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (number, object) in objects.iter().enumerate() {
            offsets.push(file.len());
            file.extend(format!("{} 0 obj\n", number + 1).as_bytes());
            file.extend(object);
            file.extend(b"\nendobj\n");
        }
        // The table of where each object lies, every line of it twenty bytes, and where that table lies.
        let table = file.len();
        file.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        offsets.iter().for_each(|offset| file.extend(format!("{offset:010} 00000 n \n").as_bytes()));
        file.extend(
            format!(
                "trailer\n<< /Size {} /Root {CATALOG} 0 R /Info {INFO} 0 R >>\nstartxref\n{table}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        file
    }
}

fn to_unicode_map(characters: &[char]) -> String {
    let mut table = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for (part, chunk) in characters.chunks(CODES_PER_BLOCK).enumerate() {
        let _ = writeln!(table, "{} beginbfchar", chunk.len());
        for (index, character) in chunk.iter().enumerate() {
            let mut units = [0u16; 2];
            let utf16: String = character.encode_utf16(&mut units).iter().map(|unit| format!("{unit:04X}")).collect();
            let _ = writeln!(table, "<{:02X}> <{utf16}>", FIRST_CODE + part * CODES_PER_BLOCK + index);
        }
        table.push_str("endbfchar\n");
    }
    table.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend");
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::font::spleen_12x24;
    use crate::gfx::font::spleen_16x32;

    fn count(file: &[u8], pattern: &str) -> usize {
        file.windows(pattern.len()).filter(|window| *window == pattern.as_bytes()).count()
    }

    #[test]
    fn character_rectangles() {
        let font = spleen_12x24();
        for character in ['A', 'g', '?', '\u{E4}', ' ', '-', '#'] {
            let mut covered = alloc::vec![0u8; 12 * 24];
            for (left, bottom, width, height) in to_rectangles(&font, character) {
                assert!(
                    width > 0 && height > 0 && left + width <= 12 && bottom + height <= 24,
                    "{character:?} in its cell"
                );
                for y in bottom..bottom + height {
                    for x in left..left + width {
                        covered[((23 - y) * 12 + x) as usize] += 1;
                    }
                }
            }
            let lit: Vec<u8> = font
                .glyph(character)
                .chunks(font.row_bytes)
                .flat_map(|row| (0..12).map(move |column| u8::from(row[column / 8] & (0x80 >> (column % 8)) != 0)))
                .collect();
            assert_eq!(covered, lit, "{character:?}: every lit pixel once and no other");
        }
        assert!(to_rectangles(&font, ' ').is_empty());
        assert_eq!(to_rectangles(&font, '-').len(), 1, "a dash is one rectangle: rows that are the same go together");
        assert!(to_rectangles(&font, 'A').len() < 24, "fewer rectangles than runs of pixels");
    }

    #[test]
    fn file_structure() {
        let mut pdf = Pdf::new(100.0, 200.0, 0.5);
        let (body, heading) = (pdf.add_face(spleen_12x24()), pdf.add_face(spleen_16x32()));
        pdf.fill_rect(Color(0xFF_0000), (0.0, 0.0, 1.0, 1.0));
        pdf.add_text(body, Color(0), (0.0, 0.0), "before a page");
        assert_eq!(pdf.pages.len(), 0, "nothing is drawn before a page is begun");
        pdf.add_page();
        pdf.fill_rect(Color(0x00_0000), (0.0, 0.0, 100.0, 200.0));
        pdf.fill_shape(Color(0xFF_B000), &[alloc::vec![(10.0, 10.0), (20.0, 10.0), (20.0, 30.0)], Vec::new()]);
        pdf.add_text(body, Color(0xDF_9A00), (8.0, 40.0), "K\u{E4}se, Kasse");
        pdf.add_text(heading, Color(0xDF_9A00), (8.0, 80.0), "NOTE");
        pdf.add_page();
        pdf.add_text(body, Color(0x8A_5F00), (8.0, 8.0), "2 of 2");
        let file = pdf.into_file("K\u{E4}se", Some((Moment::from_seconds(1_747_392_631), -210)));
        let text = String::from_utf8_lossy(&file);
        assert!(file.starts_with(b"%PDF-1.4\n") && file.ends_with(b"%%EOF\n"));
        assert!(text.contains("/Count 2 /MediaBox [0 0 50.00 100.00]"), "pages of 50 by 100 points");
        assert!(
            text.contains("0.000 0.000 0.000 rg\n0.00 0.00 50.00 100.00 re f\n"),
            "the whole page, from its bottom left"
        );
        assert!(text.contains("1.000 0.690 0.000 rg\n5.00 95.00 m\n10.00 95.00 l\n10.00 85.00 l\nh\nf\n"));
        assert!(
            text.contains("BT /F0 12.00 Tf 4.00 68.00 Td <2122232425262127232324> Tj ET"),
            "a character is written as the byte it got when it was first set, at its cell's bottom left"
        );
        assert!(text.contains("BT /F1 16.00 Tf 4.00 44.00 Td <21222324> Tj ET"), "each font counts its own");
        assert!(text.contains("/FontBBox [0 0 12 24] /FontMatrix [0.041667 0 0 0.041667 0 0]"));
        assert!(
            text.contains("<20> <003F>") && text.contains("<22> <00E4>") && text.contains("/LastChar 42 "),
            "the unknown character first, then the ten that were set in the small font"
        );
        assert!(
            text.contains("/Title <FEFF004B00E400730065> /Producer (r1metal) /CreationDate (D:20250516105031-03'30')")
        );
        let table = text.rfind("startxref\n").map(|at| &text[at + 10..]).and_then(|rest| rest.lines().next());
        let table: usize = table.and_then(|offset| offset.parse().ok()).expect("where the table lies");
        assert!(file[table..].starts_with(b"xref\n0 "));
        let table_text = core::str::from_utf8(&file[table..]).expect("the table is plain text");
        let lines: Vec<&str> = table_text.lines().skip(3).take_while(|line| line.ends_with(" n ")).collect();
        assert_eq!(lines.len() + 1, count(&file, " 0 obj\n") + 1, "a line for every object");
        for (number, line) in lines.iter().enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            assert!(file[offset..].starts_with(format!("{} 0 obj\n", number + 1).as_bytes()), "object {}", number + 1);
        }
        assert_eq!(count(&file, "/Type /Page /Parent"), 2);
        assert_eq!(count(&file, " d1"), 11 + 5, "what draws each character that was set, once");
    }

    #[test]
    fn face_letterings() {
        let mut pdf = Pdf::new(100.0, 200.0, 0.5);
        let body = pdf.add_face(spleen_12x24());
        let (strong, stressed) =
            (Lettering { is_strong: true, is_stressed: false }, Lettering { is_strong: false, is_stressed: true });
        assert_eq!(
            (pdf.variant_of(body, Lettering::default()), pdf.faces.len()),
            (body, 1),
            "plain strokes upright are the face itself"
        );
        let (heavy, leaning) = (pdf.variant_of(body, strong), pdf.variant_of(body, stressed));
        assert_eq!(
            (pdf.variant_of(body, strong), pdf.variant_of(heavy, stressed)),
            (1, 2),
            "each lettering is made once, whichever of the face's letterings it is asked of"
        );
        assert_eq!((heavy, leaning, pdf.faces.len()), (1, 2, 3));
        pdf.add_page();
        pdf.add_text(body, Color(0xDF_9A00), (8.0, 8.0), "-");
        pdf.add_text(heavy, Color(0xFF_B000), (20.0, 8.0), "-");
        pdf.add_text(leaning, Color(0xDF_9A00), (32.0, 8.0), "-");
        let file = pdf.into_file("", None);
        let text = String::from_utf8_lossy(&file);
        assert!(text.contains("/FontBBox [0 0 12 24] /FontMatrix [0.041667 0 0 0.041667 0 0]"), "the face as it is");
        assert!(
            text.contains("/FontBBox [0 0 13 24] /FontMatrix [0.041667 0 0 0.041667 0 0]")
                && text.contains("12 0 0 0 13 24 d1\n"),
            "its strong lettering reaches a pixel further and moves on by the same cell"
        );
        assert!(
            text.contains("/FontMatrix [0.041667 0 0.006944 0.041667 -0.083333 0]"),
            "its stressed lettering leans by a sixth about the middle of its height"
        );
        let dash = to_rectangles(&spleen_12x24(), '-');
        let (left, bottom, width, height) = dash[0];
        assert!(
            text.contains(&format!("{left} {bottom} {width} {height} re"))
                && text.contains(&format!("{left} {bottom} {} {height} re", width + 1)),
            "a stroke of the strong lettering is the face's own, a pixel wider"
        );
        assert!(
            text.contains("BT /F1 12.00 Tf") && text.contains("BT /F2 12.00 Tf"),
            "text is set in each by its number"
        );
    }

    #[test]
    fn cells_as_the_screen_counts() {
        let mut pdf = Pdf::new(100.0, 200.0, 0.5);
        let body = pdf.add_face(spleen_12x24());
        pdf.add_page();
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        pdf.add_text(body, Color(0), (0.0, 0.0), &format!("a\u{4E2D}\u{200B}b{family}c"));
        assert!(
            pdf.pages[0].contains("<21202022202023> Tj"),
            "a wide drawing and a joined emoji are the unknown character in each of their two cells, and what the screen draws as nothing is nothing: {}",
            pdf.pages[0]
        );
    }

    #[test]
    fn one_byte_codes() {
        let mut face = Face::new(spleen_12x24(), 0, Lettering::default());
        assert_eq!((face.code('A'), face.code('\u{1F407}'), face.code('?'), face.code('A')), (0x21, 0x20, 0x20, 0x21));
        assert_eq!(
            (face.code('\u{201E}'), face.code('\u{201C}')),
            (0x22, 0x23),
            "the low quotation mark the font lacks has a byte of its own, as the high one has, and no unknown one's"
        );
        assert_eq!(to_rectangles(&face.font, '\u{201E}'), to_rectangles(&face.font, '"'), "drawn as the straight one");
        assert!(to_unicode_map(&face.characters).contains("<22> <201E>"), "and found and copied as itself");
        let characters: Vec<char> =
            (0x21..0x250u32).filter_map(char::from_u32).filter(|character| face.font.has(*character)).collect();
        assert!(characters.len() > CODES, "the font draws more characters than a byte tells apart");
        let codes: Vec<u8> = characters.iter().map(|character| face.code(*character)).collect();
        assert_eq!((face.characters.len(), codes.iter().max()), (CODES, Some(&0xFF)));
        assert_eq!(codes.last(), Some(&0x20), "one that comes when no byte is left is written as the unknown one");
        assert_eq!(face.code('A'), 0x21, "and one set before keeps its byte");
    }
}
