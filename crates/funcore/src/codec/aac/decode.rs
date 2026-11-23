use super::tables::{
    BOOK_1, BOOK_2, BOOK_3, BOOK_4, BOOK_5, BOOK_6, BOOK_7, BOOK_8, BOOK_9, BOOK_10, BOOK_11, LONG_BANDS, SCALEFACTORS,
    SHORT_BANDS, TNS_LONG_BANDS, TNS_SHORT_BANDS, split,
};
use super::tools::{self, Filter, MAX_FILTERS, MAX_ORDER, Tns};
use super::transform::{SHORT, SINE, Sequence, Transform, WINDOWS};
use super::{
    BAND_COUNT_BITS, BOOK_BITS, BOOKS, Book, CHANNEL_PAIR, CODE_BOOKS, COUPLING_CHANNEL, Config, DATA_STREAM,
    ELEMENT_BITS, ESCAPE, ESCAPE_BITS, FILL, FRAME, GAIN_BITS, IN_PHASE_BOOK, LARGEST_GROUP, LARGEST_VALUE,
    LOW_FREQUENCY, NOISE_BOOK, OUT_OF_PHASE_BOOK, PROGRAM_CONFIG, RESERVED_BOOK, RUN_BITS, RUN_ESCAPE, Reader,
    SCALE_REACH, SCALE_STEPS, SCALES, SEQUENCE_BITS, SINGLE_CHANNEL, TAG_BITS, UNIT_SCALE, ZERO_BOOK, power,
};
use crate::base::bits::BitReader;
use crate::base::math::{cbrt, to_sample};
use crate::codec::Flaw;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

const LEAF: u16 = 1 << 15;
const GROUPS: usize = WINDOWS;
// The bands of a long window at 32 kHz, the most any window has (4.5.4).
const MAX_BANDS: usize = 51;
const SHORT_BAND_COUNT_BITS: u32 = 4;
// A bit for each short window after the first (Table 4.6).
const GROUPING_BITS: u32 = 7;
const SHORT_RUN_BITS: u32 = 3;
const SHORT_RUN_ESCAPE: u32 = (1 << SHORT_RUN_BITS) - 1;
// What a pulse_data holds (Table 4.7).
const PULSE_COUNT_BITS: u32 = 2;
const PULSE_BAND_BITS: u32 = 6;
const PULSE_OFFSET_BITS: u32 = 5;
const PULSE_AMPLITUDE_BITS: u32 = 4;
const LARGEST_PULSE: u32 = (1 << PULSE_AMPLITUDE_BITS) - 1;
// Every pulse of a pulse_data may add to one line, as an offset may be 0.
const MOST_PULSES: u32 = 1 << PULSE_COUNT_BITS;
// What a tns_data holds, for a long window and a short one (Table 4.48); the coefficients' resolution is one bit.
const TNS_FILTER_BITS: [u32; 2] = [2, 1];
const TNS_LENGTH_BITS: [u32; 2] = [6, 4];
const TNS_ORDER_BITS: [u32; 2] = [5, 3];
// Bits a coefficient's index takes at the lowest resolution; a compressed one has one bit fewer (4.6.9.3).
const TNS_LEAST_RESOLUTION: u32 = 3;
// The first energy of noise is told in 9 bits, less 256, from the global gain less 90 (4.6.13).
const NOISE_FIRST_BITS: u32 = 9;
const NOISE_FIRST: i32 = 256;
const NOISE_OFFSET: i32 = 90;
// The ranges of an energy of noise and of a position of intensity: a scalefactor's exponent, and its negation (4.6.13, 4.6.8.2).
const NOISE_ENERGIES: RangeInclusive<i32> = *SCALES.start() - UNIT_SCALE..=*SCALES.end() - UNIT_SCALE;
const INTENSITY_POSITIONS: RangeInclusive<i32> = UNIT_SCALE - *SCALES.end()..=UNIT_SCALE - *SCALES.start();
// The mid/side mask is read from the packet, or set for every band (4.6.8.1).
const MASK_BITS: u32 = 2;
const MASK_READ: u32 = 1;
const MASK_ALL: u32 = 2;
const DATA_COUNT_BITS: u32 = 8;
const DATA_ESCAPE: u32 = 255;
// What a program_config_element holds before the elements it lists: the tag, the object type and the rate's index; the bits of the counts of its front, side, back, low frequency, data and coupling elements; the optional mixdowns, mono, stereo and matrix, and the bits each holds (Table 4.2).
const PROGRAM_HEADER_BITS: u32 = TAG_BITS + 2 + 4;
const PROGRAM_COUNT_BITS: [u32; 6] = [4, 4, 4, 2, 3, 4];
const MIXDOWN_BITS: [u32; 3] = [4, 4, 3];
// The bits of each element it lists: a front, side, back or coupling element a flag and a tag, a low frequency or data element a tag; and of the count of the comment's bytes.
const LISTED_BITS: u32 = 5;
const LISTED_SHORT_BITS: u32 = 4;
const COMMENT_COUNT_BITS: u32 = 8;
// A fill element's size in bytes, the size that says an escape follows, and the extension types of spectral band replication (Table 4.51).
const FILL_COUNT_BITS: u32 = 4;
const FILL_ESCAPE: u32 = 15;
const ESCAPE_COUNT_BITS: u32 = 8;
const EXTENSION_BITS: u32 = 4;
const SBR_EXTENSIONS: [u32; 2] = [13, 14];

// Every size a quantized line can have, up to the largest the pulses may make of one.
const LINE_SIZES: usize = (LARGEST_VALUE + MOST_PULSES * LARGEST_PULSE) as usize + 1;

// Every size a quantized line can have to the power of four thirds, each cube root found from the one before.
static DEQUANT: [f32; LINE_SIZES] = {
    let (mut table, mut root, mut size) = ([0.0; LINE_SIZES], 1.0, 0);
    while size < LINE_SIZES {
        root = cbrt(if size == 0 { 1.0 } else { size as f64 }, root);
        table[size] = (size as f64 * root) as f32;
        size += 1;
    }
    table
};

// The trees of the books, in their order, and of the scalefactors.
static TREES: [&[[u16; 2]]; BOOKS] = [
    &tree::<{ BOOK_1.len() - 1 }>(&BOOK_1),
    &tree::<{ BOOK_2.len() - 1 }>(&BOOK_2),
    &tree::<{ BOOK_3.len() - 1 }>(&BOOK_3),
    &tree::<{ BOOK_4.len() - 1 }>(&BOOK_4),
    &tree::<{ BOOK_5.len() - 1 }>(&BOOK_5),
    &tree::<{ BOOK_6.len() - 1 }>(&BOOK_6),
    &tree::<{ BOOK_7.len() - 1 }>(&BOOK_7),
    &tree::<{ BOOK_8.len() - 1 }>(&BOOK_8),
    &tree::<{ BOOK_9.len() - 1 }>(&BOOK_9),
    &tree::<{ BOOK_10.len() - 1 }>(&BOOK_10),
    &tree::<{ BOOK_11.len() - 1 }>(&BOOK_11),
];
static SCALE_TREE: [[u16; 2]; SCALEFACTORS.len() - 1] = tree(&SCALEFACTORS);

// The codes of a book as the ways through a tree: at each place, where a 0 and a 1 lead; codes that leave no bits unused need one place fewer than there are codes.
const fn tree<const PLACES: usize>(codes: &[u32]) -> [[u16; 2]; PLACES] {
    let (mut nodes, mut count, mut value) = ([[0u16; 2]; PLACES], 1, 0);
    while value < codes.len() {
        let (code, length) = split(codes[value]);
        let (mut at, mut bit) = (0, length);
        while bit > 0 {
            bit -= 1;
            let branch = (code >> bit & 1) as usize;
            if bit == 0 {
                nodes[at][branch] = LEAF | value as u16;
            } else if nodes[at][branch] == 0 {
                nodes[at][branch] = count;
                count += 1;
            }
            at = (nodes[at][branch] & !LEAF) as usize;
        }
        value += 1;
    }
    nodes
}

// The value of the code that comes next, read along its book's tree.
fn read_code(tree: &[[u16; 2]], reader: &mut Reader) -> Result<usize, Flaw> {
    let mut at = 0;
    loop {
        let next = tree[at][reader.bit()? as usize];
        match next {
            0 => return Err(Flaw::Broken),
            leaf if leaf & LEAF != 0 => return Ok(usize::from(leaf & !LEAF)),
            node => at = usize::from(node),
        }
    }
}

// What a block's windows are: how they are laid out, what shape they have, and how the bands and the windows are grouped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Info {
    sequence: Sequence,
    shape: usize,
    max_sfb: usize,
    // The windows in each group; a long window is one group of one.
    lengths: [usize; GROUPS],
    groups: usize,
}

impl Info {
    const SILENT: Info =
        Info { sequence: Sequence::Long, shape: SINE, max_sfb: 0, lengths: [1, 0, 0, 0, 0, 0, 0, 0], groups: 1 };

    const fn is_short(&self) -> bool {
        matches!(self.sequence, Sequence::Short)
    }

    // Lines between the first of a window and the first of the next.
    const fn width(&self) -> usize {
        if self.is_short() { SHORT } else { FRAME }
    }
}

// The mid/side mask of a pair: whether there is one, and for each group and band whether it is set.
struct Mask {
    is_present: bool,
    used: [[bool; MAX_BANDS]; GROUPS],
}

// What the tests count of the tools a stream uses, to see that a fixture reaches them all.
#[cfg(test)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Seen {
    // Blocks of each window sequence, and of them those of the Kaiser-Bessel shape.
    sequences: [usize; 4],
    kaiser_bessel: usize,
    // Filters with coefficients, pulses, bands of noise, of which correlated, of intensity and of mid/side.
    filters: usize,
    pulses: usize,
    noise: usize,
    correlated: usize,
    intensity: usize,
    mid_side: usize,
}

// What a channel holds of the block being read, and what the next block needs of it.
struct Channel {
    info: Info,
    books: [[u8; MAX_BANDS]; GROUPS],
    // A band's scalefactor, or its energy of noise, or its position of intensity.
    scales: [[i32; MAX_BANDS]; GROUPS],
    // The lines, the windows of a block of short ones one after another, and the quantized lines they are made of.
    spectrum: Vec<f32>,
    quantized: Vec<i32>,
    tns: Tns,
    // The shape of the window the block before had.
    shape: usize,
    overlap: Vec<f32>,
    out: Vec<f32>,
    #[cfg(test)]
    seen: Seen,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            info: Info::SILENT,
            books: [[0; MAX_BANDS]; GROUPS],
            scales: [[0; MAX_BANDS]; GROUPS],
            spectrum: vec![0.0; FRAME],
            quantized: vec![0; FRAME],
            tns: Tns::default(),
            shape: SINE,
            overlap: vec![0.0; FRAME],
            out: vec![0.0; FRAME],
            #[cfg(test)]
            seen: Seen::default(),
        }
    }

    // Makes the block silence, in the shape of the window before.
    fn silence(&mut self) {
        self.spectrum.fill(0.0);
        self.info = Info { shape: self.shape, ..Info::SILENT };
    }
}

/// Decodes packets into frames of samples; the noise of bands that are noise goes on from packet to packet, so a stream decoded from its middle has other noise than one decoded from its first packet.
pub struct Decoder {
    config: Config,
    transform: Transform,
    channels: [Channel; 2],
    random: u32,
    // Where the tests have the noise of each channel of a pair made on its own, as FFmpeg's decoder makes it, though the standard has it the same.
    #[cfg(test)]
    is_independent: bool,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

fn unpack_index(book: &Book, mut index: usize, group: &mut [i32]) {
    let base = book.base() as usize;
    for line in group.iter_mut().rev() {
        let digit = (index % base) as i32;
        *line = if book.is_signed { digit - book.largest as i32 } else { digit };
        index /= base;
    }
}

fn bands_of(config: Config, is_short: bool) -> &'static [u16] {
    if is_short { SHORT_BANDS[config.index] } else { LONG_BANDS[config.index] }
}

// The lines of a band in a window of a block whose windows lie `width` lines apart.
fn lines_at(bands: &[u16], width: usize, window: usize, band: usize) -> Range<usize> {
    window * width + usize::from(bands[band])..window * width + usize::from(bands[band + 1])
}

// The sequence, the shape and the bands of a block, as its ics_info tells (Table 4.6).
fn read_info(reader: &mut Reader, config: Config) -> Result<Info, Flaw> {
    reader.bit()?;
    let sequence = Sequence::of(reader.take(SEQUENCE_BITS)?);
    let shape = reader.bit()? as usize;
    let mut info = Info { sequence, shape, max_sfb: 0, lengths: [0; GROUPS], groups: 1 };
    info.lengths[0] = 1;
    if info.is_short() {
        info.max_sfb = reader.take(SHORT_BAND_COUNT_BITS)? as usize;
        let grouping = reader.take(GROUPING_BITS)?;
        for bit in (0..GROUPING_BITS).rev() {
            if grouping >> bit & 1 != 0 {
                info.lengths[info.groups - 1] += 1;
            } else {
                info.groups += 1;
                info.lengths[info.groups - 1] = 1;
            }
        }
    } else {
        info.max_sfb = reader.take(BAND_COUNT_BITS)? as usize;
        if reader.bit()? != 0 {
            return Err(Flaw::Unsupported("prediction"));
        }
    }
    if info.max_sfb >= bands_of(config, info.is_short()).len() {
        return Err(Flaw::Broken);
    }
    Ok(info)
}

// The book of each band, as the sections of a block tell (Table 4.46).
fn read_sections(reader: &mut Reader, channel: &mut Channel) -> Result<(), Flaw> {
    let info = channel.info;
    let (bits, escape) = if info.is_short() { (SHORT_RUN_BITS, SHORT_RUN_ESCAPE) } else { (RUN_BITS, RUN_ESCAPE) };
    for group in 0..info.groups {
        let mut at = 0;
        while at < info.max_sfb {
            let book = reader.take(BOOK_BITS)? as usize;
            let mut run = 0;
            loop {
                let more = reader.take(bits)?;
                run += more as usize;
                if more != escape {
                    break;
                }
            }
            if book == RESERVED_BOOK || run == 0 || at + run > info.max_sfb {
                return Err(Flaw::Broken);
            }
            channel.books[group][at..at + run].fill(book as u8);
            at += run;
        }
    }
    Ok(())
}

// The scalefactor of each band that has lines, the energy of each of noise and the position of each of intensity, told by their steps from the one before (Table 4.47).
fn read_scales(reader: &mut Reader, channel: &mut Channel, gain: i32) -> Result<(), Flaw> {
    let info = channel.info;
    let (mut scale, mut energy, mut position, mut is_first_noise) = (gain, gain - NOISE_OFFSET, 0, true);
    for group in 0..info.groups {
        for band in 0..info.max_sfb {
            let told = match usize::from(channel.books[group][band]) {
                ZERO_BOOK => 0,
                NOISE_BOOK => {
                    energy += if is_first_noise {
                        is_first_noise = false;
                        reader.take(NOISE_FIRST_BITS)? as i32 - NOISE_FIRST
                    } else {
                        read_code(&SCALE_TREE, reader)? as i32 - SCALE_REACH
                    };
                    energy.clamp(*NOISE_ENERGIES.start(), *NOISE_ENERGIES.end())
                }
                OUT_OF_PHASE_BOOK | IN_PHASE_BOOK => {
                    position += read_code(&SCALE_TREE, reader)? as i32 - SCALE_REACH;
                    position.clamp(*INTENSITY_POSITIONS.start(), *INTENSITY_POSITIONS.end())
                }
                _ => {
                    scale += read_code(&SCALE_TREE, reader)? as i32 - SCALE_REACH;
                    if !SCALES.contains(&scale) {
                        return Err(Flaw::Broken);
                    }
                    scale
                }
            };
            channel.scales[group][band] = told;
        }
    }
    Ok(())
}

// The pulses a pulse_data tells, each the line it adds to and its amplitude (Table 4.7).
fn read_pulses(reader: &mut Reader, bands: &[u16]) -> Result<Vec<(usize, i32)>, Flaw> {
    let count = reader.take(PULSE_COUNT_BITS)? as usize + 1;
    let first = reader.take(PULSE_BAND_BITS)? as usize;
    if first >= bands.len() - 1 {
        return Err(Flaw::Broken);
    }
    let mut line = usize::from(bands[first]);
    (0..count)
        .map(|_| {
            line += reader.take(PULSE_OFFSET_BITS)? as usize;
            let amplitude = reader.take(PULSE_AMPLITUDE_BITS)? as i32;
            if line >= usize::from(bands[bands.len() - 1]) {
                return Err(Flaw::Broken);
            }
            Ok((line, amplitude))
        })
        .collect()
}

// The filters a tns_data tells for every window of the block, designed (Table 4.48, 4.6.9.3).
fn read_tns(reader: &mut Reader, channel: &mut Channel) -> Result<(), Flaw> {
    let kind = usize::from(channel.info.is_short());
    let windows = if kind == 1 { WINDOWS } else { 1 };
    for window in 0..windows {
        let count = reader.take(TNS_FILTER_BITS[kind])? as usize;
        channel.tns.counts[window] = count;
        if count == 0 {
            continue;
        }
        let resolution = TNS_LEAST_RESOLUTION + reader.bit()?;
        for number in 0..count.min(MAX_FILTERS) {
            let length = reader.take(TNS_LENGTH_BITS[kind])? as usize;
            let order = reader.take(TNS_ORDER_BITS[kind])? as usize;
            if order > MAX_ORDER[kind] {
                return Err(Flaw::Broken);
            }
            let mut filter = Filter { length, order, ..Filter::default() };
            if order > 0 {
                filter.down = reader.bit()? != 0;
                let bits = resolution - reader.bit()?;
                let mut indices = [0i32; MAX_ORDER[0]];
                for index in &mut indices[..order] {
                    let raw = reader.take(bits)? as i32;
                    *index = if raw >= 1 << (bits - 1) { raw - (1 << bits) } else { raw };
                }
                filter.lpc = tools::design(&indices[..order], resolution);
            }
            channel.tns.filters[window][number] = filter;
        }
    }
    Ok(())
}

fn read_band(reader: &mut Reader, which: usize, lines: &mut [i32]) -> Result<(), Flaw> {
    let book = &CODE_BOOKS[which];
    let mut group = [0i32; LARGEST_GROUP];
    let group = &mut group[..book.group];
    for lines in lines.chunks_exact_mut(book.group) {
        unpack_index(book, read_code(TREES[which], reader)?, group);
        if !book.is_signed {
            for line in group.iter_mut().filter(|line| **line != 0) {
                if reader.bit()? != 0 {
                    *line = -*line;
                }
            }
        }
        for line in group.iter_mut().filter(|line| book.largest == ESCAPE && line.unsigned_abs() == ESCAPE) {
            let mut ones = 0;
            while reader.bit()? != 0 {
                ones += 1;
            }
            if ones + ESCAPE_BITS > LARGEST_VALUE.trailing_ones() - 1 {
                return Err(Flaw::Broken);
            }
            let size = (1 << (ones + ESCAPE_BITS) | reader.take(ones + ESCAPE_BITS)?) as i32;
            *line = if *line < 0 { -size } else { size };
        }
        lines.copy_from_slice(group);
    }
    Ok(())
}

// The left channel of a pair and its mask, for the right to take correlated noise from.
struct Left<'a> {
    channel: &'a Channel,
    mask: &'a Mask,
}

// Reads a channel's individual_channel_stream into its lines (Table 4.44); `shared` is the ics_info a pair has in common, `left` the left channel of a pair where this is the right and their noise correlates.
fn read_channel(
    config: Config,
    channel: &mut Channel,
    random: &mut u32,
    reader: &mut Reader,
    shared: Option<Info>,
    left: Option<Left<'_>>,
) -> Result<(), Flaw> {
    let gain = reader.take(GAIN_BITS)? as i32;
    channel.info = match shared {
        Some(info) => info,
        None => read_info(reader, config)?,
    };
    let info = channel.info;
    let bands = bands_of(config, info.is_short());
    channel.tns = Tns::default();
    read_sections(reader, channel)?;
    read_scales(reader, channel, gain)?;
    let pulses = if reader.bit()? != 0 {
        if info.is_short() {
            return Err(Flaw::Broken);
        }
        read_pulses(reader, bands)?
    } else {
        Vec::new()
    };
    if reader.bit()? != 0 {
        read_tns(reader, channel)?;
    }
    if reader.bit()? != 0 {
        return Err(Flaw::Unsupported("gain control"));
    }
    let width = info.width();
    channel.quantized.fill(0);
    let mut first = 0;
    for group in 0..info.groups {
        for band in 0..info.max_sfb {
            let book = usize::from(channel.books[group][band]);
            if (1..=BOOKS).contains(&book) {
                for window in first..first + info.lengths[group] {
                    let range = lines_at(bands, width, window, band);
                    read_band(reader, book - 1, &mut channel.quantized[range])?;
                }
            }
        }
        first += info.lengths[group];
    }
    for &(line, amplitude) in &pulses {
        let band = bands.partition_point(|&begin| usize::from(begin) <= line) - 1;
        if band < info.max_sfb && (1..=BOOKS).contains(&usize::from(channel.books[0][band])) {
            let quantized = &mut channel.quantized[line];
            *quantized += if *quantized > 0 { amplitude } else { -amplitude };
            #[cfg(test)]
            {
                channel.seen.pulses += 1;
            }
        }
    }
    channel.spectrum.fill(0.0);
    let mut first = 0;
    for group in 0..info.groups {
        for band in 0..info.max_sfb {
            let (book, scale) = (usize::from(channel.books[group][band]), channel.scales[group][band]);
            for window in first..first + info.lengths[group] {
                let range = lines_at(bands, width, window, band);
                match book {
                    ZERO_BOOK | OUT_OF_PHASE_BOOK | IN_PHASE_BOOK => {}
                    NOISE_BOOK => {
                        #[cfg(test)]
                        {
                            channel.seen.noise += 1;
                        }
                        // The generator goes on whether or not the left channel's noise is taken, so that the noise after it does not depend on that.
                        let energy = power(SCALE_STEPS * scale);
                        tools::noise(random, energy, &mut channel.spectrum[range.clone()]);
                        let paired = left.as_ref().filter(|left| {
                            left.mask.is_present
                                && left.mask.used[group][band]
                                && usize::from(left.channel.books[group][band]) == NOISE_BOOK
                        });
                        if let Some(left) = paired {
                            #[cfg(test)]
                            {
                                channel.seen.correlated += 1;
                            }
                            let ratio = power(SCALE_STEPS * (scale - left.channel.scales[group][band]));
                            for at in range {
                                channel.spectrum[at] = left.channel.spectrum[at] * ratio;
                            }
                        }
                    }
                    _ => {
                        let gain = power(SCALE_STEPS * (scale - UNIT_SCALE));
                        for at in range {
                            let line = channel.quantized[at];
                            let size = DEQUANT[line.unsigned_abs() as usize] * gain;
                            channel.spectrum[at] = if line < 0 { -size } else { size };
                        }
                    }
                }
            }
        }
        first += info.lengths[group];
    }
    Ok(())
}

// Turns the lines of a pair that were coded as mid and side into left and right, and gives the right channel's bands of intensity the left's lines times their factor (4.6.8.1, 4.6.8.2).
fn stereo(config: Config, left: &mut Channel, right: &mut Channel, mask: &Mask) {
    let (info, other) = (left.info, right.info);
    let bands = bands_of(config, info.is_short());
    let width = info.width();
    let mut first = 0;
    for group in 0..info.groups {
        for band in 0..info.max_sfb {
            if mask.is_present
                && mask.used[group][band]
                && usize::from(left.books[group][band]) < NOISE_BOOK
                && usize::from(right.books[group][band]) < NOISE_BOOK
            {
                #[cfg(test)]
                {
                    left.seen.mid_side += 1;
                }
                for window in first..first + info.lengths[group] {
                    let range = lines_at(bands, width, window, band);
                    tools::mid_side(&mut left.spectrum[range.clone()], &mut right.spectrum[range]);
                }
            }
        }
        first += info.lengths[group];
    }
    if info.is_short() != other.is_short() {
        return;
    }
    let mut first = 0;
    for group in 0..other.groups {
        for band in 0..other.max_sfb {
            let book = usize::from(right.books[group][band]);
            if book == OUT_OF_PHASE_BOOK || book == IN_PHASE_BOOK {
                #[cfg(test)]
                {
                    left.seen.intensity += 1;
                }
                let sign = if book == IN_PHASE_BOOK { 1.0 } else { -1.0 };
                let inverted = if mask.is_present && mask.used[group][band] { -1.0 } else { 1.0 };
                let factor = sign * inverted * power(-SCALE_STEPS * right.scales[group][band]);
                for window in first..first + other.lengths[group] {
                    let range = lines_at(bands, width, window, band);
                    tools::intensity(&left.spectrum[range.clone()], &mut right.spectrum[range], factor);
                }
            }
        }
        first += other.lengths[group];
    }
}

// Runs a channel's filters of temporal noise shaping over its lines, each from the band that is its own up to where the one above it begins, no higher than the bands the rate allows (4.6.9.3).
fn shape_temporal_noise(config: Config, channel: &mut Channel) {
    let info = channel.info;
    let bands = bands_of(config, info.is_short());
    let reach =
        usize::from(if info.is_short() { TNS_SHORT_BANDS } else { TNS_LONG_BANDS }[config.index]).min(info.max_sfb);
    let windows = if info.is_short() { WINDOWS } else { 1 };
    for window in 0..windows {
        let mut bottom = bands.len() - 1;
        for number in 0..channel.tns.counts[window].min(MAX_FILTERS) {
            let tap = channel.tns.filters[window][number];
            let top = bottom;
            bottom = top.saturating_sub(tap.length);
            if tap.order == 0 {
                continue;
            }
            let lines = window * info.width() + usize::from(bands[bottom.min(reach)])
                ..window * info.width() + usize::from(bands[top.min(reach)]);
            if !lines.is_empty() {
                #[cfg(test)]
                {
                    channel.seen.filters += 1;
                }
                tools::filter(&mut channel.spectrum[lines], &tap);
            }
        }
    }
}

// Passes over a data_stream_element (Table 4.10).
fn skip_data_stream(reader: &mut Reader) -> Result<(), Flaw> {
    reader.take(TAG_BITS)?;
    let is_aligned = reader.bit()? != 0;
    let mut count = reader.take(DATA_COUNT_BITS)? as usize;
    if count == DATA_ESCAPE as usize {
        count += reader.take(DATA_COUNT_BITS)? as usize;
    }
    if is_aligned {
        reader.align()?;
    }
    reader.skip(u8::BITS as usize * count)
}

// Passes over a program_config_element, which lists the elements of a layout of channels (Table 4.2).
fn skip_program_config(reader: &mut Reader) -> Result<(), Flaw> {
    reader.skip(PROGRAM_HEADER_BITS as usize)?;
    let mut counts = [0u32; PROGRAM_COUNT_BITS.len()];
    for (count, bits) in counts.iter_mut().zip(PROGRAM_COUNT_BITS) {
        *count = reader.take(bits)?;
    }
    let [front, side, back, low, data, coupling] = counts;
    for bits in MIXDOWN_BITS {
        if reader.bit()? != 0 {
            reader.skip(bits as usize)?;
        }
    }
    reader.skip(((front + side + back + coupling) * LISTED_BITS + (low + data) * LISTED_SHORT_BITS) as usize)?;
    reader.align()?;
    let comment = reader.take(COMMENT_COUNT_BITS)? as usize;
    reader.skip(u8::BITS as usize * comment)
}

// Passes over a fill_element; spectral band replication in it is refused (Table 4.51).
fn skip_fill(reader: &mut Reader) -> Result<(), Flaw> {
    let mut count = reader.take(FILL_COUNT_BITS)? as usize;
    if count == FILL_ESCAPE as usize {
        count = count + reader.take(ESCAPE_COUNT_BITS)? as usize - 1;
    }
    if count > 0 && SBR_EXTENSIONS.contains(&reader.peek(EXTENSION_BITS)?) {
        return Err(Flaw::Unsupported("spectral band replication"));
    }
    reader.skip(u8::BITS as usize * count)
}

impl Decoder {
    /// A decoder of our own stream: one channel at 48 kHz.
    pub fn new() -> Decoder {
        Decoder::of(Config::OURS)
    }

    pub(crate) fn of(config: Config) -> Decoder {
        Decoder {
            config,
            transform: Transform::new(),
            channels: [Channel::new(), Channel::new()],
            random: tools::SEED,
            #[cfg(test)]
            is_independent: false,
        }
    }

    // Whether a band of noise in both channels of a pair, with its mask's bit set, has the same noise in both (4.6.13).
    const fn correlates(&self) -> bool {
        #[cfg(test)]
        {
            !self.is_independent
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    #[cfg(test)]
    fn seen(&self) -> Seen {
        let mut total = Seen::default();
        for channel in &self.channels {
            let seen = channel.seen;
            total.sequences.iter_mut().zip(seen.sequences).for_each(|(sum, count)| *sum += count);
            total.kaiser_bessel += seen.kaiser_bessel;
            total.filters += seen.filters;
            total.pulses += seen.pulses;
            total.noise += seen.noise;
            total.correlated += seen.correlated;
            total.intensity += seen.intensity;
            total.mid_side += seen.mid_side;
        }
        total
    }

    // Goes back to where a new decoder begins: nothing is left of the blocks before.
    pub(crate) fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.overlap.fill(0.0);
            channel.shape = SINE;
        }
        self.random = tools::SEED;
    }

    /// Decodes a packet into the frame it completes, the mean of its channels; one that is not played leaves silence.
    pub fn decode(&mut self, packet: &[u8], samples: &mut [i16]) -> Result<(), Flaw> {
        let outcome = self.channels_of(packet);
        let [first, second] = &self.channels;
        let mean = (0..FRAME)
            .map(|at| if self.config.channels() == 2 { 0.5 * (first.out[at] + second.out[at]) } else { first.out[at] });
        for (sample, value) in samples.iter_mut().zip(mean) {
            *sample = to_sample(value);
        }
        outcome
    }

    // Decodes a packet into the frame of every channel, which stands in its `out`.
    fn channels_of(&mut self, packet: &[u8]) -> Result<(), Flaw> {
        let outcome = self.elements(packet);
        for number in 0..self.config.channels() {
            let channel = &mut self.channels[number];
            if outcome.is_err() {
                channel.silence();
            }
            let sequence = channel.info.sequence;
            let (shapes, spectrum) = ([channel.shape, channel.info.shape], &channel.spectrum);
            self.transform.inverse(spectrum, sequence, shapes, &mut channel.overlap, &mut channel.out);
            channel.shape = channel.info.shape;
            #[cfg(test)]
            {
                channel.seen.sequences[sequence as usize] += 1;
                channel.seen.kaiser_bessel += channel.info.shape;
            }
        }
        outcome
    }

    // Reads the elements of a packet into the lines of the channels.
    fn elements(&mut self, packet: &[u8]) -> Result<(), Flaw> {
        let mut reader = Reader(BitReader::new(packet));
        let mut found = 0;
        loop {
            match reader.take(ELEMENT_BITS)? {
                SINGLE_CHANNEL | LOW_FREQUENCY => {
                    reader.take(TAG_BITS)?;
                    if found >= self.config.channels() {
                        return Err(Flaw::Unsupported("more channels than the configuration has"));
                    }
                    self.single(&mut reader, found)?;
                    found += 1;
                }
                CHANNEL_PAIR => {
                    reader.take(TAG_BITS)?;
                    if found + 2 > self.config.channels() {
                        return Err(Flaw::Unsupported("more channels than the configuration has"));
                    }
                    self.pair(&mut reader)?;
                    found += 2;
                }
                COUPLING_CHANNEL => return Err(Flaw::Unsupported("channel coupling")),
                DATA_STREAM => skip_data_stream(&mut reader)?,
                PROGRAM_CONFIG => skip_program_config(&mut reader)?,
                FILL => skip_fill(&mut reader)?,
                _ => break,
            }
        }
        if found == self.config.channels() { Ok(()) } else { Err(Flaw::Broken) }
    }

    // Reads a single_channel_element into the channel of that number (Table 4.4).
    fn single(&mut self, reader: &mut Reader, number: usize) -> Result<(), Flaw> {
        let channel = &mut self.channels[number];
        read_channel(self.config, channel, &mut self.random, reader, None, None)?;
        shape_temporal_noise(self.config, channel);
        Ok(())
    }

    // Reads a channel_pair_element into the two channels (Table 4.4).
    fn pair(&mut self, reader: &mut Reader) -> Result<(), Flaw> {
        let mut mask = Mask { is_present: false, used: [[false; MAX_BANDS]; GROUPS] };
        let common = if reader.bit()? != 0 {
            let info = read_info(reader, self.config)?;
            match reader.take(MASK_BITS)? {
                0 => {}
                MASK_READ => {
                    mask.is_present = true;
                    for group in 0..info.groups {
                        for band in 0..info.max_sfb {
                            mask.used[group][band] = reader.bit()? != 0;
                        }
                    }
                }
                MASK_ALL => {
                    mask.is_present = true;
                    mask.used = [[true; MAX_BANDS]; GROUPS];
                }
                _ => return Err(Flaw::Broken),
            }
            Some(info)
        } else {
            None
        };
        let (config, correlates) = (self.config, self.correlates());
        let (first, second) = self.channels.split_at_mut(1);
        let (left, right) = (&mut first[0], &mut second[0]);
        read_channel(config, left, &mut self.random, reader, common, None)?;
        let paired = correlates.then_some(Left { channel: left, mask: &mask });
        read_channel(config, right, &mut self.random, reader, common, paired)?;
        stereo(config, left, right, &mask);
        shape_temporal_noise(config, left);
        shape_temporal_noise(config, right);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::super::encode::pack;
    use super::super::encode::samples::sound;
    use super::super::{Encoder, read_track};
    use super::*;
    use crate::base::alloc_count::allocations;
    use crate::base::bits::BitWriter;
    use crate::base::fft::Plan;
    use crate::base::math::{iterated, next_random, sine_cosine};
    use crate::codec::aac::track_samples;
    use core::hint::black_box;

    const BANDS_USED: usize = 42;
    const QUIET_SCALE: u8 = 60;

    fn all_codes(book_index: usize) -> (Vec<i32>, Vec<u8>) {
        let book = &CODE_BOOKS[book_index];
        let mut lines = vec![0i32; FRAME];
        for (index, group) in lines.chunks_exact_mut(book.group).take(book.codes.len()).enumerate() {
            unpack_index(book, index, group);
            for (place, line) in group.iter_mut().enumerate() {
                if book.largest == ESCAPE && *line == ESCAPE as i32 {
                    let least = ESCAPE << (index % 9);
                    *line = if index == ESCAPE as usize { LARGEST_VALUE } else { least + (index as u32 * 37) % least }
                        as i32;
                }
                if !book.is_signed && (index + place) % 2 == 1 {
                    *line = -*line;
                }
            }
        }
        let mut packet = Vec::new();
        pack(&lines, &[QUIET_SCALE; BANDS_USED], &[book_index as u8 + 1; BANDS_USED], BANDS_USED, &mut packet);
        (lines, packet)
    }

    #[test]
    fn book_codes_round_trip() {
        let mut decoder = Decoder::new();
        let gain = power(SCALE_STEPS * (i32::from(QUIET_SCALE) - UNIT_SCALE));
        for book_index in 0..BOOKS {
            let (lines, packet) = all_codes(book_index);
            assert_eq!(decoder.elements(&packet), Ok(()), "book {}", book_index + 1);
            for (at, (&line, &actual)) in lines.iter().zip(&decoder.channels[0].spectrum).enumerate() {
                let size = f64::from(line.unsigned_abs()).powf(4.0 / 3.0) * f64::from(gain);
                let expected = if line < 0 { -size } else { size };
                assert!(
                    (f64::from(actual) - expected).abs() <= size * 1.0e-5,
                    "book {} line {at}: {actual} for {expected}",
                    book_index + 1
                );
            }
        }
        let (lines, _) = all_codes(BOOKS - 1);
        assert_eq!(
            lines.iter().map(|line| line.unsigned_abs()).max(),
            Some(LARGEST_VALUE),
            "up to the largest value there is"
        );
    }

    // The places of a book's tree as each is found, pushed onto a Vec.
    fn pushed_tree(codes: &[u32]) -> Vec<[u16; 2]> {
        let mut nodes = vec![[0u16; 2]];
        for (value, &entry) in codes.iter().enumerate() {
            let (code, length) = split(entry);
            let mut at = 0;
            for bit in (0..length).rev() {
                let branch = (code >> bit & 1) as usize;
                if bit == 0 {
                    nodes[at][branch] = LEAF | value as u16;
                } else if nodes[at][branch] == 0 {
                    nodes.push([0; 2]);
                    nodes[at][branch] = nodes.len() as u16 - 1;
                }
                at = usize::from(nodes[at][branch] & !LEAF);
            }
        }
        nodes
    }

    #[test]
    fn tables_bit_exact() {
        let mut root = 1.0;
        let dequant: Vec<u32> = (0..=LARGEST_VALUE + MOST_PULSES * LARGEST_PULSE)
            .map(|size| {
                root = iterated::cbrt(f64::from(size.max(1)), root);
                ((f64::from(size) * root) as f32).to_bits()
            })
            .collect();
        let made: Vec<u32> = DEQUANT.iter().map(|size| size.to_bits()).collect();
        assert_eq!(made, dequant, "every size as an iterator makes it");
        let books = CODE_BOOKS.iter().map(|book| book.codes).chain([&SCALEFACTORS[..]]);
        let trees = TREES.iter().copied().chain([&SCALE_TREE[..]]);
        for (number, (codes, tree)) in books.zip(trees).enumerate() {
            assert_eq!(tree, &pushed_tree(codes)[..], "book {}", number + 1);
        }
    }

    // The buffers a decoder works in: four of each channel's, and its transform's four and two of each of its cosine transforms.
    const BUFFERS: u64 = 16;

    #[test]
    fn tables_allocate_nothing() {
        let count = |make: fn()| {
            let before = allocations();
            make();
            allocations() - before
        };
        let plans = count(|| drop(black_box((Plan::new(FRAME / 2), Plan::new(SHORT / 2)))));
        let decoder = count(|| drop(black_box(Decoder::new())));
        assert_eq!(decoder, plans + BUFFERS, "a decoder allocates its transform's plans and its buffers alone");
    }

    // The bits of a packet as (value, count) pairs, the last byte filled with zeros.
    fn written(fields: &[(u32, u32)]) -> Vec<u8> {
        let mut bits = BitWriter::default();
        fields.iter().for_each(|&(value, count)| bits.put(value, count));
        bits.finish()
    }

    // A single channel element of our configuration up to the pulse, noise shaping and gain control flags: a long window with no bands, which reads as silence.
    fn bare(tools: [u32; 3]) -> Vec<u8> {
        written(&[
            (0, 3),
            (0, 4),
            (0, 8),
            (0, 1),
            (0, 2),
            (0, 1),
            (0, 6),
            (0, 1),
            (tools[0], 1),
            (tools[1], 1),
            (tools[2], 1),
            (7, 3),
        ])
    }

    #[test]
    fn unsupported_stream_silent() {
        let mut decoder = Decoder::new();
        let mut out = [1i16; FRAME];
        let short_pulses = written(&[(0, 3), (0, 4), (0, 8), (0, 1), (2, 2), (0, 1), (0, 4), (0, 7), (1, 1)]);
        let sbr = written(&[(6, 3), (2, 4), (13, 4), (0, 4), (0, 8)]);
        let faults = [
            (&[0b0010_0000u8, 0, 0, 0][..], Flaw::Unsupported("more channels than the configuration has")),
            (&[0b0100_0000, 0, 0, 0], Flaw::Unsupported("channel coupling")),
            (&[0, 0, 0, 0b0100_0000], Flaw::Unsupported("prediction")),
            (&bare([0, 0, 1]), Flaw::Unsupported("gain control")),
            (&short_pulses, Flaw::Broken),
            (&sbr, Flaw::Unsupported("spectral band replication")),
            (&[0, 0, 0, 0b1011_0000, 0b0010_0000], Flaw::Broken),
            (&[0, 0, 0b0001_1111, 0b1000_0000], Flaw::Broken),
            (&[0, 0, 0], Flaw::Short),
            (&[0xE0], Flaw::Broken),
        ];
        for (packet, fault) in faults {
            assert_eq!(decoder.decode(packet, &mut out), Err(fault), "{packet:x?}");
            assert_eq!(out, [0; FRAME]);
            out.fill(1);
        }
        let (_, whole) = all_codes(BOOKS - 1);
        assert_eq!(decoder.decode(&whole[..whole.len() / 2], &mut out), Err(Flaw::Short));
    }

    fn to_adts(packet: &[u8]) -> Vec<u8> {
        let length = packet.len() + 7;
        let mut bytes =
            vec![0xFF, 0xF1, 0x4C, 0x40 | (length >> 11) as u8, (length >> 3) as u8, (length << 5) as u8 | 0x1F, 0xFC];
        bytes.extend_from_slice(packet);
        bytes
    }

    fn ffmpeg(args: &[&str]) {
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(args)
            .output()
            .expect("ffmpeg on the host");
        assert!(
            output.status.success() && output.stderr.is_empty(),
            "{}",
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore]
    fn ffmpeg_agrees() {
        let samples = sound(60, &[(220.0, 9000.0), (1300.0, 3000.0), (5200.0, 900.0)], 400.0);
        let mut encoder = Encoder::new(64_000);
        let mut packets: Vec<Vec<u8>> = (0..BOOKS).map(|book_index| all_codes(book_index).1).collect();
        for frame in samples.as_chunks::<FRAME>().0 {
            let mut packet = Vec::new();
            encoder.encode(frame, &mut packet);
            packets.push(packet);
        }
        let (mut decoder, mut ours) = (Decoder::new(), vec![0i16; packets.len() * FRAME]);
        for (packet, out) in packets.iter().zip(ours.as_chunks_mut::<FRAME>().0) {
            decoder.decode(packet, out).unwrap();
        }
        let folder = std::env::temp_dir().join(std::format!("r1metal-aac-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let (stream, sound) = (folder.join("ours.aac"), folder.join("theirs.raw"));
        std::fs::write(&stream, packets.iter().flat_map(|packet| to_adts(packet)).collect::<Vec<u8>>()).unwrap();
        ffmpeg(&["-i", stream.to_str().unwrap(), "-f", "s16le", "-c:a", "pcm_s16le", sound.to_str().unwrap()]);
        let theirs: Vec<i16> =
            std::fs::read(&sound).unwrap().as_chunks::<2>().0.iter().copied().map(i16::from_le_bytes).collect();
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(theirs.len(), ours.len(), "a frame of samples for every packet");
        let difference = ours.iter().zip(&theirs).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max();
        assert!(difference <= Some(2), "ffmpeg's samples lie up to {difference:?} from ours");
        assert!(ours.iter().any(|&sample| sample.unsigned_abs() > 1000), "and there is sound in them");
    }

    // A signal with transients and noise at each of the standard's rates, encoded by ffmpeg to its widest band and decoded by it and by us.
    #[test]
    #[ignore]
    fn ffmpeg_agrees_at_every_rate() {
        let folder = std::env::temp_dir().join(std::format!("r1metal-rates-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for rate in super::super::tables::RATES {
            let mut state = rate;
            let signal: Vec<u8> = (0..rate as usize * 6 / 10)
                .map(|at| {
                    let time = at as f64 / f64::from(rate);
                    let burst = if (time * 12.0) % 1.0 < 0.15 {
                        f64::from(next_random(&mut state) >> 8) / f64::from(1u32 << 23) - 1.0
                    } else {
                        0.0
                    };
                    let tone = sine_cosine(220.0 * time).0 * 0.3 + sine_cosine(1750.0 * time).0 * 0.1;
                    let hiss = (f64::from(next_random(&mut state) >> 8) / f64::from(1u32 << 23) - 1.0) * 0.02;
                    ((tone + burst * 0.6 + hiss) * 30_000.0) as i16 as u16
                })
                .flat_map(u16::to_le_bytes)
                .collect();
            let (raw, file, theirs) = (folder.join("in.raw"), folder.join("out.m4a"), folder.join("out.pcm"));
            std::fs::write(&raw, signal).unwrap();
            let (rate_text, cutoff) = (std::format!("{rate}"), std::format!("{}", rate / 2));
            let (raw_path, file_path, theirs_path) =
                (raw.to_str().unwrap(), file.to_str().unwrap(), theirs.to_str().unwrap());
            ffmpeg(&[
                "-f", "s16le", "-ar", &rate_text, "-ac", "1", "-i", raw_path, "-c:a", "aac", "-b:a", "128k", "-cutoff",
                &cutoff, "-f", "ipod", file_path,
            ]);
            // FFmpeg 7 plays the last packet whole, past the end the file's edit tells, where FFmpeg 9 ends by itself.
            let trim = std::format!("atrim=end_sample={}", rate * 6 / 10);
            ffmpeg(&["-i", file_path, "-af", &trim, "-f", "s16le", "-c:a", "pcm_s16le", theirs_path]);
            assert_matches_ffmpeg(
                &std::format!("{rate} Hz"),
                &played(&std::fs::read(&file).unwrap(), true).channels,
                &reference(&std::fs::read(&theirs).unwrap(), 1),
            );
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    // What a decoder makes of a file: the samples of each channel as they are played, their lead passed over and no more than the edit lets be heard, the mean of them, what the decoder saw of the tools, and which packets had noise taken from the left channel of a pair.
    struct Played {
        channels: Vec<Vec<i16>>,
        mixed: Vec<i16>,
        seen: Seen,
        correlated: Vec<bool>,
    }

    // Decodes every packet of a file, the noise of a pair's channels made on its own if `is_independent`, as FFmpeg makes it.
    fn played(file: &[u8], is_independent: bool) -> Played {
        let track = read_track(file).expect("a file whose index reads");
        let config = Config::read(&track.sound.configuration).expect("a configuration that is played");
        let mut decoder = Decoder::of(config);
        decoder.is_independent = is_independent;
        let mut frame = [0i16; FRAME];
        let (mut channels, mut mixed, mut correlated) = (vec![Vec::new(); config.channels()], Vec::new(), Vec::new());
        for (number, sample) in track.packets.iter().enumerate() {
            assert!(sample.fits(file.len() as u64), "packet {number} lies in the file");
            let packet = &file[sample.at as usize..][..sample.bytes as usize];
            let before = decoder.seen().correlated;
            assert_eq!(decoder.decode(packet, &mut frame), Ok(()), "packet {number}");
            correlated.push(decoder.seen().correlated > before);
            mixed.extend_from_slice(&frame);
            for (number, samples) in channels.iter_mut().enumerate() {
                samples.extend(decoder.channels[number].out.iter().map(|&sample| to_sample(sample)));
            }
        }
        let (lead, length) = (track.sound.lead as usize, track_samples(&track) as usize);
        let kept = |samples: &mut Vec<i16>| *samples = samples[lead..lead + length].to_vec();
        channels.iter_mut().chain([&mut mixed]).for_each(kept);
        Played { channels, mixed, seen: decoder.seen(), correlated }
    }

    // The channels of a file of 16 bit samples, one after another for every sample.
    fn reference(file: &[u8], channels: usize) -> Vec<Vec<i16>> {
        let samples: Vec<i16> = file.as_chunks::<2>().0.iter().copied().map(i16::from_le_bytes).collect();
        (0..channels).map(|channel| samples.iter().skip(channel).step_by(channels).copied().collect()).collect()
    }

    // By how many decibels the samples differ less from the reference than the reference is large.
    fn ratio(reference: &[i16], found: &[i16]) -> f64 {
        let signal: f64 = reference.iter().map(|&sample| f64::from(sample).powi(2)).sum();
        let noise: f64 =
            reference.iter().zip(found).map(|(&one, &other)| (f64::from(one) - f64::from(other)).powi(2)).sum();
        10.0 * (signal / noise.max(1.0)).log10()
    }

    // Files of FFmpeg's AAC encoder with temporal noise shaping, noise substitution, intensity and mid/side stereo on, in MP4 and as a stream, of a signal that holds transients: 0.8 s of two channels at 44.1 kHz and of one at 48 kHz, and one channel at 16 kHz of the default coder. Beside each is what FFmpeg's decoder makes of it, 16 bit samples of the channels one after another at the file's own rate.
    const STEREO: &[u8] = include_bytes!("../../../assets/sound/stereo_44100.m4a");
    const STEREO_STREAM: &[u8] = include_bytes!("../../../assets/sound/stereo_44100.aac");
    const STEREO_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/stereo_44100.pcm");
    const MONO: &[u8] = include_bytes!("../../../assets/sound/mono_48000.m4a");
    const MONO_STREAM: &[u8] = include_bytes!("../../../assets/sound/mono_48000.aac");
    const MONO_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/mono_48000.pcm");
    const LOW: &[u8] = include_bytes!("../../../assets/sound/mono_16000.m4a");
    const LOW_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/mono_16000.pcm");
    // The same for two channels at 22.05 kHz with noise substitution off, so that no band has noise taken from the left channel and the standard's decoding is FFmpeg's.
    const PLAIN: &[u8] = include_bytes!("../../../assets/sound/stereo_22050.m4a");
    const PLAIN_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/stereo_22050.pcm");
    // A file as QuickTime lays out sound: a sample entry of version 1 that holds the stream in a `wave` box, one channel at 16 kHz.
    const QUICKTIME: &[u8] = include_bytes!("../../../assets/sound/quicktime_16000.mov");
    const QUICKTIME_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/quicktime_16000.pcm");

    // The decibels the fixtures' samples came to above their difference from FFmpeg's, measured at 104.74 and 101.14 for the two channels at 44.1 kHz, 103.39 at 48 kHz, 101.07 at 16 kHz, 105.10 at 16 kHz in QuickTime's layout and 105.51 and 108.03 at 22.05 kHz; the bound is 6 dB under the lowest.
    const AGREEMENT: f64 = 95.0;
    // The most any sample lay from FFmpeg's in any fixture, measured at 1; the bound is twice that.
    const NEAREST: i32 = 2;
    // The same for the mean of the two channels at 44.1 kHz against the mean of FFmpeg's, measured at 70.56 dB, which the rounding of FFmpeg's two channels before they are added sets; the bound is 6 dB under it.
    const MEAN_AGREEMENT: f64 = 64.0;

    fn assert_matches_ffmpeg(name: &str, played: &[Vec<i16>], reference: &[Vec<i16>]) {
        assert_eq!(played.len(), reference.len(), "{name}: channels");
        for (number, (ours, theirs)) in played.iter().zip(reference).enumerate() {
            assert_eq!(ours.len(), theirs.len(), "{name} channel {number}: as many samples as FFmpeg plays");
            let worst = ours.iter().zip(theirs).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max();
            assert!(worst <= Some(NEAREST), "{name} channel {number}: up to {worst:?} from FFmpeg's samples");
            assert!(ratio(theirs, ours) > AGREEMENT, "{name} channel {number}: {} dB", ratio(theirs, ours));
        }
    }

    #[test]
    fn fixtures_match_ffmpeg() {
        for (name, file, reference_file, channels) in [
            ("44.1 kHz stereo", STEREO, STEREO_FFMPEG, 2),
            ("48 kHz mono", MONO, MONO_FFMPEG, 1),
            ("16 kHz mono", LOW, LOW_FFMPEG, 1),
            ("16 kHz mono in QuickTime's layout", QUICKTIME, QUICKTIME_FFMPEG, 1),
        ] {
            assert_matches_ffmpeg(name, &played(file, true).channels, &reference(reference_file, channels));
        }
        let plain = played(PLAIN, false);
        assert_matches_ffmpeg("22.05 kHz stereo", &plain.channels, &reference(PLAIN_FFMPEG, 2));
        assert_eq!(plain.seen.noise, 0, "and no band of it is noise");
    }

    #[test]
    fn fixtures_reach_every_tool() {
        for (name, file) in
            [("44.1 kHz stereo", STEREO), ("48 kHz mono", MONO), ("16 kHz mono", LOW), ("22.05 kHz stereo", PLAIN)]
        {
            let seen = played(file, false).seen;
            let blocks: usize = seen.sequences.iter().sum();
            assert!(
                seen.sequences.iter().all(|&count| count > 0),
                "{name}: long, start, short and stop windows {seen:?}"
            );
            assert!(0 < seen.kaiser_bessel && seen.kaiser_bessel < blocks, "{name}: both shapes {seen:?}");
            assert!(seen.filters > 0, "{name}: noise shaping {seen:?}");
            if name.contains("stereo") {
                assert!(seen.intensity > 0 && seen.mid_side > 0, "{name}: stereo {seen:?}");
            }
            if !name.contains("22.05") {
                assert!(seen.noise > 0, "{name}: noise {seen:?}");
            }
        }
        assert!(played(STEREO, false).seen.correlated > 0, "bands of noise with the mask set");
    }

    #[test]
    fn masked_noise_shared() {
        let (standard, independent) = (played(STEREO, false), played(STEREO, true));
        assert_ne!(standard.channels, independent.channels, "the encoder sets the mask on its noise");
        assert_eq!(read_track(STEREO).unwrap().sound.lead, FRAME as u32, "a frame of the file's is its lead");
        let (mut same, mut apart) = (0, 0);
        for chunk in 0..standard.channels[0].len() / FRAME {
            // What the file plays from its chunk is what its packet and the one before it complete.
            let frame = chunk * FRAME..(chunk + 1) * FRAME;
            let is_plain = !standard.correlated[chunk + 1] && !standard.correlated[chunk];
            let is_alike = standard
                .channels
                .iter()
                .zip(&independent.channels)
                .all(|(ours, theirs)| ours[frame.clone()] == theirs[frame.clone()]);
            assert!(!is_plain || is_alike, "frame {chunk}: no band of it has the noise of the left channel");
            (same, apart) = (same + usize::from(is_plain), apart + usize::from(!is_alike));
        }
        assert!(same > 0 && apart > 0, "frames with no such band are the same in both, and the others are not");
    }

    #[test]
    fn pair_plays_as_mean() {
        let played = played(STEREO, true);
        let reference = reference(STEREO_FFMPEG, 2);
        let mean: Vec<i16> = reference[0]
            .iter()
            .zip(&reference[1])
            .map(|(&left, &right)| ((f32::from(left) + f32::from(right)) / 2.0 + 0.5).floor() as i16)
            .collect();
        let worst =
            played.mixed.iter().zip(&mean).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max();
        assert!(worst <= Some(NEAREST), "up to {worst:?} from the mean of FFmpeg's channels");
        assert!(ratio(&mean, &played.mixed) > MEAN_AGREEMENT, "{} dB", ratio(&mean, &played.mixed));
    }

    #[test]
    fn stream_packets_match_file_packets() {
        for (file, stream) in [(STEREO, STEREO_STREAM), (MONO, MONO_STREAM)] {
            let (in_file, in_stream) = (read_track(file).unwrap(), read_track(stream).unwrap());
            let configs = [&in_file, &in_stream].map(|track| Config::read(&track.sound.configuration));
            assert!(configs[0].is_ok() && configs[0] == configs[1], "the same stream is told");
            assert_eq!((in_file.sound.lead, in_stream.sound.lead), (1024, 0));
            assert_eq!(in_file.packets.len(), in_stream.packets.len());
            for (number, (one, other)) in in_file.packets.iter().zip(&in_stream.packets).enumerate() {
                let packet = |file: &[u8], sample: &crate::codec::mp4::Sample| {
                    file[sample.at as usize..][..sample.bytes as usize].to_vec()
                };
                assert_eq!(packet(file, one), packet(stream, other), "packet {number}");
            }
            let (kept, all) = (played(file, true), played(stream, true));
            for (kept, all) in kept.channels.iter().zip(&all.channels) {
                assert_eq!(all.len(), in_stream.packets.len() * FRAME, "a stream plays all its packets");
                assert_eq!(kept[..], all[FRAME..FRAME + kept.len()], "behind the lead the file has");
            }
        }
    }

    // The fields of a scalefactor's step from the one before it.
    fn step(delta: i32) -> (u32, u32) {
        split(SCALEFACTORS[(delta + SCALE_REACH) as usize])
    }

    // The fields of a band's lines under a book of codes: the code of each group, the signs of a book that does not hold them behind it.
    fn coded(book: usize, lines: &[i32]) -> Vec<(u32, u32)> {
        let book = &CODE_BOOKS[book - 1];
        let mut fields = Vec::new();
        for group in lines.chunks_exact(book.group) {
            fields.push(book.code(book.index(group)));
            if !book.is_signed {
                fields.extend(group.iter().filter(|&&line| line != 0).map(|&line| (u32::from(line < 0), 1)));
            }
        }
        fields
    }

    // The fields of an ics_info of a long window with that many bands.
    fn long_info(shape: u32, bands: u32) -> Vec<(u32, u32)> {
        vec![(0, 1), (0, 2), (shape, 1), (bands, 6), (0, 1)]
    }

    // The fields of a section of that many bands of one book, a long window's.
    fn section(book: u32, bands: u32) -> [(u32, u32); 2] {
        [(book, 4), (bands, 5)]
    }

    const END_FIELDS: (u32, u32) = (7, 3);

    // A single channel element of a long window, one section of one book, the lines of its bands, and the bits of the tools that are used.
    fn single_element(book: u32, lines: &[i32], bands: u32, tools: &[(u32, u32)], gain: u32) -> Vec<(u32, u32)> {
        let mut fields = vec![(0, 3), (0, 4), (gain, 8)];
        fields.extend(long_info(0, bands));
        fields.extend(section(book, bands));
        fields.extend((0..bands).map(|_| step(0)));
        fields.extend_from_slice(tools);
        for band in lines.as_chunks::<4>().0 {
            fields.extend(coded(book as usize, band));
        }
        fields
    }

    #[test]
    fn pulses_add_before_scaling() {
        let pulses = [(1, 1), (2, 2), (0, 6), (0, 5), (2, 4), (3, 5), (5, 4), (1, 5), (4, 4), (0, 1), (0, 1)];
        let mut fields = single_element(1, &[1, 0, 0, -1, 0, 0, 0, 0], 2, &pulses, 100);
        fields.push(END_FIELDS);
        let mut decoder = Decoder::new();
        assert_eq!(decoder.elements(&written(&fields)), Ok(()));
        let lifted = |amount: f64| amount.powf(4.0 / 3.0) as f32;
        let expected = [lifted(3.0), 0.0, 0.0, -lifted(6.0), -lifted(4.0), 0.0, 0.0, 0.0];
        for (at, (&actual, &expected)) in decoder.channels[0].spectrum.iter().zip(&expected).enumerate() {
            assert!((actual - expected).abs() < 1.0e-4, "line {at}: {actual} for {expected}");
        }
        assert!(decoder.channels[0].spectrum[8..].iter().all(|&line| line == 0.0));
        assert_eq!(decoder.seen().pulses, 3);
        let mut beyond =
            single_element(1, &[1, 0, 0, -1], 1, &[(1, 1), (0, 2), (0, 6), (31, 5), (15, 4), (0, 1), (0, 1)], 100);
        beyond.push(END_FIELDS);
        assert_eq!(decoder.elements(&written(&beyond)), Ok(()), "a pulse past the bands that are coded is passed over");
        assert_eq!(decoder.seen().pulses, 3);
        let off = [(1, 1), (0, 2), (63, 6), (0, 5), (0, 4), (0, 1), (0, 1)];
        let mut after = single_element(1, &[1, 0, 0, -1], 1, &off, 100);
        after.push(END_FIELDS);
        assert_eq!(decoder.elements(&written(&after)), Err(Flaw::Broken), "a pulse in a band there is none of");
        // The largest escaped line, 8191, with every pulse of a pulse_data at its largest on it.
        let stacked = [(1, 1), (3, 2), (0, 6), (0, 5), (15, 4), (0, 5), (15, 4), (0, 5), (15, 4), (0, 5), (15, 4)];
        let mut most = single_element(BOOKS as u32, &[], 1, &[&stacked[..], &[(0, 1), (0, 1)]].concat(), 100);
        most.extend(coded(BOOKS, &[ESCAPE as i32, 0]));
        most.extend([(0xFF, 8), (0, 1), (0xFFF, 12)]);
        most.extend(coded(BOOKS, &[0, 0]));
        most.push(END_FIELDS);
        assert_eq!(decoder.elements(&written(&most)), Ok(()), "every pulse on the largest line");
        let expected = f64::from(LARGEST_VALUE + MOST_PULSES * LARGEST_PULSE).powf(4.0 / 3.0);
        let actual = f64::from(decoder.channels[0].spectrum[0]);
        assert!((actual - expected).abs() <= expected * 1.0e-5, "{actual} for {expected}");
    }

    // The lines of a long window under a book of pairs: 24 lines of the six lowest bands, of values up to 4.
    fn tonal_lines() -> Vec<i32> {
        (0..24).map(|at| (at * 5 + 3) % 9 - 4).collect()
    }

    // What a filter of that order and those reflection indices does to lines as the standard writes it, in f64: the coefficients by its step-up recursion, the lines run from the lowest or from the highest.
    fn filtered(lines: &[f64], indices: &[i32], resolution: u32, down: bool) -> Vec<f64> {
        let steps = f64::from(1u32 << (resolution - 1));
        let reflections: Vec<f64> = indices
            .iter()
            .map(|&index| {
                (f64::from(index) * core::f64::consts::FRAC_PI_2 / if index >= 0 { steps - 0.5 } else { steps + 0.5 })
                    .sin()
            })
            .collect();
        let mut lpc = vec![1.0];
        for (order, &reflection) in reflections.iter().enumerate() {
            let before = lpc.clone();
            lpc.push(reflection);
            for place in 1..=order {
                lpc[place] = before[place] + reflection * before[order + 1 - place];
            }
        }
        let mut made = lines.to_vec();
        let at = |step: usize| if down { lines.len() - 1 - step } else { step };
        for step in 0..lines.len() {
            let sum: f64 = (1..=indices.len().min(step))
                .map(|back| lpc[back] * made[if down { at(step) + back } else { at(step) - back }])
                .sum();
            made[at(step)] = lines[at(step)] - sum;
        }
        made
    }

    #[test]
    fn long_window_noise_shaping() {
        for (resolution, compress, down, indices) in [
            (1u32, 0u32, 0u32, vec![1, -3]),
            (0, 1, 1, vec![1, -2, 1]),
            (1, 1, 1, vec![3, -2, -4, 1]),
            (0, 0, 0, vec![-4, 3, 2, 1, -1]),
        ] {
            let bits = 3 + resolution - compress;
            let mut tools = vec![
                (0, 1),
                (1, 1),
                (1, 2),
                (resolution, 1),
                (49, 6),
                (indices.len() as u32, 5),
                (down, 1),
                (compress, 1),
            ];
            tools.extend(indices.iter().map(|&index| (index as u32 & ((1 << bits) - 1), bits)));
            tools.push((0, 1));
            let mut fields = single_element(5, &tonal_lines(), 6, &tools, 100);
            fields.push(END_FIELDS);
            let mut decoder = Decoder::new();
            assert_eq!(decoder.elements(&written(&fields)), Ok(()));
            let plain: Vec<f64> = tonal_lines()
                .iter()
                .map(|&line| f64::from(line.signum()) * f64::from(line.unsigned_abs()).powf(4.0 / 3.0))
                .collect();
            let expected = filtered(&plain, &indices, 3 + resolution, down == 1);
            for (at, (&actual, expected)) in decoder.channels[0].spectrum.iter().zip(&expected).enumerate() {
                assert!(
                    (f64::from(actual) - expected).abs() < 1.0e-4 * (1.0 + expected.abs()),
                    "{resolution} {compress} {down} line {at}: {actual} for {expected}"
                );
            }
            assert_eq!(decoder.seen().filters, 1);
        }
    }

    // An ics_info of eight short windows of that shape and bands, the windows grouped as the seven bits say.
    fn short_info(shape: u32, bands: u32, grouping: u32) -> Vec<(u32, u32)> {
        vec![(0, 1), (2, 2), (shape, 1), (bands, 4), (grouping, 7)]
    }

    #[test]
    fn short_windows_in_groups() {
        // Windows 0 and 1 form a group, 2 and 3 another, and each of the other four a group of its own.
        let (grouping, lengths) = (0b101_0000, [2, 2, 1, 1, 1, 1]);
        let value = |window: usize, band: usize, at: usize| ((window * 7 + band * 3 + at) % 3) as i32 - 1;
        let mut fields = vec![(0, 3), (0, 4), (100, 8)];
        fields.extend(short_info(0, 2, grouping));
        fields.extend((0..lengths.len()).flat_map(|_| [(1, 4), (2, 3)]));
        fields.extend((0..2 * lengths.len()).map(|_| step(0)));
        fields.extend([(0, 1), (1, 1)]);
        // Windows 0 to 2 have no filters; window 3 has one: of resolution 3, of length 14 bands, which is all there are, of order 2, running down.
        fields.extend([(0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (14, 4), (2, 3), (1, 1), (0, 1)]);
        let indices = [1, -3];
        fields.extend(indices.iter().map(|&index| (index as u32 & 0b111, 3)));
        // The four windows behind it have none, and there is no gain control.
        fields.extend([(0, 1), (0, 1), (0, 1), (0, 1), (0, 1)]);
        let mut first = 0;
        for &length in &lengths {
            for band in 0..2 {
                for window in first..first + length {
                    fields.extend(coded(1, &(0..4).map(|at| value(window, band, at)).collect::<Vec<_>>()));
                }
            }
            first += length;
        }
        fields.push(END_FIELDS);
        let mut decoder = Decoder::new();
        assert_eq!(decoder.elements(&written(&fields)), Ok(()));
        let spectrum = &decoder.channels[0].spectrum;
        for window in 0..8 {
            let plain: Vec<f64> = (0..8).map(|line| f64::from(value(window, line / 4, line % 4))).collect();
            let expected = if window == 3 { filtered(&plain, &indices, 3, true) } else { plain };
            for (line, expected) in expected.iter().enumerate() {
                let actual = f64::from(spectrum[window * SHORT + line]);
                assert!((actual - expected).abs() < 1.0e-4, "window {window} line {line}: {actual} for {expected}");
            }
            assert!(spectrum[window * SHORT + 8..(window + 1) * SHORT].iter().all(|&line| line == 0.0));
        }
    }

    #[test]
    fn reset_decodes_as_new() {
        let track = read_track(STEREO).unwrap();
        let config = Config::read(&track.sound.configuration).unwrap();
        let (mut used, mut fresh) = (Decoder::of(config), Decoder::of(config));
        let (mut one, mut other) = ([0i16; FRAME], [0i16; FRAME]);
        let packet =
            |number: usize| &STEREO[track.packets[number].at as usize..][..track.packets[number].bytes as usize];
        for number in 0..12 {
            assert_eq!(used.decode(packet(number), &mut one), Ok(()));
        }
        used.reset();
        for number in 0..track.packets.len() {
            assert_eq!(
                (used.decode(packet(number), &mut one), fresh.decode(packet(number), &mut other)),
                (Ok(()), Ok(()))
            );
            assert_eq!(one, other, "packet {number}");
        }
    }

    #[test]
    fn band_room_holds_every_window() {
        let most = LONG_BANDS.iter().chain(&SHORT_BANDS).map(|bands| bands.len() - 1).max();
        assert_eq!(most, Some(MAX_BANDS), "no window has more bands than there is room for");
    }

    // The configuration of two channels at 48 kHz.
    fn pair_decoder() -> Decoder {
        Decoder::of(Config::read(&[0x11, 0x90]).unwrap())
    }

    #[test]
    fn elements_must_match_configuration() {
        let mut plain = single_element(1, &[1, 0, 0, -1], 1, &[(0, 1), (0, 1), (0, 1)], 100);
        plain.push(END_FIELDS);
        let mut out = [1i16; FRAME];
        let mut pair = pair_decoder();
        assert_eq!(pair.decode(&written(&plain), &mut out), Err(Flaw::Broken), "one channel where two are told");
        assert_eq!(out, [0; FRAME]);
        let mut two = vec![(0, 3), (0, 4)];
        two.extend_from_slice(&plain[2..plain.len() - 1]);
        two.push((0, 3));
        two.extend_from_slice(&plain[1..]);
        assert_eq!(pair.decode(&written(&two), &mut out), Ok(()), "two single channel elements are a pair's channels");
        let mut mono = Decoder::new();
        assert_eq!(
            mono.decode(&written(&two), &mut out),
            Err(Flaw::Unsupported("more channels than the configuration has"))
        );
    }

    // A channel pair element of a long window of two bands, common to both channels, with that mask; each channel's fields follow: its gain, sections, scalefactor steps, the bits of its tools, and its lines.
    fn pair_element(mask: &[(u32, u32)], channels: [Vec<(u32, u32)>; 2]) -> Vec<(u32, u32)> {
        let mut fields = vec![(1, 3), (0, 4), (1, 1)];
        fields.extend(long_info(0, 2));
        fields.extend_from_slice(mask);
        fields.extend(channels.into_iter().flatten());
        fields.push(END_FIELDS);
        fields
    }

    // One channel of a pair of two bands: the gain, one section for each band, the steps, no tools, the lines of bands under book 1.
    fn pair_channel(gain: u32, books: [u32; 2], steps: [i32; 2], lines: [&[i32]; 2]) -> Vec<(u32, u32)> {
        let mut fields = vec![(gain, 8), (books[0], 4), (1, 5), (books[1], 4), (1, 5)];
        fields.extend(steps.map(step));
        fields.extend([(0, 1), (0, 1), (0, 1)]);
        for (book, lines) in books.iter().zip(lines) {
            if (1..=BOOKS as u32).contains(book) {
                fields.extend(coded(*book as usize, lines));
            }
        }
        fields
    }

    #[test]
    fn mid_side_and_intensity() {
        let left = [1, -1, 0, 1, 1, 1, -1, -1];
        let (silent, quiet) = (&[][..], &[1, 0, 1, 0][..]);
        let right = |books, steps, lines| pair_channel(100, books, steps, lines);
        // Band 0 of the right channel is intensity in phase, at position 4, a factor of a half; the mask is set for band 0 and not for 1.
        let mut decoder = pair_decoder();
        let fields = pair_element(
            &[(1, 2), (1, 1), (0, 1)],
            [pair_channel(100, [1, 1], [0, 0], [&left[..4], &left[4..]]), right([15, 1], [4, 0], [silent, quiet])],
        );
        assert_eq!(decoder.elements(&written(&fields)), Ok(()));
        let [first, second] = &decoder.channels;
        let inverted: Vec<f32> = left[..4].iter().map(|&line| -0.5 * line as f32).collect();
        assert_eq!(second.spectrum[..4], inverted[..], "the mask inverts the phase of the band of intensity");
        assert_eq!(
            second.spectrum[4..8],
            [1.0, 0.0, 1.0, 0.0],
            "and where it is not set the right channel is as coded"
        );
        assert_eq!(first.spectrum[..8], left.map(|line| line as f32), "the left channel is as coded");
        assert_eq!(decoder.seen().intensity, 1);
        // The mask is set for band 1 and not for band 0: band 1 is mid and side, band 0 is intensity as it stands.
        let fields = pair_element(
            &[(1, 2), (0, 1), (1, 1)],
            [pair_channel(100, [1, 1], [0, 0], [&left[..4], &left[4..]]), right([14, 1], [4, 0], [silent, quiet])],
        );
        let mut decoder = pair_decoder();
        assert_eq!(decoder.elements(&written(&fields)), Ok(()));
        let [first, second] = &decoder.channels;
        let out_of_phase: Vec<f32> = left[..4].iter().map(|&line| -0.5 * line as f32).collect();
        assert_eq!(second.spectrum[..4], out_of_phase[..], "out of phase");
        assert_eq!(first.spectrum[4..8], [2.0, 1.0, 0.0, -1.0], "mid plus side");
        assert_eq!(second.spectrum[4..8], [0.0, 1.0, -2.0, -1.0], "mid minus side");
        assert_eq!(decoder.seen().mid_side, 1);
        // A mask of every band, as the 2 says.
        let all = pair_element(
            &[(2, 2)],
            [
                pair_channel(100, [1, 1], [0, 0], [&left[..4], &left[4..]]),
                right([1, 1], [0, 0], [&[1, 1, 0, 0], quiet]),
            ],
        );
        let mut decoder = pair_decoder();
        assert_eq!(decoder.elements(&written(&all)), Ok(()));
        assert_eq!(decoder.seen().mid_side, 2);
        assert_eq!(decoder.channels[0].spectrum[..4], [2.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn noise_in_pairs_by_mask() {
        // Both bands of both channels are noise, in one section: the first energy is told in nine bits as its difference from the gain less 90, which is 10, and the second band's is a step of none from it: 18 for the left channel, 6 for the right.
        let build = |mask: &[(u32, u32)]| {
            let mut fields = vec![(1, 3), (0, 4), (1, 1)];
            fields.extend(long_info(0, 2));
            fields.extend_from_slice(mask);
            for first in [256 + 8, 256 - 4] {
                fields.extend([(100, 8), (13, 4), (2, 5), (first, 9), step(0), (0, 1), (0, 1), (0, 1)]);
            }
            fields.push(END_FIELDS);
            written(&fields)
        };
        let mut decoder = pair_decoder();
        assert_eq!(decoder.elements(&build(&[(2, 2)])), Ok(()));
        let [left, right] = &decoder.channels;
        // 2 to the 18th over 4 against the 6th over 4: the right's gain is an eighth of the left's.
        for at in 0..8 {
            assert!(
                (right.spectrum[at] - left.spectrum[at] / 8.0).abs() < 1.0e-4 * left.spectrum[at].abs(),
                "line {at}"
            );
        }
        let energy: f32 = left.spectrum[..4].iter().map(|line| line * line).sum();
        assert!((energy / 512.0 - 1.0).abs() < 1.0e-4, "the band's energy is 2 to the 18th over 2: {energy}");
        assert_eq!(decoder.seen().correlated, 2);
        let mut alone = pair_decoder();
        assert_eq!(alone.elements(&build(&[(0, 2)])), Ok(()));
        let [left, right] = &alone.channels;
        assert!((0..4).any(|at| (right.spectrum[at] - left.spectrum[at] / 8.0).abs() > 0.01 * left.spectrum[at].abs()));
        assert_eq!(alone.seen().correlated, 0);
        let mut other = pair_decoder();
        assert_eq!(other.elements(&build(&[(1, 2), (1, 1), (0, 1)])), Ok(()));
        assert_eq!(other.seen().correlated, 1, "a mask that is read sets it for each band on its own");
    }

    #[test]
    fn elements_without_sound_skipped() {
        let mut plain = single_element(1, &[1, 0, 0, -1], 1, &[(0, 1), (0, 1), (0, 1)], 100);
        plain.push(END_FIELDS);
        let mut decoder = Decoder::new();
        assert_eq!(decoder.elements(&written(&plain)), Ok(()));
        let expected = decoder.channels[0].spectrum.clone();
        // A program config of one front element, the bits that end its byte and a comment of two bytes; a data stream of three bytes, which begins at a byte and whose count ends at one, so that its alignment adds none; a fill element of two bytes of an extension that is the filling's; and one whose count of 15 says that its own follows in eight bits, 3, of which one less is added: 17 bytes.
        let mut padded = vec![
            (5, 3),
            (0, 4),
            (1, 2),
            (3, 4),
            (1, 4),
            (0, 4),
            (0, 4),
            (0, 2),
            (0, 3),
            (0, 4),
            (0, 1),
            (0, 1),
            (0, 1),
        ];
        padded.extend([(1, 1), (0, 4), (0, 6), (2, 8), (0x12, 8), (0x34, 8)]);
        padded.extend([(4, 3), (5, 4), (1, 1), (3, 8), (0xAA, 8), (0xBB, 8), (0xCC, 8)]);
        padded.extend([(6, 3), (2, 4), (0, 4), (0xFF, 8), (0, 4)]);
        padded.extend([(6, 3), (15, 4), (3, 8)]);
        padded.extend((0..17).map(|_| (0x55, 8)));
        // And one whose own count is 0: 14 bytes.
        padded.extend([(6, 3), (15, 4), (0, 8)]);
        padded.extend((0..14).map(|_| (0x55, 8)));
        padded.extend(plain);
        let mut decoder = Decoder::new();
        assert_eq!(decoder.elements(&written(&padded)), Ok(()));
        assert_eq!(decoder.channels[0].spectrum, expected);
        padded.truncate(6);
        assert_eq!(decoder.elements(&written(&padded)), Err(Flaw::Short), "an element cut short");
    }
}
