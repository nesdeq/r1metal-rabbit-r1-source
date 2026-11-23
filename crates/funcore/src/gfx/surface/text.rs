use super::Surface;
use crate::base::pixel::Color;
use crate::gfx::Rect;
use crate::gfx::emoji::Emoji;
use crate::gfx::font::{self, Font};
use crate::gfx::glyph::{self, Glyph};
use crate::gfx::unifont::{self, Drawing};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Lettering {
    pub(crate) is_strong: bool,
    pub(crate) is_stressed: bool,
}

#[inline(always)]
fn row_bits(bits: &[u8], cell: i32) -> u64 {
    let lit = bits
        .iter()
        .take(size_of::<u64>())
        .enumerate()
        .fold(0u64, |lit, (at, byte)| lit | u64::from(*byte) << (u64::BITS as usize - 8 * (at + 1)));
    lit & !(u64::MAX >> cell)
}

#[inline(always)]
fn runs(mut lit: u64) -> impl Iterator<Item = (u32, u32)> {
    core::iter::from_fn(move || {
        let first = lit.leading_zeros();
        (lit != 0).then(|| {
            let run = (!(lit << first)).leading_zeros();
            lit &= u64::MAX.checked_shr(first + run).unwrap_or(0);
            (first, run)
        })
    })
}

fn text_box(font: &Font, x: i32, y: i32, text_width: i32, lettering: Lettering) -> Rect {
    let height = font.height as i32;
    let middle = height / 2;
    let (least, most) = if lettering.is_stressed {
        ((middle - (height - 1)).div_euclid(font::LEAN).min(0), middle.div_euclid(font::LEAN).max(0))
    } else {
        (0, 0)
    };
    let width = text_width - least + most + i32::from(lettering.is_strong);
    Rect::new(x + least, y, width, height)
}

impl Surface<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text(
        &mut self,
        within: Rect,
        font: &Font,
        (x, y): (i32, i32),
        text: &str,
        ink: Color,
        scale: i32,
        lettering: Lettering,
    ) -> i32 {
        if scale > 1 {
            debug_assert!(lettering == Lettering::default(), "letters grown are drawn plain");
            return self.text_scaled(within, font, (x, y), text, ink, scale);
        }
        let Lettering { is_strong, is_stressed } = lettering;
        let (cell_width, middle) = (font.width as i32, font.height as i32 / 2);
        let text_width = font.text_width(text);
        let after = x + text_width;
        let Some(visible) = self.to_visible(text_box(font, x, y, text_width, lettering), within) else {
            return after;
        };
        let word = self.format.pack(ink);
        let rows = (visible.y - y).max(0) as usize..(visible.bottom() - y).min(font.height as i32).max(0) as usize;
        let mut pen = x;
        for (glyph, character, _) in glyph::glyphs(text.chars()) {
            let width = glyph.cells() as i32 * cell_width;
            // A letter whose cell, leaning or stronger, lies outside what is visible lights nothing there.
            let is_outside =
                pen + cell_width + middle / font::LEAN + 1 < visible.x || pen - middle / font::LEAN > visible.right();
            let place = Rect::new(pen, y, width, font.height as i32);
            if let Glyph::Emoji(emoji) = glyph {
                self.emoji_within(visible, place, &emoji);
            } else if let Glyph::Drawing(drawing) = glyph {
                self.drawing_within(visible, place, &drawing, ink, 1);
            } else if glyph == Glyph::Letter && !is_outside {
                let glyph = font.glyph(character).chunks(font.row_bytes).enumerate().skip(rows.start).take(rows.len());
                for (row, bits) in glyph {
                    let lit = row_bits(bits, cell_width);
                    if lit == 0 {
                        continue;
                    }
                    let aside = if is_stressed { (middle - row as i32).div_euclid(font::LEAN) } else { 0 };
                    let left = pen + aside;
                    // A strong letter lights the column right of each it lights.
                    let (from, to) = ((visible.x - left).max(0), (visible.right() - left).min(u64::BITS as i32));
                    if from >= to {
                        continue;
                    }
                    let visible_bits = (u64::MAX >> from) & !u64::MAX.checked_shr(to as u32).unwrap_or(0);
                    let lit = (if is_strong { lit | lit >> 1 } else { lit }) & visible_bits;
                    let line = (y + row as i32) as usize * self.stride;
                    for (first, run) in runs(lit) {
                        let at = line + (left + first as i32) as usize;
                        self.pixels[at..at + run as usize].fill(word);
                    }
                }
            }
            pen += width;
        }
        after
    }

    // Letters of `scale` screen pixels to each of the font's, every run of lit pixels a fill.
    fn text_scaled(
        &mut self,
        within: Rect,
        font: &Font,
        (x, y): (i32, i32),
        text: &str,
        ink: Color,
        scale: i32,
    ) -> i32 {
        let cell = font.width as i32;
        let after = x + font.text_width(text) * scale;
        let Some(within) = within.clip(self.bounds()) else {
            return after;
        };
        let rows = (within.y - y).div_euclid(scale).max(0) as usize
            ..(within.bottom() - y + scale - 1).div_euclid(scale).max(0) as usize;
        let mut pen = x;
        for (glyph, character, _) in glyph::glyphs(text.chars()) {
            if pen >= within.right() {
                break;
            }
            let width = glyph.cells() as i32 * cell * scale;
            let place = Rect::new(pen, y, width, font.height as i32 * scale);
            if let Glyph::Emoji(emoji) = glyph {
                self.emoji_within(within, place, &emoji);
            } else if let Glyph::Drawing(drawing) = glyph {
                self.drawing_within(within, place, &drawing, ink, scale);
            } else if glyph == Glyph::Letter && pen + width > within.x {
                let glyph = font.glyph(character).chunks(font.row_bytes).enumerate().skip(rows.start).take(rows.len());
                for (row, bits) in glyph {
                    let top = y + row as i32 * scale;
                    for (first, run) in runs(row_bits(bits, cell)) {
                        let square = Rect::new(pen + first as i32 * scale, top, run as i32 * scale, scale);
                        if let Some(visible) = square.clip(within) {
                            self.fill(visible, ink);
                        }
                    }
                }
            }
            pen += width;
        }
        after
    }

    // A drawing grown as tall as its cells, `scale` screen pixels to each of its own.
    fn drawing_within(&mut self, within: Rect, place: Rect, drawing: &Drawing, ink: Color, scale: i32) {
        let side = (place.h / scale.max(1)).clamp(0, unifont::GROWN_ROWS as i32) as usize;
        for (row, bits) in drawing.grown(side).into_iter().take(side).enumerate() {
            for (first, run) in runs(u64::from(bits) << (u64::BITS - u32::BITS)) {
                let square =
                    Rect::new(place.x + first as i32 * scale, place.y + row as i32 * scale, run as i32 * scale, scale);
                if let Some(visible) = square.clip(within) {
                    self.fill(visible, ink);
                }
            }
        }
    }

    // An emoji's picture in its own colours, each run of one colour a fill, the ground left where it has none.
    fn emoji_within(&mut self, within: Rect, place: Rect, emoji: &Emoji) {
        let side = Emoji::side(place.h);
        let scale = (place.w / side as i32).min(place.h / side as i32).max(1);
        let left = place.x + (place.w - side as i32 * scale) / 2;
        let top = place.y + (place.h - side as i32 * scale) / 2;
        for row in 0..side {
            let mut first = 0;
            while first < side {
                let colour = emoji.pixel(side, first, row);
                let run = (first..side).take_while(|x| emoji.pixel(side, *x, row) == colour).count();
                let square =
                    Rect::new(left + first as i32 * scale, top + row as i32 * scale, run as i32 * scale, scale);
                if let (Some(colour), Some(visible)) = (colour, square.clip(within)) {
                    self.fill(visible, colour);
                }
                first += run;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::support::RGBX;
    use crate::gfx::surface::WHITE;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn strong_and_stressed_lettering() {
        const WIDTH: usize = 64;
        let font = font::spleen_12x24();
        let lettered = |is_strong: bool, is_stressed: bool| {
            let mut memory = vec![0u32; WIDTH * 24];
            let mut surface = Surface::new(&mut memory, WIDTH as u32, 24, WIDTH, RGBX).unwrap();
            let lettering = Lettering { is_strong, is_stressed };
            assert_eq!(
                surface.text(surface.bounds(), &font, (12, 0), "Hg", WHITE, 1, lettering),
                36,
                "a lettering moves no letter from its cell"
            );
            memory
        };
        let (plain, strong, stressed) = (lettered(false, false), lettered(true, false), lettered(false, true));
        let lit = |memory: &[u32]| memory.iter().filter(|&&pixel| pixel != 0).count();
        assert!(
            lit(&strong) > lit(&plain) && plain.iter().zip(&strong).all(|(plain, strong)| *plain == 0 || *strong != 0),
            "every stroke is a pixel wider, and none is thinner"
        );
        assert_eq!(lit(&stressed), lit(&plain), "leaning, a letter lights as many pixels as upright");
        for row in 0..24 {
            let aside = (12 - row as i32).div_euclid(font::LEAN);
            for x in 0..WIDTH as i32 {
                let upright = usize::try_from(x - aside)
                    .ok()
                    .filter(|from| *from < WIDTH)
                    .map_or(0, |from| plain[row * WIDTH + from]);
                assert_eq!(stressed[row * WIDTH + x as usize], upright, "row {row} stands {aside} pixels aside");
            }
        }
        assert_eq!(
            [0i32, 6, 12, 18, 23].map(|row| (12 - row).div_euclid(font::LEAN)),
            [2, 1, 0, -1, -2],
            "a pixel to the right for every six rows above the middle, one to the left for every six below"
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn by_bits(
        surface: &mut Surface<'_>,
        within: Rect,
        font: &Font,
        x: i32,
        y: i32,
        text: &str,
        ink: Color,
        lettering: Lettering,
    ) {
        let Lettering { is_strong, is_stressed } = lettering;
        let (cell_width, middle) = (font.width as i32, font.height as i32 / 2);
        let bounds = text_box(font, x, y, font.text_width(text), lettering);
        let Some(visible) = within.clip(surface.bounds()).and_then(|area| area.clip(bounds)) else {
            return;
        };
        let word = surface.format.pack(ink);
        for (place, character) in text.chars().enumerate() {
            let pen = x + place as i32 * cell_width;
            for (row, bits) in font.glyph(character).chunks(font.row_bytes).enumerate() {
                let line_y = y + row as i32;
                if line_y < visible.y || line_y >= visible.bottom() {
                    continue;
                }
                let aside = if is_stressed { (middle - row as i32).div_euclid(font::LEAN) } else { 0 };
                for column in 0..cell_width {
                    if bits[column as usize / 8] & (0x80 >> (column % 8)) == 0 {
                        continue;
                    }
                    for pixel_x in pen + column + aside..=pen + column + aside + i32::from(is_strong) {
                        if pixel_x >= visible.x && pixel_x < visible.right() {
                            surface.pixels[line_y as usize * surface.stride + pixel_x as usize] = word;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn runs_match_bits() {
        const WIDTH: u32 = 200;
        const STRIDE: usize = 211;
        let places: [(i32, i32, Rect); 6] = [
            (0, 0, Rect::new(0, 0, 200, 40)),
            (3, 4, Rect::new(5, 3, 37, 11)),
            (-7, -5, Rect::new(0, 0, 200, 40)),
            (170, 20, Rect::new(0, 0, 200, 40)),
            (-30, 9, Rect::new(-50, -50, 500, 500)),
            (12, -40, Rect::new(0, 0, 200, 40)),
        ];
        for font in [font::spleen_12x24(), font::spleen_16x32()] {
            let is_letter = |character: &char| {
                font.has(*character)
                    && glyph::glyphs([*character].into_iter()).next().map(|read| read.0) == Some(Glyph::Letter)
            };
            let letters: Vec<char> = (0x20u32..0x2600).filter_map(char::from_u32).filter(is_letter).collect();
            assert!(letters.len() > 300, "the letters the font draws: {}", letters.len());
            for line in letters.chunks(17) {
                let line: alloc::string::String = line.iter().collect();
                for (x, y, within) in places {
                    for (is_strong, is_stressed) in [(false, false), (true, false), (false, true), (true, true)] {
                        let lettering = Lettering { is_strong, is_stressed };
                        let (mut runs, mut bits) = (vec![0u32; STRIDE * 40], vec![0u32; STRIDE * 40]);
                        let mut surface = Surface::new(&mut runs, WIDTH, 40, STRIDE, RGBX).unwrap();
                        let after = surface.text(within, &font, (x, y), &line, WHITE, 1, lettering);
                        by_bits(
                            &mut Surface::new(&mut bits, WIDTH, 40, STRIDE, RGBX).unwrap(),
                            within,
                            &font,
                            x,
                            y,
                            &line,
                            WHITE,
                            lettering,
                        );
                        assert!(runs == bits, "{line:?} at ({x}, {y}) within {within:?}, {lettering:?}");
                        assert_eq!(after, x + line.chars().count() as i32 * font.width as i32);
                    }
                }
            }
        }
    }

    #[test]
    fn drawing_in_two_cells() {
        let Some((Glyph::Drawing(sign), _, _)) = glyph::glyphs("\u{4E2D}".chars()).next() else {
            panic!("a character Spleen lacks is Unifont's drawing")
        };
        for (font, scale) in [(font::spleen_12x24(), 1), (font::spleen_16x32(), 1), (font::spleen_12x24(), 2)] {
            let (cell, height) = (font.width as i32 * scale, font.height as i32 * scale);
            let (width, grown) = (3 * cell, sign.grown(font.height as usize));
            let (mut memory, mut alone) = (vec![0u32; (width * height) as usize], vec![0u32; (cell * height) as usize]);
            let plain = Lettering::default();
            let mut surface = Surface::new(&mut memory, width as u32, height as u32, width as usize, RGBX).unwrap();
            assert_eq!(
                surface.text(surface.bounds(), &font, (0, 0), "\u{4E2D}\u{200B}A", WHITE, scale, plain),
                width,
                "two cells, none and one"
            );
            let mut letter = Surface::new(&mut alone, cell as u32, height as u32, cell as usize, RGBX).unwrap();
            letter.text(letter.bounds(), &font, (0, 0), "A", WHITE, scale, plain);
            for (y, x) in (0..height).flat_map(|y| (0..width).map(move |x| (y, x))) {
                let lit = if x < 2 * cell {
                    grown[(y / scale) as usize] & (0x8000_0000 >> (x / scale)) != 0
                } else {
                    alone[(y * cell + x - 2 * cell) as usize] != 0
                };
                assert_eq!(
                    memory[(y * width + x) as usize] != 0,
                    lit,
                    "({x}, {y}): the drawing grown to the font's {} rows, {scale} a pixel, over both its cells",
                    font.height
                );
            }
        }
    }

    #[test]
    fn emoji_in_own_colours() {
        let Some((Glyph::Emoji(face), _, _)) = glyph::glyphs("\u{1F600}".chars()).next() else {
            panic!("a face is an emoji")
        };
        for (font, scale) in [(font::spleen_12x24(), 1), (font::spleen_16x32(), 1), (font::spleen_16x32(), 2)] {
            let (side, cell) = (font.height as usize, font.width as i32 * scale);
            let (width, height) = (3 * cell as usize, side * scale as usize);
            let mut memory = vec![0u32; width * height];
            let mut surface = Surface::new(&mut memory, width as u32, height as u32, width, RGBX).unwrap();
            let (whole, plain) = (surface.bounds(), Lettering::default());
            assert_eq!(surface.text(whole, &font, (0, 0), "\u{1F600}\u{FE0F}A", WHITE, scale, plain), 3 * cell);
            for (y, x) in (0..height).flat_map(|y| (0..2 * cell as usize).map(move |x| (y, x))) {
                let colour = face.pixel(side, x / scale as usize, y / scale as usize);
                assert_eq!(
                    memory[y * width + x],
                    colour.map_or(0, |colour| RGBX.pack(colour)),
                    "({x}, {y}) of the picture {side} a side, {scale} a pixel: its own colour whatever the writing's"
                );
            }
        }
        let font = font::spleen_12x24();
        let mut memory = vec![0u32; 24 * 24];
        let mut part = Surface::new(&mut memory, 24, 24, 24, RGBX).unwrap();
        part.text(Rect::new(5, 7, 9, 11), &font, (0, 0), "\u{1F600}", WHITE, 1, Lettering::default());
        let is_within = |at: usize| Rect::new(5, 7, 9, 11).contains((at % 24) as i32, (at / 24) as i32);
        assert!(
            (0..24 * 24).all(|at| is_within(at) || memory[at] == 0) && memory.iter().any(|pixel| *pixel != 0),
            "and no further than where it may be drawn"
        );
    }

    #[test]
    fn scaled_text_lights_squares() {
        let font = font::spleen_12x24();
        let (mut small, mut large) = (vec![0u32; 24 * 24], vec![0u32; 72 * 72]);
        let plain = Lettering::default();
        let mut once = Surface::new(&mut small, 24, 24, 24, RGBX).unwrap();
        once.text(once.bounds(), &font, (0, 0), "5%", WHITE, 1, plain);
        let mut thrice = Surface::new(&mut large, 72, 72, 72, RGBX).unwrap();
        assert_eq!(thrice.text(thrice.bounds(), &font, (0, 0), "5%", WHITE, 3, plain), 72);
        for y in 0..72 {
            for x in 0..72 {
                assert_eq!(large[y * 72 + x], small[(y / 3) * 24 + x / 3], "({x}, {y})");
            }
        }
        for within in
            [Rect::new(5, 7, 20, 31), Rect::new(37, -4, 9, 90), Rect::new(0, 50, 72, 22), Rect::new(80, 9, 5, 5)]
        {
            let mut cut = vec![0u32; 72 * 72];
            let mut part = Surface::new(&mut cut, 72, 72, 72, RGBX).unwrap();
            assert_eq!(part.text(within, &font, (0, 0), "5%", WHITE, 3, plain), 72);
            let expected = |at: usize| if within.contains((at % 72) as i32, (at / 72) as i32) { large[at] } else { 0 };
            assert!(
                (0..72 * 72).all(|at| cut[at] == expected(at)),
                "within {within:?} as much as lies there, and nothing else"
            );
        }
        let mut over = Surface::new(&mut large, 72, 72, 72, RGBX).unwrap();
        assert_eq!(
            over.text(over.bounds(), &font, (-50, 60), "5", WHITE, 4, plain),
            -2,
            "and writing that hangs over the edges is cut there"
        );
    }
}
