use super::exif::{self, APP1, UPRIGHT};
use super::{BLOCK, MARKER};
use crate::base::bytes::be16;
use crate::base::pixel::PixelFormat;
use crate::base::sampling::Place;
use crate::base::sampling::blend;
use crate::base::sampling::pick;
use crate::base::ycbcr::CHROMA_MIDDLE;
use crate::base::ycbcr::Chroma;
use crate::base::ycbcr::Colours;
use crate::base::ycbcr::WEIGHTS_601;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

const START_OF_IMAGE: u8 = 0xD8;
const BASELINE: u8 = 0xC0;
const SEQUENTIAL: u8 = 0xC1;
const FRAMES: Range<u8> = 0xC0..0xD0;
const HUFFMAN_TABLES: u8 = 0xC4;
const ARITHMETIC_CONDITIONING: u8 = 0xCC;
const RESTART_FIRST: u8 = 0xD0;
const RESTART_LAST: u8 = 0xD7;
const START_OF_SCAN: u8 = 0xDA;
const QUANT_TABLES: u8 = 0xDB;
const RESTART_MARKER_INTERVAL: u8 = 0xDD;
const COEFFICIENTS: usize = BLOCK * BLOCK;
#[rustfmt::skip]
const ZIGZAG: [u8; COEFFICIENTS] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
const SCALES: [usize; 4] = [1, 2, 4, BLOCK];
const TABLES: usize = 4;
const LONGEST_CODE: usize = 16;
const FAST_BITS: u32 = 9;
const LARGEST_SIZE: u8 = 15;
const END_OF_BLOCK: u8 = 0x00;
const ZERO_RUN: u8 = 0xF0;
const LEVEL_SHIFT: i64 = 128;
const COSINES: [i64; 9] = [4096, 4017, 3784, 3406, 2896, 2276, 1567, 799, 0];
const COSINE_BITS: u32 = 12;
const ROOT_HALF: i64 = 2896;
const JFIF: Colours = Colours::new(WEIGHTS_601, true);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    NotJpeg,
    Truncated,
    Unsupported(&'static str),
    Corrupt,
}

impl core::fmt::Display for Error {
    fn fmt(&self, text: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotJpeg => text.write_str("NotJpeg"),
            Error::Truncated => text.write_str("Truncated"),
            Error::Unsupported(what) => write!(text, "Unsupported({what:?})"),
            Error::Corrupt => text.write_str("Corrupt"),
        }
    }
}

const LEAST_BLOCK_BITS: usize = 2;

const fn cosine(sixteenths: usize) -> i64 {
    match sixteenths % 32 {
        turn @ 0..=8 => COSINES[turn],
        turn @ 9..=16 => -COSINES[16 - turn],
        turn @ 17..=24 => -COSINES[turn - 16],
        turn => COSINES[32 - turn],
    }
}

const fn weights(count: usize) -> [[i64; BLOCK]; BLOCK] {
    let mut weights = [[0; BLOCK]; BLOCK];
    let step = BLOCK / count;
    let mut frequency = 0;
    while frequency < count {
        let mut place = 0;
        while place < count {
            weights[frequency][place] =
                if frequency == 0 { ROOT_HALF } else { cosine((2 * place + 1) * frequency * step) };
            place += 1;
        }
        frequency += 1;
    }
    weights
}

const WEIGHTS: [[[i64; BLOCK]; BLOCK]; SCALES.len()] =
    [weights(SCALES[0]), weights(SCALES[1]), weights(SCALES[2]), weights(SCALES[3])];

struct Huffman {
    fast: [u16; 1 << FAST_BITS],
    largest: [i32; LONGEST_CODE + 1],
    offset: [i32; LONGEST_CODE + 1],
    symbols: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8], symbols: &[u8]) -> Result<Huffman, Error> {
        let mut table = Huffman {
            fast: [0; 1 << FAST_BITS],
            largest: [-1; LONGEST_CODE + 1],
            offset: [0; LONGEST_CODE + 1],
            symbols: symbols.to_vec(),
        };
        let (mut code, mut place) = (0u32, 0usize);
        for length in 1..=LONGEST_CODE {
            let count = usize::from(counts[length - 1]);
            if code + count as u32 > 1 << length {
                return Err(Error::Corrupt);
            }
            table.offset[length] = place as i32 - code as i32;
            for symbol in &symbols[place..place + count] {
                if length as u32 <= FAST_BITS {
                    let first = (code << (FAST_BITS - length as u32)) as usize;
                    table.fast[first..first + (1 << (FAST_BITS - length as u32))]
                        .fill(u16::from(*symbol) << 8 | length as u16);
                }
                code += 1;
            }
            if count > 0 {
                table.largest[length] = code as i32 - 1;
            }
            place += count;
            code <<= 1;
        }
        Ok(table)
    }
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    hold: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, at: 0, hold: 0, count: 0 }
    }

    #[inline]
    fn fill(&mut self) {
        while self.count <= 56 {
            let byte = match self.data.get(self.at) {
                Some(&MARKER) if self.data.get(self.at + 1) == Some(&0) => {
                    self.at += 2;
                    MARKER
                }
                Some(&MARKER) | None => 0,
                Some(&byte) => {
                    self.at += 1;
                    byte
                }
            };
            self.hold |= u64::from(byte) << (56 - self.count);
            self.count += 8;
        }
    }

    #[inline]
    fn peek(&self, bits: u32) -> u32 {
        (self.hold >> (64 - bits)) as u32
    }

    #[inline]
    fn consume(&mut self, bits: u32) {
        self.hold <<= bits;
        self.count -= bits;
    }

    #[inline]
    fn read_symbol(&mut self, table: &Huffman) -> Result<u8, Error> {
        let entry = table.fast[self.peek(FAST_BITS) as usize];
        if entry & 0xFF != 0 {
            self.consume(u32::from(entry & 0xFF));
            return Ok((entry >> 8) as u8);
        }
        for length in FAST_BITS as usize + 1..=LONGEST_CODE {
            let code = self.peek(length as u32) as i32;
            if code <= table.largest[length] {
                self.consume(length as u32);
                return table.symbols.get((code + table.offset[length]) as usize).copied().ok_or(Error::Corrupt);
            }
        }
        Err(Error::Corrupt)
    }

    #[inline]
    fn read_value(&mut self, size: u8) -> i32 {
        if size == 0 {
            return 0;
        }
        let bits = self.peek(u32::from(size)) as i32;
        self.consume(u32::from(size));
        if bits < 1 << (size - 1) { bits - (1 << size) + 1 } else { bits }
    }

    fn restart(&mut self) -> Result<(), Error> {
        (self.hold, self.count) = (0, 0);
        match self.data.get(self.at..self.at + 2) {
            Some(&[MARKER, RESTART_FIRST..=RESTART_LAST]) => {
                self.at += 2;
                Ok(())
            }
            Some(_) => Err(Error::Corrupt),
            None => Err(Error::Truncated),
        }
    }
}

#[derive(Clone, Copy)]
struct Component {
    id: u8,
    horizontal: usize,
    vertical: usize,
    quant: usize,
    dc: usize,
    ac: usize,
}

/// A JPEG file whose headers have been read.
pub struct Jpeg<'a> {
    width: usize,
    height: usize,
    components: Vec<Component>,
    quant: [[u16; COEFFICIENTS]; TABLES],
    dc: [Option<Huffman>; TABLES],
    ac: [Option<Huffman>; TABLES],
    restart: usize,
    orientation: u8,
    scan: &'a [u8],
}

/// How a picture is decoded for one size on the screen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    width: usize,
    height: usize,
    samples: [usize; 3],
    plane_width: [usize; 3],
    band_height: [usize; 3],
    unit_columns: usize,
    unit_rows: usize,
    part_rows: usize,
    width_eighths: usize,
    height_eighths: usize,
}

impl Plan {
    fn plane_bytes(&self) -> [usize; 3] {
        core::array::from_fn(|component| self.plane_width[component] * self.band_height[component] * self.unit_rows)
    }

    /// The count of parts decoded apart, and the bytes of each plane a part fills.
    pub fn parts(&self) -> (usize, [usize; 3]) {
        let bytes = core::array::from_fn(|component| {
            self.plane_width[component] * self.band_height[component] * self.part_rows
        });
        (self.unit_rows.div_ceil(self.part_rows), bytes)
    }

    /// Zeroes the planes at the plan's size and cuts them into each part's pieces.
    pub fn pieces<'a>(&self, planes: &'a mut [Vec<u8>; 3]) -> Vec<(usize, [&'a mut [u8]; 3])> {
        for (plane, bytes) in planes.iter_mut().zip(self.plane_bytes()) {
            plane.clear();
            plane.resize(bytes, 0);
        }
        let ((parts, part_bytes), [luma, blue, red]) = (self.parts(), planes);
        let pieces =
            cut(luma, part_bytes[0], parts).zip(cut(blue, part_bytes[1], parts)).zip(cut(red, part_bytes[2], parts));
        pieces.map(|((luma, blue), red)| [luma, blue, red]).enumerate().collect()
    }
}

fn cut(plane: &mut [u8], bytes: usize, parts: usize) -> impl Iterator<Item = &mut [u8]> {
    let mut rest = plane;
    (0..parts).map(move |_| {
        let current = core::mem::take(&mut rest);
        let (piece, tail) = current.split_at_mut(bytes.min(current.len()));
        rest = tail;
        piece
    })
}

fn word(bytes: &[u8], at: usize) -> Result<usize, Error> {
    be16(bytes, at).map(usize::from).ok_or(Error::Truncated)
}

impl<'a> Jpeg<'a> {
    /// Reads the headers of a file up to its coded picture.
    pub fn read(bytes: &'a [u8]) -> Result<Jpeg<'a>, Error> {
        if bytes.get(..2) != Some(&[MARKER, START_OF_IMAGE]) {
            return Err(Error::NotJpeg);
        }
        let mut jpeg = Jpeg {
            width: 0,
            height: 0,
            components: Vec::new(),
            quant: [[0; COEFFICIENTS]; TABLES],
            dc: [const { None }; TABLES],
            ac: [const { None }; TABLES],
            restart: 0,
            orientation: UPRIGHT,
            scan: &[],
        };
        let mut told = None;
        let mut at = 2;
        loop {
            // A marker may follow bytes that fill up to it (ITU T.81, B.1.1.2).
            while bytes.get(at..at + 2) == Some(&[MARKER, MARKER]) {
                at += 1;
            }
            let (marker, length) = (*bytes.get(at + 1).ok_or(Error::Truncated)?, word(bytes, at + 2)?);
            if bytes[at] != MARKER || length < 2 {
                return Err(Error::Corrupt);
            }
            let body = bytes.get(at + 4..at + 2 + length).ok_or(Error::Truncated)?;
            at += 2 + length;
            match marker {
                BASELINE | SEQUENTIAL => jpeg.read_frame(body)?,
                HUFFMAN_TABLES => jpeg.read_huffman(body)?,
                ARITHMETIC_CONDITIONING => return Err(Error::Unsupported("arithmetic coding")),
                marker if FRAMES.contains(&marker) => {
                    return Err(Error::Unsupported("an arithmetic coded, progressive, lossless or layered picture"));
                }
                QUANT_TABLES => jpeg.read_quantisers(body)?,
                RESTART_MARKER_INTERVAL => jpeg.restart = word(body, 0)?,
                // The first segment that holds EXIF tells how the picture stands.
                APP1 => told = told.or_else(|| exif::orientation(body)),
                START_OF_SCAN => {
                    jpeg.read_scan_header(body)?;
                    jpeg.scan = &bytes[at..];
                    jpeg.orientation = told.unwrap_or(UPRIGHT);
                    // A header may claim any size: blocks that need more bits than the scan has are cut short.
                    let (columns, rows) = jpeg.units();
                    let unit_blocks: usize =
                        jpeg.components.iter().map(|component| component.horizontal * component.vertical).sum();
                    let blocks = columns.saturating_mul(rows).saturating_mul(unit_blocks);
                    if blocks.saturating_mul(LEAST_BLOCK_BITS) > jpeg.scan.len().saturating_mul(u8::BITS as usize) {
                        return Err(Error::Truncated);
                    }
                    return Ok(jpeg);
                }
                _ => {}
            }
        }
    }

    fn read_frame(&mut self, body: &[u8]) -> Result<(), Error> {
        let count = usize::from(*body.get(5).ok_or(Error::Truncated)?);
        if body[0] != 8 {
            return Err(Error::Unsupported("samples of more than 8 bits"));
        }
        if count != 1 && count != 3 {
            return Err(Error::Unsupported("a picture of neither one nor three components"));
        }
        (self.height, self.width) = (word(body, 1)?, word(body, 3)?);
        self.components.clear();
        for part in body.get(6..6 + 3 * count).ok_or(Error::Truncated)?.chunks(3) {
            let (horizontal, vertical, quant) =
                (usize::from(part[1] >> 4), usize::from(part[1] & 15), usize::from(part[2]));
            if !(1..=2).contains(&horizontal) || !(1..=2).contains(&vertical) {
                return Err(Error::Unsupported("components sampled more than twice as finely as others"));
            }
            if quant >= TABLES {
                return Err(Error::Corrupt);
            }
            self.components.push(Component { id: part[0], horizontal, vertical, quant, dc: 0, ac: 0 });
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::Unsupported("a picture whose height comes after it"));
        }
        if count == 1 {
            (self.components[0].horizontal, self.components[0].vertical) = (1, 1);
        } else if self.components[1..].iter().any(|chroma| chroma.horizontal != 1 || chroma.vertical != 1) {
            return Err(Error::Unsupported("chroma sampled more finely than once a unit"));
        }
        Ok(())
    }

    fn read_quantisers(&mut self, mut body: &[u8]) -> Result<(), Error> {
        while let Some((&kind, rest)) = body.split_first() {
            if kind >> 4 != 0 {
                return Err(Error::Unsupported("quantisers of 16 bits"));
            }
            let (values, rest) = rest.split_at_checked(COEFFICIENTS).ok_or(Error::Truncated)?;
            let table = self.quant.get_mut(usize::from(kind & 15)).ok_or(Error::Corrupt)?;
            table.iter_mut().zip(values).for_each(|(quantiser, &value)| *quantiser = u16::from(value));
            body = rest;
        }
        Ok(())
    }

    fn read_huffman(&mut self, mut body: &[u8]) -> Result<(), Error> {
        while let Some((&kind, rest)) = body.split_first() {
            let (counts, rest) = rest.split_at_checked(LONGEST_CODE).ok_or(Error::Truncated)?;
            let total: usize = counts.iter().map(|&count| usize::from(count)).sum();
            let (symbols, rest) = rest.split_at_checked(total).ok_or(Error::Truncated)?;
            let tables = if kind >> 4 == 0 { &mut self.dc } else { &mut self.ac };
            *tables.get_mut(usize::from(kind & 15)).ok_or(Error::Corrupt)? = Some(Huffman::new(counts, symbols)?);
            body = rest;
        }
        Ok(())
    }

    fn read_scan_header(&mut self, body: &[u8]) -> Result<(), Error> {
        let count = usize::from(*body.first().ok_or(Error::Truncated)?);
        if self.components.is_empty() {
            return Err(Error::Corrupt);
        }
        if count != self.components.len() {
            return Err(Error::Unsupported("a picture coded in more than one scan"));
        }
        for (component, part) in
            self.components.iter_mut().zip(body.get(1..1 + 2 * count).ok_or(Error::Truncated)?.chunks(2))
        {
            if part[0] != component.id {
                return Err(Error::Corrupt);
            }
            (component.dc, component.ac) = (usize::from(part[1] >> 4), usize::from(part[1] & 15));
            if component.dc >= TABLES
                || component.ac >= TABLES
                || self.dc[component.dc].is_none()
                || self.ac[component.ac].is_none()
            {
                return Err(Error::Corrupt);
            }
        }
        Ok(())
    }

    /// Width and height in pixels.
    pub const fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    // How the picture is stored, 1 to 8 as its EXIF tells; upright where it holds none.
    pub(crate) const fn orientation(&self) -> u8 {
        self.orientation
    }

    fn units(&self) -> (usize, usize) {
        let (columns, rows) = (BLOCK * self.components[0].horizontal, BLOCK * self.components[0].vertical);
        (self.width.div_ceil(columns), self.height.div_ceil(rows))
    }

    /// The plan for a target size: the fewest samples a block that give it, chroma no coarser than luma.
    pub fn plan(&self, width: usize, height: usize) -> Plan {
        let (unit_columns, unit_rows) = self.units();
        let luma = SCALES
            .into_iter()
            .find(|samples| self.width * samples >= width * BLOCK && self.height * samples >= height * BLOCK)
            .unwrap_or(BLOCK);
        let (width_eighths, height_eighths) = (self.width * luma, self.height * luma);
        let mut plan = Plan {
            width,
            height,
            samples: [luma; 3],
            plane_width: [0; 3],
            band_height: [0; 3],
            unit_columns,
            unit_rows,
            part_rows: unit_rows,
            width_eighths,
            height_eighths,
        };
        let finest = self.components[0];
        for (index, component) in self.components.iter().enumerate() {
            let samples = (luma * finest.horizontal / component.horizontal)
                .max(luma * finest.vertical / component.vertical)
                .min(BLOCK);
            plan.samples[index] = samples;
            plan.plane_width[index] = unit_columns * component.horizontal * samples;
            plan.band_height[index] = component.vertical * samples;
        }
        if self.restart > 0 && self.restart.is_multiple_of(unit_columns) {
            plan.part_rows = self.restart / unit_columns;
        }
        plan
    }

    /// Where each of the plan's parts begins in the scan.
    pub fn part_starts(&self, plan: &Plan) -> Result<Vec<usize>, Error> {
        let (parts, _) = plan.parts();
        let mut starts = Vec::from([0]);
        let mut at = 0;
        while starts.len() < parts {
            let Some(offset) = self.scan.get(at..).and_then(|rest| rest.iter().position(|&byte| byte == MARKER)) else {
                break;
            };
            at += offset;
            match self.scan.get(at + 1) {
                Some(RESTART_FIRST..=RESTART_LAST) => {
                    starts.push(at + 2);
                    at += 2;
                }
                Some(0) => at += 2,
                Some(_) => at += 1,
                None => break,
            }
        }
        if starts.len() < parts { Err(Error::Truncated) } else { Ok(starts) }
    }

    /// Decodes a part, which begins at `start` of the scan, into its rows of the planes.
    pub fn decode(&self, plan: &Plan, part: usize, start: usize, planes: [&mut [u8]; 3]) -> Result<(), Error> {
        let first_row = part * plan.part_rows;
        let rows = plan.part_rows.min(plan.unit_rows.saturating_sub(first_row));
        for (index, plane) in planes.iter().enumerate().take(self.components.len()) {
            if plane.len() < plan.plane_width[index] * plan.band_height[index] * rows {
                return Err(Error::Truncated);
            }
        }
        let mut bits = Bits::new(self.scan.get(start..).ok_or(Error::Truncated)?);
        let mut predictions = [0i32; 3];
        let mut block = [0i32; COEFFICIENTS];
        let mut samples = [0u8; COEFFICIENTS];
        let places: [[u8; COEFFICIENTS]; 3] = core::array::from_fn(|index| to_places(plan.samples[index]));
        let mut until_restart = self.restart;
        for row in 0..rows {
            for unit in 0..plan.unit_columns {
                if self.restart > 0 && until_restart == 0 {
                    bits.restart()?;
                    (predictions, until_restart) = ([0; 3], self.restart);
                }
                until_restart = until_restart.saturating_sub(1);
                for (index, component) in self.components.iter().enumerate() {
                    let count = plan.samples[index];
                    let (width, band) = (plan.plane_width[index], plan.band_height[index]);
                    for block_row in 0..component.vertical {
                        for block_column in 0..component.horizontal {
                            self.read_block(&mut bits, component, &mut predictions[index], &places[index], &mut block)?;
                            inverse_dct(&block, count, &mut samples);
                            let (left, top) =
                                ((unit * component.horizontal + block_column) * count, row * band + block_row * count);
                            for (line, values) in samples.chunks(BLOCK).take(count).enumerate() {
                                let at = (top + line) * width + left;
                                planes[index][at..at + count].copy_from_slice(&values[..count]);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn read_block(
        &self,
        bits: &mut Bits<'_>,
        component: &Component,
        prediction: &mut i32,
        places: &[u8; COEFFICIENTS],
        block: &mut [i32; COEFFICIENTS],
    ) -> Result<(), Error> {
        let (Some(dc), Some(ac)) = (&self.dc[component.dc], &self.ac[component.ac]) else {
            return Err(Error::Corrupt);
        };
        let quant = &self.quant[component.quant];
        *block = [0; COEFFICIENTS];
        bits.fill();
        let size = bits.read_symbol(dc)?;
        if size > LARGEST_SIZE {
            return Err(Error::Corrupt);
        }
        // A damaged file's differences may add up past a word: they wrap, and the picture is wrong where they did.
        *prediction = prediction.wrapping_add(bits.read_value(size));
        block[0] = prediction.wrapping_mul(i32::from(quant[0]));
        let mut at = 1;
        while at < COEFFICIENTS {
            bits.fill();
            let symbol = bits.read_symbol(ac)?;
            let (run, size) = (usize::from(symbol >> 4), symbol & 15);
            if size == 0 {
                if symbol == END_OF_BLOCK {
                    break;
                }
                if symbol != ZERO_RUN {
                    return Err(Error::Corrupt);
                }
                at += 16;
                continue;
            }
            at += run;
            if at >= COEFFICIENTS {
                return Err(Error::Corrupt);
            }
            let value = bits.read_value(size);
            let place = places[at];
            if usize::from(place) < COEFFICIENTS {
                block[usize::from(place)] = value * i32::from(quant[at]);
            }
            at += 1;
        }
        Ok(())
    }

    /// The plane places of every column of the plan's picture.
    pub fn columns(&self, plan: &Plan) -> Columns {
        let (luma, chroma) = (plan.plane_width[0], plan.plane_width[self.chroma()]);
        Columns([luma, chroma].map(|samples| {
            (0..plan.width).map(|x| place_on(x, (plan.width, plan.width_eighths), (samples, luma))).collect()
        }))
    }

    fn chroma(&self) -> usize {
        usize::from(self.components.len() > 1)
    }

    /// Writes `lines` of the plan's picture from the decoded planes into `out` as pixel words.
    pub fn paint(
        &self,
        (plan, columns): (&Plan, &Columns),
        planes: [&[u8]; 3],
        format: PixelFormat,
        lines: Range<usize>,
        out: &mut [u32],
    ) {
        let is_grey = self.components.len() == 1;
        let luma_height = plan.band_height[0] * plan.unit_rows;
        let chroma_height = plan.band_height[self.chroma()] * plan.unit_rows;
        // A line's light, blue and red, each pixel's the mix of the four samples about it; a grey picture's colours stay at the middle.
        let mut samples = vec![CHROMA_MIDDLE as u8; 3 * plan.width];
        let (lights, colours) = samples.split_at_mut(plan.width);
        let (blues, reds) = colours.split_at_mut(plan.width);
        let mut blends = vec![0u16; plan.plane_width[0]];
        // A plane's two lines about a row mixed, then each pixel between the mixes about its column.
        let mut mix = |(plane, width): (&[u8], usize), row: Place, columns: &[Place], out: &mut [u8]| {
            let line = |at: u32| &plane[at as usize * width..][..width];
            blend(&mut blends[..width], line(row.low), line(row.high), row.fraction);
            pick::<1>(&blends, columns, out);
        };
        let [luma_columns, chroma_columns] = &columns.0;
        for (line, row) in lines.zip(out.chunks_mut(plan.width.max(1))) {
            let lines = (plan.height, plan.height_eighths);
            let (luma, colour) = (
                place_on(line, lines, (luma_height, luma_height)),
                place_on(line, lines, (chroma_height, luma_height)),
            );
            mix((planes[0], plan.plane_width[0]), luma, luma_columns, lights);
            if !is_grey {
                mix((planes[1], plan.plane_width[1]), colour, chroma_columns, blues);
                mix((planes[2], plan.plane_width[2]), colour, chroma_columns, reds);
            }
            JFIF.row(lights, Chroma::Apart(blues, reds), format, row);
        }
    }
}

fn to_places(samples: usize) -> [u8; COEFFICIENTS] {
    core::array::from_fn(|coded| {
        let place = ZIGZAG[coded];
        if usize::from(place) % BLOCK < samples && usize::from(place) / BLOCK < samples { place } else { u8::MAX }
    })
}

fn inverse_dct(block: &[i32; COEFFICIENTS], count: usize, samples: &mut [u8; COEFFICIENTS]) {
    let weight = &WEIGHTS[count.trailing_zeros() as usize];
    let shift = 2 * COSINE_BITS + 2;
    let sample = |sum: i64| (((sum + (1 << (shift - 1))) >> shift) + LEVEL_SHIFT).clamp(0, 255) as u8;
    let lines: [bool; BLOCK] =
        core::array::from_fn(|v| v < count && block[v * BLOCK..v * BLOCK + count] != [0; BLOCK][..count]);
    if lines[1..].iter().all(|line| !line) && block[1..count].iter().all(|&coefficient| coefficient == 0) {
        // Only the first coefficient: the block is one level throughout.
        let flat = sample(i64::from(block[0]) * ROOT_HALF * ROOT_HALF);
        samples.chunks_mut(BLOCK).take(count).for_each(|line| line[..count].fill(flat));
        return;
    }
    let mut half = [0i64; COEFFICIENTS];
    for v in (0..count).filter(|&v| lines[v]) {
        let coefficients = &block[v * BLOCK..v * BLOCK + count];
        for x in 0..count {
            half[v * BLOCK + x] =
                coefficients.iter().zip(weight).map(|(&coefficient, by)| i64::from(coefficient) * by[x]).sum();
        }
    }
    for y in 0..count {
        for x in 0..count {
            let sum: i64 = (0..count).filter(|&v| lines[v]).map(|v| half[v * BLOCK + x] * weight[v][y]).sum();
            samples[y * BLOCK + x] = sample(sum);
        }
    }
}

/// The places of a plan's columns on the luma and on the chroma plane.
pub struct Columns([Vec<Place>; 2]);

// The place of pixel `pixel` of `pixels`, a picture `eighths` eighths of a luma sample long, on a plane `samples` long beside a luma plane `luma` long: neither plane's samples past the picture's last hold any of it.
fn place_on(pixel: usize, (pixels, eighths): (usize, usize), (samples, luma): (usize, usize)) -> Place {
    let (valid, luma) = (eighths.div_ceil(BLOCK).min(luma), luma.max(1));
    Place::new(pixel, pixels, (eighths * samples, BLOCK * luma), (valid * samples).div_ceil(luma).min(samples))
}

#[cfg(test)]
mod tests {
    use super::super::exif::samples::exif_body;
    use super::super::{file_end, restart_interval};
    use super::*;
    use alloc::vec;

    macro_rules! asset {
        ($name:literal) => {
            include_bytes!(concat!("../../../assets/jpeg/", $name))
        };
    }

    const WIDTH: usize = 72;
    const HEIGHT: usize = 52;
    const FORMAT: PixelFormat = PixelFormat::XRGB8888;
    const COLOURS_420: &[u8] = asset!("colours_420.jpg");
    const RESTARTS_420: &[u8] = asset!("restarts_420.jpg");

    fn draw(bytes: &[u8], width: usize, height: usize, format: PixelFormat) -> Result<Vec<u32>, Error> {
        let jpeg = Jpeg::read(bytes)?;
        let plan = jpeg.plan(width, height);
        let mut planes = Default::default();
        let starts = jpeg.part_starts(&plan)?;
        for (part, pieces) in plan.pieces(&mut planes) {
            jpeg.decode(&plan, part, starts[part], pieces)?;
        }
        let mut out = vec![0u32; width * height];
        jpeg.paint((&plan, &jpeg.columns(&plan)), [&planes[0], &planes[1], &planes[2]], format, 0..height, &mut out);
        Ok(out)
    }

    fn channels(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| [(word >> 16) as u8, (word >> 8) as u8, *word as u8]).collect()
    }

    fn distance(actual: &[u8], reference: &[u8]) -> (u32, u8) {
        assert_eq!(actual.len(), reference.len());
        let differences = || actual.iter().zip(reference).map(|(first, second)| first.abs_diff(*second));
        (differences().map(u32::from).sum::<u32>() * 100 / actual.len() as u32, differences().max().unwrap_or(0))
    }

    fn to_smaller(reference: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut out = vec![0u8; width * height * 3];
        for (index, value) in out.iter_mut().enumerate() {
            let (pixel, channel) = (index / 3, index % 3);
            let (rows, columns) = (
                (pixel / width) * HEIGHT / height..((pixel / width + 1) * HEIGHT).div_ceil(height),
                (pixel % width) * WIDTH / width..((pixel % width + 1) * WIDTH).div_ceil(width),
            );
            let sum: u32 = rows
                .clone()
                .flat_map(|y| columns.clone().map(move |x| (x, y)))
                .map(|(x, y)| u32::from(reference[(y * WIDTH + x) * 3 + channel]))
                .sum();
            *value = (sum / (rows.len() * columns.len()) as u32) as u8;
        }
        out
    }

    #[test]
    fn size_and_plan() {
        let jpeg = Jpeg::read(COLOURS_420).unwrap();
        assert_eq!(jpeg.size(), (WIDTH, HEIGHT));
        let full = jpeg.plan(WIDTH, HEIGHT);
        assert_eq!(
            (full.samples, full.unit_columns, full.unit_rows),
            ([8, 8, 8], 5, 4),
            "units of 16 pixels, the last ones part outside the picture"
        );
        assert_eq!((full.plane_bytes(), full.parts().0), ([80 * 64, 40 * 32, 40 * 32], 1));
        let quarter = jpeg.plan(18, 13);
        assert_eq!(
            quarter.samples,
            [2, 4, 4],
            "a quarter of the luma samples and half of the chroma ones: as many of each"
        );
        assert_eq!(
            (quarter.plane_bytes(), quarter.width_eighths, quarter.height_eighths),
            ([20 * 16; 3], 8 * 18, 8 * 13)
        );
        assert_eq!(jpeg.plan(19, 13).samples, [4, 8, 8], "one pixel more needs the next finer step");
        assert_eq!(jpeg.plan(9, 6).samples, [1, 2, 2]);
        assert_eq!(jpeg.plan(400, 300).samples, [8, 8, 8], "a picture is never decoded finer than it is");
        let grey = Jpeg::read(asset!("grey.jpg")).unwrap();
        assert_eq!((grey.plan(36, 26).samples[0], grey.plan(36, 26).plane_bytes()), (4, [36 * 28, 0, 0]));
    }

    #[test]
    fn full_size_matches_reference() {
        let check = |name: &str, file: &[u8], reference: &[u8], mean: u32, largest: u8| {
            let actual = channels(&draw(file, WIDTH, HEIGHT, FORMAT).unwrap());
            let (mean_distance, largest_distance) = distance(&actual, reference);
            assert!(
                mean_distance <= mean && largest_distance <= largest,
                "{name}: {mean_distance} hundredths apart on average, {largest_distance} at most"
            );
        };
        check("4:4:4", asset!("colours_444.jpg"), asset!("colours_444.rgb"), 60, 3);
        check("4:2:2", asset!("colours_422.jpg"), asset!("colours_422.rgb"), 150, 30);
        check("4:2:0", COLOURS_420, asset!("colours_420.rgb"), 150, 30);
        check("grey", asset!("grey.jpg"), asset!("grey.rgb"), 60, 3);
    }

    #[test]
    fn scaled_matches_average() {
        let reference = asset!("colours_444.rgb");
        for (width, height, mean) in [(36, 26, 300), (18, 13, 300), (48, 35, 300), (30, 20, 300), (9, 6, 600)] {
            for (name, file) in [
                ("4:4:4", &asset!("colours_444.jpg")[..]),
                ("4:2:0", COLOURS_420),
                ("4:2:2", asset!("colours_422.jpg")),
            ] {
                let actual = channels(&draw(file, width, height, FORMAT).unwrap());
                let (mean_distance, largest_distance) = distance(&actual, &to_smaller(reference, width, height));
                assert!(
                    mean_distance <= mean && largest_distance <= 60,
                    "{name} at {width}x{height}: {mean_distance} hundredths apart on average, {largest_distance} at most"
                );
            }
        }
    }

    #[test]
    fn restart_parts() {
        let plain = draw(COLOURS_420, WIDTH, HEIGHT, FORMAT).unwrap();
        assert_eq!(draw(RESTARTS_420, WIDTH, HEIGHT, FORMAT).unwrap(), plain, "a restart after every row of units");
        assert_eq!(
            draw(asset!("odd_restarts_420.jpg"), WIDTH, HEIGHT, FORMAT).unwrap(),
            plain,
            "restarts in the middle of rows"
        );
        let jpeg = Jpeg::read(RESTARTS_420).unwrap();
        let plan = jpeg.plan(36, 26);
        let (parts, part_bytes) = plan.parts();
        let starts = jpeg.part_starts(&plan).unwrap();
        assert_eq!((parts, part_bytes, starts.len()), (4, [40 * 8; 3], 4));
        let restart = |pair: &[u8]| pair[0] == MARKER && (RESTART_FIRST..=RESTART_LAST).contains(&pair[1]);
        let last = RESTARTS_420.windows(2).rposition(restart).unwrap();
        let cut = Jpeg::read(&RESTARTS_420[..last]).unwrap();
        assert_eq!(cut.part_starts(&cut.plan(36, 26)), Err(Error::Truncated), "a scan cut before its last part begins");
        assert_eq!(
            Jpeg::read(asset!("odd_restarts_420.jpg")).unwrap().plan(36, 26).parts().0,
            1,
            "parts that are no whole rows are decoded in one go"
        );
        let mut planes = Default::default();
        let pieces = plan.pieces(&mut planes);
        let lengths: Vec<(usize, [usize; 3])> =
            pieces.iter().map(|(part, piece)| (*part, piece.each_ref().map(|plane| plane.len()))).collect();
        assert_eq!(
            lengths,
            [(0, part_bytes), (1, part_bytes), (2, part_bytes), (3, part_bytes)],
            "a part's rows of each plane"
        );
        for (part, piece) in pieces.into_iter().rev() {
            jpeg.decode(&plan, part, starts[part], piece).unwrap();
        }
        let mut actual = vec![0u32; 36 * 26];
        let (upper, lower) = actual.split_at_mut(36 * 10);
        let columns = jpeg.columns(&plan);
        jpeg.paint((&plan, &columns), [&planes[0], &planes[1], &planes[2]], FORMAT, 10..26, lower);
        jpeg.paint((&plan, &columns), [&planes[0], &planes[1], &planes[2]], FORMAT, 0..10, upper);
        assert_eq!(
            actual,
            draw(COLOURS_420, 36, 26, FORMAT).unwrap(),
            "decoded last part first and painted in two pieces"
        );
    }

    #[test]
    fn parts_any_grouping() {
        let mut planes: [Vec<u8>; 3] = Default::default();
        for file in [RESTARTS_420, COLOURS_420, asset!("grey.jpg")] {
            for (width, height) in [(36, 26), (72, 52), (20, 14), (9, 6)] {
                let (jpeg, expected) = (Jpeg::read(file).unwrap(), draw(file, width, height, FORMAT).unwrap());
                let plan = jpeg.plan(width, height);
                let (starts, (parts, _)) = (jpeg.part_starts(&plan).unwrap(), plan.parts());
                for group in 1..=parts {
                    let mut pieces = plan.pieces(&mut planes);
                    for lot in pieces.chunks_mut(group).rev() {
                        for (part, [luma, blue, red]) in lot {
                            jpeg.decode(&plan, *part, starts[*part], [luma, blue, red]).unwrap();
                        }
                    }
                    let mut actual = vec![0u32; width * height];
                    let columns = jpeg.columns(&plan);
                    jpeg.paint((&plan, &columns), [&planes[0], &planes[1], &planes[2]], FORMAT, 0..height, &mut actual);
                    assert!(
                        actual == expected,
                        "{width}x{height} in lots of {group} of {parts} parts, the last lot first"
                    );
                }
            }
        }
        let mut plane = [0u8; 5];
        let lengths: Vec<usize> = cut(&mut plane, 2, 4).map(|piece| piece.len()).collect();
        assert_eq!(lengths, [2, 2, 1, 0], "the last part fills what is left");
        assert_eq!(cut(&mut [], 0, 3).map(|piece| piece.len()).collect::<Vec<_>>(), [0, 0, 0], "a plane not used");
    }

    #[test]
    fn pixel_format() {
        let other = PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 };
        let (first, second) = (draw(COLOURS_420, 18, 13, FORMAT).unwrap(), draw(COLOURS_420, 18, 13, other).unwrap());
        assert!(first.iter().zip(&second).all(|(left, right)| FORMAT.unpack(*left) == other.unpack(*right)));
        let red = FORMAT.unpack(draw(COLOURS_420, WIDTH, HEIGHT, FORMAT).unwrap()[16 * WIDTH + 16]);
        assert!(red.r() > 215 && red.g() < 60 && red.b() < 60, "the red patch: {red:?}");
        let grey = draw(asset!("grey.jpg"), WIDTH, HEIGHT, FORMAT).unwrap();
        assert!(grey.iter().all(|word| word >> 16 & 0xFF == word & 0xFF && word >> 8 & 0xFF == word & 0xFF));
    }

    #[test]
    fn refusals() {
        assert_eq!(draw(b"not a picture", 8, 8, FORMAT).err(), Some(Error::NotJpeg));
        assert_eq!(draw(&[], 8, 8, FORMAT).err(), Some(Error::NotJpeg));
        assert!(matches!(draw(asset!("progressive.jpg"), 8, 8, FORMAT), Err(Error::Unsupported(_))));
        assert_eq!(draw(&COLOURS_420[..0x200], 8, 8, FORMAT).err(), Some(Error::Truncated), "cut inside its tables");
        assert_eq!(draw(COLOURS_420, 0, 0, FORMAT), Ok(vec![]));
        let frame = COLOURS_420.windows(2).position(|pair| pair == [MARKER, BASELINE]).unwrap();
        let mut boundless = COLOURS_420.to_vec();
        boundless[frame + 5..frame + 9].fill(0xFF);
        assert_eq!(
            Jpeg::read(&boundless).err(),
            Some(Error::Truncated),
            "a picture of 65535 by 65535 pixels in a scan of a few hundred bytes is no picture to decode"
        );
        let filled = [&COLOURS_420[..2], &[MARKER, MARKER], &COLOURS_420[2..]].concat();
        assert_eq!(
            draw(&filled, WIDTH, HEIGHT, FORMAT),
            draw(COLOURS_420, WIDTH, HEIGHT, FORMAT),
            "bytes that fill up to a marker"
        );
        let errors = [Error::NotJpeg, Error::Truncated, Error::Unsupported("arithmetic coding"), Error::Corrupt];
        assert_eq!(
            errors.map(|error| alloc::string::ToString::to_string(&error)),
            ["NotJpeg", "Truncated", "Unsupported(\"arithmetic coding\")", "Corrupt"],
            "the words an error's text and the log tell each by"
        );
    }

    #[test]
    fn orientation_of_first_exif() {
        let segment =
            |marker: u8, body: &[u8]| [&[MARKER, marker][..], &(body.len() as u16 + 2).to_be_bytes(), body].concat();
        let with = |segments: &[Vec<u8>]| [&COLOURS_420[..2], &segments.concat()[..], &COLOURS_420[2..]].concat();
        let told = |file: &[u8]| Jpeg::read(file).map(|jpeg| jpeg.orientation());
        assert_eq!(told(COLOURS_420), Ok(UPRIGHT), "no EXIF");
        let commented = with(&[segment(0xFE, b"hi"), segment(APP1, &exif_body(6, true))]);
        assert_eq!(told(&commented), Ok(6), "behind a comment");
        let filled = with(&[vec![MARKER, MARKER], segment(APP1, &exif_body(8, false))]);
        assert_eq!(told(&filled), Ok(8), "fill bytes before a marker are passed over");
        let several = with(&[
            segment(APP1, b"http://ns.adobe.com/xap/1.0/\0"),
            segment(APP1, &exif_body(3, true)),
            segment(APP1, &exif_body(6, true)),
        ]);
        assert_eq!(told(&several), Ok(3), "the first segment that holds EXIF");
    }

    #[test]
    fn encoder_interval_and_end() {
        assert_eq!(
            Jpeg::read(RESTARTS_420).unwrap().restart,
            restart_interval(WIDTH as u32) as usize,
            "as a file with a restart every row of units has it"
        );
        assert_eq!(file_end(COLOURS_420), Some(COLOURS_420.len()));
    }

    #[test]
    fn dc_sum_wraps() {
        let one_code = |symbol: u8| {
            let mut counts = [0u8; LONGEST_CODE];
            counts[0] = 1;
            Huffman::new(&counts, &[symbol]).unwrap()
        };
        // Every block the code of the largest size, a difference of all ones, and the end of the block: 17 bits.
        let blocks: usize = 70_000;
        let (mut data, mut hold, mut held) = (Vec::new(), 0u64, 0u32);
        for _ in 0..blocks {
            (hold, held) = (hold << 17 | 0x7FFF << 1, held + 17);
            while held >= 8 {
                held -= 8;
                let byte = (hold >> held) as u8;
                data.push(byte);
                if byte == MARKER {
                    data.push(0);
                }
            }
        }
        let jpeg = Jpeg {
            width: BLOCK,
            height: BLOCK,
            components: vec![Component { id: 1, horizontal: 1, vertical: 1, quant: 0, dc: 0, ac: 0 }],
            quant: [[255; COEFFICIENTS]; TABLES],
            dc: [Some(one_code(LARGEST_SIZE)), None, None, None],
            ac: [Some(one_code(END_OF_BLOCK)), None, None, None],
            restart: 0,
            orientation: UPRIGHT,
            scan: &data,
        };
        let (mut bits, mut prediction, mut block) = (Bits::new(&data), 0i32, [0i32; COEFFICIENTS]);
        let places = to_places(BLOCK);
        for _ in 0..blocks {
            jpeg.read_block(&mut bits, &jpeg.components[0], &mut prediction, &places, &mut block).unwrap();
        }
        assert_eq!(prediction, (blocks as i64 * 32_767) as i32, "the sum wraps as a word does");
    }

    #[test]
    fn damaged_files() {
        for file in [RESTARTS_420, asset!("grey.jpg")] {
            let headers = file.len() - Jpeg::read(file).unwrap().scan.len();
            for end in (0..headers).chain((headers..file.len()).step_by(8)) {
                let _ = draw(&file[..end], 18, 13, FORMAT);
            }
            let mut damaged = file.to_vec();
            for at in (2..file.len()).step_by(5) {
                let bit = 1 << (at % 8);
                damaged[at] ^= bit;
                let _ = draw(&damaged, 18, 13, FORMAT);
                damaged[at] ^= bit;
            }
        }
    }
}
