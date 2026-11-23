use super::glyph;
use super::key::Key;
use crate::base::bytes::le32;
use alloc::collections::BTreeMap;

const MAGIC: [u8; 4] = [0x72, 0xB5, 0x4A, 0x86];
const FLAG_HAS_UNICODE_TABLE: u32 = 1;
const TABLE_END: u8 = 0xFF;
const TABLE_SEQUENCE: u8 = 0xFE;
const FALLBACK: char = '?';
pub(crate) const LEAN: i32 = 6;
const QUOTE_SUBSTITUTES: [(char, char); 4] =
    [('\u{201E}', '"'), ('\u{201F}', '"'), ('\u{201A}', '\''), ('\u{201B}', '\'')];
const LATIN: usize = 1 << u8::BITS;

static SPLEEN_12X24: &[u8] = include_bytes!("../../assets/spleen/spleen-12x24.psfu");
static SPLEEN_16X32: &[u8] = include_bytes!("../../assets/spleen/spleen-16x32.psfu");

#[derive(Debug, PartialEq, Eq)]
enum Error {
    NotPsf2,
    Truncated,
    TooWide,
    Shapeless,
}

#[derive(Clone)]
pub(crate) struct Font {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) row_bytes: usize,
    glyph_bytes: usize,
    glyphs: &'static [u8],
    map: BTreeMap<char, u32>,
    latin: [u32; LATIN],
    pub(crate) id: u64,
}

impl Font {
    fn from_psf2(data: &'static [u8]) -> Result<Font, Error> {
        if data.get(..MAGIC.len()) != Some(&MAGIC[..]) {
            return Err(Error::NotPsf2);
        }
        let field = |at: usize| le32(data, at * 4).ok_or(Error::Truncated);
        let (header_size, flags, count) = (field(2)?, field(3)?, field(4)?);
        let (glyph_bytes, height, width) = (field(5)?, field(6)?, field(7)?);
        if width >= u64::BITS {
            return Err(Error::TooWide);
        }
        let glyph_bytes = glyph_bytes as usize;
        if width == 0 || height == 0 || glyph_bytes < (width as usize).div_ceil(8) * height as usize {
            return Err(Error::Shapeless);
        }
        let table_start = header_size as usize + count as usize * glyph_bytes;
        let glyphs = data.get(header_size as usize..table_start).ok_or(Error::Truncated)?;

        let mut map = BTreeMap::new();
        if flags & FLAG_HAS_UNICODE_TABLE != 0 {
            let table = &data[table_start..];
            for (glyph, entry) in table.split(|&byte| byte == TABLE_END).take(count as usize).enumerate() {
                let singles = entry.split(|&byte| byte == TABLE_SEQUENCE).next().unwrap_or(&[]);
                for character in core::str::from_utf8(singles).unwrap_or("").chars() {
                    map.entry(character).or_insert(glyph as u32);
                }
            }
        } else {
            map.extend((0..count).filter_map(|index| Some((char::from_u32(index)?, index))));
        }

        let mut font = Font {
            width,
            height,
            row_bytes: (width as usize).div_ceil(8),
            glyph_bytes,
            glyphs,
            map,
            latin: [0; LATIN],
            id: Key::default().bytes(data).value(),
        };
        font.latin = core::array::from_fn(|code| font.index(char::from(code as u8)));
        Ok(font)
    }

    fn substitute(&self, character: char) -> Option<char> {
        QUOTE_SUBSTITUTES
            .iter()
            .find(|(missing, _)| *missing == character)
            .map(|(_, plain)| *plain)
            .filter(|_| !self.has(character))
    }

    pub(super) fn ink_top(&self, characters: &str) -> u32 {
        let first = |character: char| {
            self.glyph(character).chunks(self.row_bytes.max(1)).position(|row| row.iter().any(|&bits| bits != 0))
        };
        characters.chars().filter_map(first).min().map_or(self.height, |row| row as u32)
    }

    pub(crate) fn text_width(&self, text: &str) -> i32 {
        glyph::cells_of(text) as i32 * self.width as i32
    }

    pub(crate) fn has(&self, character: char) -> bool {
        self.map.contains_key(&character)
    }

    pub(crate) fn can_draw(&self, character: char) -> bool {
        self.has(character) || self.substitute(character).is_some_and(|plain| self.has(plain))
    }

    fn index(&self, character: char) -> u32 {
        self.map
            .get(&character)
            .or_else(|| self.substitute(character).and_then(|plain| self.map.get(&plain)))
            .or_else(|| self.map.get(&FALLBACK))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn glyph(&self, character: char) -> &[u8] {
        let index = self.latin.get(character as usize).copied().unwrap_or_else(|| self.index(character)) as usize;
        &self.glyphs[index * self.glyph_bytes..(index + 1) * self.glyph_bytes]
    }
}

pub(crate) fn spleen_12x24() -> Font {
    Font::from_psf2(SPLEEN_12X24).expect("embedded font is valid")
}

pub(crate) fn spleen_16x32() -> Font {
    Font::from_psf2(SPLEEN_16X32).expect("embedded font is valid")
}

/// Every character the embedded faces have a letter of their own for, which the tool that writes Unifont's drawings and the emoji leaves to them.
pub fn spleen_characters() -> impl Iterator<Item = char> {
    [spleen_12x24(), spleen_16x32()].into_iter().flat_map(|font| font.map.into_keys())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_fonts_cover_ascii_and_german() {
        for (font, width, height) in [(spleen_12x24(), 12, 24), (spleen_16x32(), 16, 32)] {
            assert_eq!((font.width, font.height), (width, height));
            assert_eq!(font.glyph('A').len(), font.row_bytes * height as usize);
            for character in (' '..='~').chain("äöüÄÖÜß".chars()) {
                assert!(font.has(character), "missing {character:?}");
            }
            assert_ne!(font.glyph('A'), font.glyph('B'));
            assert!(font.glyph(' ').iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    fn ink_top_rows() {
        let (large, small) = (spleen_16x32(), spleen_12x24());
        assert_eq!(large.ink_top("0123456789:"), 6, "the digits of the clock begin six rows down their cell");
        assert_eq!((large.ink_top(" "), large.ink_top("")), (32, 32), "nothing lit, nothing above it");
        assert!(small.ink_top("y") > small.ink_top("d") && small.ink_top(":0") == small.ink_top("0"));
    }

    #[test]
    fn text_widths() {
        let (small, large) = (spleen_12x24(), spleen_16x32());
        assert_eq!((small.text_width("ab"), large.text_width("ab"), small.text_width("")), (24, 32, 0));
        assert_eq!(
            small.text_width("a\u{1F600}\u{200B}\u{4E2D}"),
            5 * 12,
            "a letter takes a cell, an emoji and a wide drawing two each, what is drawn as nothing none"
        );
    }

    #[test]
    fn latin_table_matches_file() {
        for font in [spleen_12x24(), spleen_16x32()] {
            for character in (0..=u8::MAX).map(char::from) {
                let index = font.index(character) as usize;
                let glyph = &font.glyphs[index * font.glyph_bytes..][..font.glyph_bytes];
                assert_eq!(font.glyph(character), glyph, "{character:?}");
            }
            let missing = (0..=u8::MAX).map(char::from).find(|character| !font.has(*character));
            let missing = missing.expect("one the file lacks");
            assert_eq!(font.glyph(missing), font.glyph('?'), "{missing:?}, which the file lacks, as `?`");
        }
    }

    #[test]
    fn missing_quote_substituted() {
        for font in [spleen_12x24(), spleen_16x32()] {
            for (missing, plain) in [('\u{201E}', '"'), ('\u{201F}', '"'), ('\u{201A}', '\''), ('\u{201B}', '\'')] {
                assert!(!font.has(missing) && font.can_draw(missing), "{missing:?} is none of the file's, and drawn");
                assert_eq!(font.glyph(missing), font.glyph(plain), "{missing:?} as {plain:?}");
            }
            for own in ['\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}'] {
                assert!(font.has(own) && font.glyph(own) != font.glyph('?'), "{own:?} is the file's own");
            }
            assert!(
                !font.can_draw('\u{1F407}') && font.glyph('\u{1F407}') == font.glyph('?'),
                "anything else the font lacks is no quotation mark, and drawn as `?`"
            );
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Font::from_psf2(b"nope").err(), Some(Error::NotPsf2));
        assert_eq!(Font::from_psf2(&MAGIC).err(), Some(Error::Truncated));
        let one = |bytes: u32, height: u32, width: u32| -> &'static [u8] {
            let header = [u32::from_le_bytes(MAGIC), 0, 32, 0, 1, bytes, height, width];
            header.iter().flat_map(|field| field.to_le_bytes()).chain([0; 8]).collect::<alloc::vec::Vec<u8>>().leak()
        };
        assert_eq!(
            Font::from_psf2(one(8, 1, 64)).err(),
            Some(Error::TooWide),
            "a strong letter's column would not fit"
        );
        for (bytes, height, width, what) in
            [(8, 1, 0, "no width"), (8, 0, 8, "no height"), (1, 1, 12, "fewer bytes than its rows")]
        {
            assert_eq!(Font::from_psf2(one(bytes, height, width)).err(), Some(Error::Shapeless), "a letter of {what}");
        }
    }
}
