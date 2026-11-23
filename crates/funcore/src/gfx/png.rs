//! PNG files: their chunks, and their inflated rows made a picture.

use super::{Picture, Rect, Surface, to_fit};
use crate::base::bytes::be32;
use crate::base::checksum::crc32;
use crate::base::pixel::{Color, PixelFormat};
use alloc::vec;
use alloc::vec::Vec;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const HEAD: usize = 8;
const CRC: usize = 4;
const HEADER: [u8; 4] = *b"IHDR";
const PALETTE: [u8; 4] = *b"PLTE";
const TRANSPARENCY: [u8; 4] = *b"tRNS";
const DATA: [u8; 4] = *b"IDAT";
const END: [u8; 4] = *b"IEND";
const HEADER_BYTES: usize = 13;
const DEPTH: u8 = 8;
const PLAIN: u8 = 0;
const ANCILLARY: u8 = 0x20;
const OPAQUE: u8 = u8::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    NotPng,
    Damaged,
    Unsupported,
    BadData,
    TooLarge(usize, usize),
}

impl core::fmt::Display for Error {
    fn fmt(&self, text: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotPng => text.write_str("NotPng"),
            Error::Damaged => text.write_str("Damaged"),
            Error::Unsupported => text.write_str("Unsupported"),
            Error::BadData => text.write_str("BadData"),
            Error::TooLarge(width, height) => write!(text, "TooLarge({width}, {height})"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Colour {
    Grey,
    Rgb,
    Palette,
    GreyAlpha,
    Rgba,
}

impl Colour {
    const fn from_byte(byte: u8) -> Option<Colour> {
        match byte {
            0 => Some(Colour::Grey),
            2 => Some(Colour::Rgb),
            3 => Some(Colour::Palette),
            4 => Some(Colour::GreyAlpha),
            6 => Some(Colour::Rgba),
            _ => None,
        }
    }

    const fn bytes(self) -> usize {
        match self {
            Colour::Grey | Colour::Palette => 1,
            Colour::GreyAlpha => 2,
            Colour::Rgb => 3,
            Colour::Rgba => 4,
        }
    }
}

/// The parsed chunks of a PNG file, its image data still zlib-compressed.
pub struct Png {
    pub(crate) width: usize,
    pub(crate) height: usize,
    colour: Colour,
    palette: Vec<[u8; 4]>,
    transparent: Option<[u8; 3]>,
    pub data: Vec<u8>,
}

impl Png {
    // Reads a file's chunks; a picture with a side longer than `largest_side` is none, so its sizes never outgrow a `usize`.
    pub(crate) fn read(file: &[u8], largest_side: usize) -> Result<Png, Error> {
        let mut rest = file.strip_prefix(&SIGNATURE).ok_or(Error::NotPng)?;
        let mut png: Option<Png> = None;
        while rest.len() >= HEAD + CRC {
            let length = be32(rest, 0).ok_or(Error::NotPng)? as usize;
            let total = HEAD.checked_add(length).and_then(|sum| sum.checked_add(CRC)).ok_or(Error::NotPng)?;
            let chunk = rest.get(..total).ok_or(Error::NotPng)?;
            rest = &rest[total..];
            let (kind, data) = ([chunk[4], chunk[5], chunk[6], chunk[7]], &chunk[HEAD..HEAD + length]);
            if be32(chunk, HEAD + length) != Some(crc32(&chunk[4..HEAD + length])) {
                return Err(Error::Damaged);
            }
            match (kind, &mut png) {
                (HEADER, None) => png = Some(Png::header(data)?),
                (_, None) => return Err(Error::NotPng),
                (PALETTE, Some(png)) => {
                    png.palette = data.as_chunks::<3>().0.iter().map(|&[r, g, b]| [r, g, b, OPAQUE]).collect();
                }
                (TRANSPARENCY, Some(png)) => png.transparency(data),
                (DATA, Some(png)) => png.data.extend_from_slice(data),
                (END, Some(_)) => break,
                (kind, Some(_)) if kind[0] & ANCILLARY == 0 => return Err(Error::Unsupported),
                _ => {}
            }
        }
        let png = png.ok_or(Error::NotPng)?;
        if png.width > largest_side || png.height > largest_side {
            return Err(Error::TooLarge(png.width, png.height));
        }
        Ok(png)
    }

    fn header(data: &[u8]) -> Result<Png, Error> {
        if data.len() != HEADER_BYTES {
            return Err(Error::NotPng);
        }
        let side = |at: usize| be32(data, at).map(|side| side as usize).ok_or(Error::NotPng);
        let (width, height) = (side(0)?, side(4)?);
        let colour = Colour::from_byte(data[9]).ok_or(Error::Unsupported)?;
        if width == 0 || height == 0 || data[8] != DEPTH || data[10..13] != [PLAIN; 3] {
            return Err(Error::Unsupported);
        }
        Ok(Png { width, height, colour, palette: Vec::new(), transparent: None, data: Vec::new() })
    }

    fn transparency(&mut self, data: &[u8]) {
        match self.colour {
            Colour::Palette => {
                self.palette.iter_mut().zip(data).for_each(|(entry, &alpha)| entry[3] = alpha);
            }
            Colour::Grey if data.len() >= 2 => self.transparent = Some([data[1]; 3]),
            Colour::Rgb if data.len() >= 6 => self.transparent = Some([data[1], data[3], data[5]]),
            _ => {}
        }
    }

    /// Bytes of the inflated image data: every row with its filter byte.
    pub const fn inflated_bytes(&self) -> usize {
        self.height * (1 + self.width * self.colour.bytes())
    }

    /// The most of the inflated image data to read: a byte past its rows, by which `picture` refuses a longer stream.
    pub const fn read_limit(&self) -> u64 {
        self.inflated_bytes() as u64 + 1
    }

    /// The picture over `ground`, fitted to `area`, from the inflated rows.
    pub fn picture(
        &self,
        mut rows: Vec<u8>,
        ground: Color,
        area: (usize, usize),
        format: PixelFormat,
    ) -> Result<Picture, Error> {
        if rows.len() != self.inflated_bytes() {
            return Err(Error::BadData);
        }
        let (step, line) = (self.colour.bytes(), 1 + self.width * self.colour.bytes());
        let mut words = Vec::with_capacity(self.width * self.height);
        for row in 0..self.height {
            let (before, here) = rows.split_at_mut(row * line);
            let above = before.get(before.len().saturating_sub(line) + 1..).filter(|_| row > 0);
            unfilter(here[0], &mut here[1..line], above, step)?;
            for sample in here[1..line].chunks_exact(step) {
                let [r, g, b, a] = self.rgba(sample)?;
                let pixel = Color(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
                words.push(format.pack(ground.toward(pixel, u32::from(a), u32::from(OPAQUE))));
            }
        }
        let (across, down) = to_fit((self.width, self.height), area);
        if (across, down) == (self.width, self.height) {
            return Ok(Picture { words, width: across });
        }
        let mut small = vec![0; across * down];
        if let Some(mut surface) = Surface::new(&mut small, across as u32, down as u32, across, format) {
            surface.shrink(Rect::new(0, 0, across as i32, down as i32), self.width, &words);
        }
        Ok(Picture { words: small, width: across })
    }

    fn rgba(&self, sample: &[u8]) -> Result<[u8; 4], Error> {
        let to_rgba = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], if self.transparent == Some(rgb) { 0 } else { OPAQUE }];
        Ok(match self.colour {
            Colour::Grey => to_rgba([sample[0]; 3]),
            Colour::Rgb => to_rgba([sample[0], sample[1], sample[2]]),
            Colour::Palette => *self.palette.get(usize::from(sample[0])).ok_or(Error::BadData)?,
            Colour::GreyAlpha => [sample[0], sample[0], sample[0], sample[1]],
            Colour::Rgba => [sample[0], sample[1], sample[2], sample[3]],
        })
    }
}

fn unfilter(filter: u8, row: &mut [u8], above: Option<&[u8]>, step: usize) -> Result<(), Error> {
    let up = |at: usize| above.map_or(0, |above| above[at]);
    match filter {
        0 => {}
        1 => (step..row.len()).for_each(|at| row[at] = row[at].wrapping_add(row[at - step])),
        2 => (0..row.len()).for_each(|at| row[at] = row[at].wrapping_add(up(at))),
        3 => {
            for at in 0..row.len() {
                let left = if at >= step { u16::from(row[at - step]) } else { 0 };
                row[at] = row[at].wrapping_add(((left + u16::from(up(at))) / 2) as u8);
            }
        }
        4 => {
            for at in 0..row.len() {
                let (left, corner) = if at >= step { (row[at - step], up(at - step)) } else { (0, 0) };
                row[at] = row[at].wrapping_add(paeth(left, up(at), corner));
            }
        }
        _ => return Err(Error::BadData),
    }
    Ok(())
}

fn paeth(left: u8, up: u8, corner: u8) -> u8 {
    let guess = i16::from(left) + i16::from(up) - i16::from(corner);
    let (from_left, from_up, from_corner) =
        ((guess - i16::from(left)).abs(), (guess - i16::from(up)).abs(), (guess - i16::from(corner)).abs());
    if from_left <= from_up && from_left <= from_corner {
        left
    } else if from_up <= from_corner {
        up
    } else {
        corner
    }
}

#[cfg(test)]
pub(crate) mod samples {
    use alloc::vec::Vec;

    // A PNG file of that size, every sample the same grey.
    pub(crate) fn grey_png(width: u32, height: u32) -> Vec<u8> {
        let mut file = Vec::new();
        let mut encoder = png::Encoder::new(&mut file, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let samples = alloc::vec![0x80; (width * height) as usize];
        encoder.write_header().unwrap().write_image_data(&samples).unwrap();
        file
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::support::RGBX;
    use super::*;

    const GROUND: Color = Color(0x10_2030);
    const LARGEST_SIDE: usize = 8;

    fn encoded(
        width: u32,
        height: u32,
        colour: png::ColorType,
        filter: png::Filter,
        samples: &[u8],
        extra: impl FnOnce(&mut png::Encoder<'_, &mut Vec<u8>>),
    ) -> Vec<u8> {
        let mut file = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut file, width, height);
            encoder.set_color(colour);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_deflate_compression(png::DeflateCompression::NoCompression);
            encoder.set_filter(filter);
            extra(&mut encoder);
            encoder.write_header().unwrap().write_image_data(samples).unwrap();
        }
        file
    }

    fn stored(stream: &[u8]) -> Vec<u8> {
        let (mut rest, mut out) = (&stream[2..], Vec::new());
        loop {
            let is_last = rest[0] & 1 == 1;
            let length = usize::from(u16::from_le_bytes([rest[1], rest[2]]));
            out.extend_from_slice(&rest[5..5 + length]);
            rest = &rest[5 + length..];
            if is_last {
                return out;
            }
        }
    }

    fn drawn(file: &[u8], area: (usize, usize)) -> Picture {
        let png = Png::read(file, LARGEST_SIDE).unwrap();
        png.picture(stored(&png.data), GROUND, area, RGBX).unwrap()
    }

    fn speckled(width: usize, height: usize, bytes: usize) -> Vec<u8> {
        (0..width * height * bytes).map(|at| (super::super::support::speckle(at as u32) >> 8) as u8).collect()
    }

    #[test]
    fn filters_and_alpha() {
        let (width, height) = (7, 5);
        let samples = speckled(width, height, 4);
        let expected: Vec<u32> = samples
            .as_chunks::<4>()
            .0
            .iter()
            .map(|rgba| {
                let over = |ink: u8, ground: u8| {
                    (u32::from(ink) * u32::from(rgba[3]) + u32::from(ground) * u32::from(255 - rgba[3]) + 127) / 255
                };
                RGBX.pack(Color(
                    over(rgba[0], GROUND.r()) << 16 | over(rgba[1], GROUND.g()) << 8 | over(rgba[2], GROUND.b()),
                ))
            })
            .collect();
        for filter in [png::Filter::NoFilter, png::Filter::Sub, png::Filter::Up, png::Filter::Avg, png::Filter::Paeth] {
            let file = encoded(width as u32, height as u32, png::ColorType::Rgba, filter, &samples, |_| {});
            let picture = drawn(&file, (width, height));
            assert_eq!((picture.width, &picture.words), (width, &expected), "{filter:?}");
        }
    }

    #[test]
    fn transparency_by_colour_type() {
        let grey = encoded(2, 1, png::ColorType::Grayscale, png::Filter::Paeth, &[0, 255], |_| {});
        assert_eq!(drawn(&grey, (2, 1)).words, [RGBX.pack(Color(0)), RGBX.pack(Color(0xFF_FFFF))]);
        let keyed = encoded(2, 1, png::ColorType::Rgb, png::Filter::Sub, &[1, 2, 3, 4, 5, 6], |encoder| {
            encoder.set_trns(alloc::vec![0, 1, 0, 2, 0, 3]);
        });
        assert_eq!(
            drawn(&keyed, (2, 1)).words,
            [RGBX.pack(GROUND), RGBX.pack(Color(0x04_0506))],
            "the colour the transparency names is the ground"
        );
        let half = encoded(1, 1, png::ColorType::GrayscaleAlpha, png::Filter::Up, &[0xFF, 0x80], |_| {});
        assert_eq!(drawn(&half, (1, 1)).words, [RGBX.pack(Color(0x88_9098))], "half of white over the ground");
        let indexed = encoded(3, 1, png::ColorType::Indexed, png::Filter::Avg, &[0, 1, 1], |encoder| {
            encoder.set_palette(alloc::vec![0xFF, 0, 0, 0, 0, 0xFF]);
            encoder.set_trns(alloc::vec![0]);
        });
        assert_eq!(
            drawn(&indexed, (3, 1)).words,
            [RGBX.pack(GROUND), RGBX.pack(Color(0x00_00FF)), RGBX.pack(Color(0x00_00FF))],
            "an entry without alpha is opaque"
        );
    }

    #[test]
    fn shrinks_to_area() {
        let samples: Vec<u8> =
            (0..8 * 4).flat_map(|at| if at % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
        let file = encoded(8, 4, png::ColorType::Rgba, png::Filter::NoFilter, &samples, |_| {});
        let picture = drawn(&file, (4, 4));
        assert_eq!((picture.width, picture.words.len()), (4, 8), "half as wide and as tall, its side the area's");
        assert!(
            picture.words.iter().all(|word| *word == RGBX.pack(Color(0x7F_7F7F))),
            "each the mean of the two it covers"
        );
        assert_eq!(drawn(&file, (100, 100)).width, 8, "never larger than it is");
    }

    #[test]
    fn png_errors() {
        let file = encoded(2, 2, png::ColorType::Rgba, png::Filter::NoFilter, &[0; 16], |_| {});
        assert_eq!(Png::read(b"GIF89a", LARGEST_SIDE).err(), Some(Error::NotPng));
        assert_eq!(Png::read(&file[..30], LARGEST_SIDE).err(), Some(Error::NotPng), "cut inside its header");
        let mut damaged = file.clone();
        damaged[20] ^= 1;
        assert_eq!(Png::read(&damaged, LARGEST_SIDE).err(), Some(Error::Damaged));
        assert_eq!(
            (Png::read(&file, 1).err(), Png::read(&damaged, 1).err(), Png::read(&file, 2).is_ok()),
            (Some(Error::TooLarge(2, 2)), Some(Error::Damaged), true),
            "a side past the largest allowed, told of a file that reads whole alone"
        );
        let mut sixteen = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut sixteen, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder.write_header().unwrap().write_image_data(&[0, 0]).unwrap();
        }
        assert_eq!(Png::read(&sixteen, LARGEST_SIDE).err(), Some(Error::Unsupported), "a depth of 16 bits");
        let png = Png::read(&file, LARGEST_SIDE).unwrap();
        assert_eq!((png.width, png.height, png.inflated_bytes(), png.read_limit()), (2, 2, 18, 19));
        assert_eq!(png.picture(alloc::vec![0; 17], GROUND, (2, 2), RGBX).err(), Some(Error::BadData), "a row short");
        let mut rows = stored(&png.data);
        rows[0] = 5;
        assert_eq!(png.picture(rows, GROUND, (2, 2), RGBX).err(), Some(Error::BadData), "a filter that is none");
        let indexed = encoded(1, 1, png::ColorType::Indexed, png::Filter::NoFilter, &[3], |encoder| {
            encoder.set_palette(alloc::vec![0, 0, 0]);
        });
        let png = Png::read(&indexed, LARGEST_SIDE).unwrap();
        assert_eq!(
            png.picture(stored(&png.data), GROUND, (1, 1), RGBX).err(),
            Some(Error::BadData),
            "past the palette"
        );
        let errors = [Error::NotPng, Error::Damaged, Error::Unsupported, Error::BadData, Error::TooLarge(4097, 10)];
        assert_eq!(
            errors.map(|error| alloc::string::ToString::to_string(&error)),
            ["NotPng", "Damaged", "Unsupported", "BadData", "TooLarge(4097, 10)"],
            "the words an error's text and the log tell each by"
        );
    }

    #[test]
    fn paeth_nearest() {
        assert_eq!([paeth(10, 20, 10), paeth(20, 10, 10), paeth(10, 10, 20), paeth(5, 5, 5)], [20, 20, 10, 5]);
    }
}
