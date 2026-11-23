//! The MT6765's H.264 decoder, programmed picture by picture as Rabbit's driver does.

use crate::codec::Flaw;
use crate::codec::h264::MACROBLOCK;
use crate::codec::h264::SMALLEST_COUNT_BITS;
use crate::codec::h264::dpb::Dpb;
use crate::codec::h264::dpb::Entry;
use crate::codec::h264::dpb::Output;
use crate::codec::h264::dpb::Target;
use crate::codec::h264::dpb::dpb_size;
use crate::codec::h264::read::Bits;
use crate::codec::h264::read::Header;
use crate::codec::h264::read::Order;
use crate::codec::h264::read::Picture;
use crate::codec::h264::read::Sequence;
use crate::codec::h264::read::Sets;
use crate::codec::h264::read::Slice;
use crate::hw::Block;
use crate::hw::Hardware;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const VLD: usize = 0x0000;
const VLD_TOP: usize = 0x0800;
const MC: usize = 0x1000;
const AVC_VLD: usize = 0x2000;
const AVC_MV: usize = 0x3000;
const MISC: usize = 0x5000;

const FEED_START: usize = VLD + 0xB4;
const FEED_END: usize = VLD + 0xB8;
const FEED_READ: usize = VLD + 0xB0;
const FEED_WRITE: usize = VLD + 0x110;
const FEED_TRIGGER: usize = VLD + 0x8C;
const FEED_FETCH: usize = VLD + 0xE8;
const FEED_BUSY: usize = VLD + 0xF4;
const FEED_RESET: usize = VLD + 0x108;
const FEED_UNIT_SHIFT: u32 = 6;
const FETCH: u32 = 0x10_0000;
const FILL_WINDOW: u32 = 0x80_0000;
const FETCH_BUSY: u32 = 1 << 15;
const FEED_REST: u32 = 1;
const HOLD_IN_RESET: u32 = 0x101;
const RESTART: u32 = 0x100;
const WINDOW: usize = AVC_VLD;
const SHIFT_LIMIT: usize = 32;
const READER_MODE: usize = AVC_VLD + 0x84;
const READER_HOLD: usize = AVC_VLD + 0x28C;
const FIND_START_CODE: usize = AVC_VLD + 0x2D8;
const SEARCH_STATUS: usize = AVC_VLD + 0x2C8;
const CLEAR_LISTS: usize = AVC_VLD + 0x228;
const READ_CHANGES: usize = AVC_VLD + 0x22C;
const READ_WEIGHTS: usize = AVC_VLD + 0x230;
const START_ARITHMETIC: usize = AVC_VLD + 0x234;
const ARITHMETIC_STATE: usize = AVC_VLD + 0x25C;
const DECODE: usize = AVC_VLD + 0x238;
const ARITHMETIC_BUSY: u32 = 1 << 25;
const SEARCH_FOUND: u32 = 0x800;
const SEQUENCE_WORD: usize = AVC_VLD + 0x88;
const FIRST_MACROBLOCK: usize = AVC_VLD + 0x8C;
const PICTURE_WORD: usize = AVC_VLD + 0x90;
const LISTS_WORD: usize = AVC_VLD + 0x94;
const SLICE_WORD: usize = AVC_VLD + 0x98;
const FILTER_WORD: usize = AVC_VLD + 0x9C;
const NUMBERS: usize = AVC_VLD + 0xA0;
const NUMBER: usize = AVC_VLD + 0xA4;
const LAST_MACROBLOCK: usize = VLD_TOP + 0x68;
const BIT_DEPTHS: usize = VLD_TOP + 0x100;
const LIST_ADDRESSES: [usize; 3] = [MC + 0x3DC, MC + 0x45C, MC + 0x4DC];
const LIST_NUMBERS: [usize; 3] = [AVC_VLD + 0xA8, AVC_VLD + 0x128, AVC_VLD + 0x1A8];
const LIST_FIELDS: [usize; 3] = [AVC_MV, AVC_MV + 0x80, AVC_MV + 0x100];
const LIST_MOTION: usize = AVC_MV + 0x180;
const LIST_MARKS: [usize; 3] = [MC + 0x564, MC + 0x568, MC + 0x56C];
const LIST_SIZE: usize = 32;
const ENTRY_LIMIT: usize = 16;
const PICTURE_ORDER: usize = AVC_MV + 0x200;
const TOP_ORDER: usize = AVC_MV + 0x204;
const BOTTOM_ORDER: usize = AVC_MV + 0x208;
const PICTURE_MOTION: usize = AVC_MV + 0x20C;
const MOTION_MODE: usize = AVC_MV + 0x220;
const REDUCED_MOTION: u32 = 0x20;
const SECOND_FIELD_REDUCED: u32 = 2;
const SECOND_FIELD_FULL: u32 = 4;
const ORDER_MASK: u32 = 0x3_FFFF;
const NUMBER_MASK: u32 = 0x7_FFFF;
const FIELD_SLOT_SHIFT: u32 = 23;
const FIELD_BOTTOM: u32 = 1 << 22;
const FIELD_TOP_FIRST: u32 = 1 << 21;
const PICTURE_WIDTH: usize = MC + 0x208;
const PICTURE_HEIGHT: usize = MC + 0x20C;
const PICTURE_SIZE: usize = VLD_TOP + 0x70;
const MACROBLOCKS: usize = VLD_TOP + 0x6C;
const LUMA: usize = MC + 0x560;
const CHROMA_OFFSET: usize = MC + 0x55C;
const LINES: [usize; 2] = [MC + 0xA20, MC + 0xA24];
const READER_BUFFER: usize = VLD_TOP + 0x28;
const PICTURE_START: usize = MISC + 0x3C;
const PICTURE_MIDDLE: usize = MISC + 0x40;
const MOTION_START: usize = MISC + 0x54;
const MOTION_END: usize = MISC + 0x58;
const COLUMN_DONE: usize = MC + 0x28;
const ROW_DONE: usize = MC + 0x2C;
const ERROR_MASK: usize = AVC_VLD + 0x280;
const ERRORS: usize = AVC_VLD + 0x284;
const ERROR_BITS: u32 = 0xBFDF_B0F7;
const INTERRUPT: usize = MISC + 0xA4;
const CLOCKS: usize = MISC;
const STOP: usize = MISC + 0x100;
const STOP_STATUS: usize = MISC + 0x104;
const STOP_BITS: u32 = 0x11;
const CLOCKS_MASK: u32 = 0xFF40_0000;
const CLOCKS_ON: u32 = 0x0080_1FF0;
const MISC_04: usize = MISC + 0x4;
const MISC_84: usize = MISC + 0x84;
const MISC_F0: usize = MISC + 0xF0;
const MC_5E4: usize = MC + 0x5E4;
const RESET_WRITES: [(usize, u32); 9] = [
    (MISC + 0xC8, 0x1F7),
    (MISC + 0xCC, 0x53E3_0182),
    (MISC + 0x178, 0xFFFF_FFE1),
    (MISC + 0xF4, 0),
    (MISC_84, 2),
    (FEED_RESET, 0),
    (MISC + 0xEC, 1),
    (MISC_04, 0x10),
    (MISC + 0x160, 0x0065_FF10),
];
const END_WRITES: [(usize, u32); 2] = [(MISC + 0xC8, 0xF7), (MISC + 0xCC, 0x13A2_0100)];
const HEAD_MODE: u32 = 0xAE00_E007;
const DECODE_MODE: u32 = 0xAE80_E007;
const ANSWER_READS: u32 = 50_000;
const FETCH_READS: u32 = 0x1001;
const SEARCH_READS: u32 = 0x10_0000;
// Milliseconds before the first poll: four kernel ticks; the slowest clean picture took 8.3 ms (1920x1088).
const FIRST_WAIT_MS: u32 = 16;
const POLL_MS: u32 = 4;
const TIMEOUT_MS: u32 = 200;
const STREAM_SLACK: usize = 0x100;
const START_CODE: [u8; 4] = [0, 0, 0, 1];
const END_OF_PICTURE: u8 = 9;
/// Bytes of the bit reader's buffer.
pub const READER_BYTES: usize = 0x8000;
/// Bytes of the two buffers of the line above.
pub const LINE_BYTES: [usize; 2] = [0xF000, 0x1_E000];
const ROW_UNIT: u32 = 2;
pub(super) const BLOCK_COLUMNS: u32 = 16;
pub(super) const LUMA_BLOCK_LINES: u32 = 32;
pub(super) const CHROMA_BLOCK_LINES: u32 = 16;
const MOTION_BYTES: usize = 64;
const MOTION_EXTRA: usize = 0x200;
const MIDDLE_UNIT: u32 = 0x200;
const MIN_SIDE: u32 = 4;
const MAX_COLUMNS: u32 = 128;
const MAX_MACROBLOCKS: u32 = 8160;
/// The error text of a sample begun while a picture is in work.
pub const BUSY_TEXT: &str = "the picture before this one was not ended";
/// The log line of a picture that would not stop.
pub const UNSTOPPED_LINE: &str = "decoder: a picture would not stop; the codec block stays taken";
/// The log line of a picture that would not stop, with its buffers held.
pub const STUCK_LINE: &str =
    "decoder: a picture would not stop; the codec block and its buffers stay taken until it does";
const STOPPED_LINE: &str = "decoder: a picture that would not stop has stopped; the codec block is let go of";

/// The log line after a retry left `remaining` of `count` decoders stuck.
pub const fn to_retry_line(count: usize, remaining: usize) -> Option<&'static str> {
    if remaining < count { Some(STOPPED_LINE) } else { None }
}

/// The error text of a stream the decoder does not take.
pub fn to_unfit_text(sequence: &Sequence) -> String {
    format!("pictures of {}x{} macroblocks", sequence.columns, sequence.rows)
}

/// How the decoder stores a stream's pictures.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    columns: u32,
    rows: u32,
    pub(super) stride: u32,
    pub(super) lines: u32,
}

impl Layout {
    /// The layout of a stream's pictures; none if the decoder does not take them.
    pub fn from_sequence(sequence: &Sequence) -> Option<Layout> {
        let (columns, rows) = (sequence.columns, sequence.rows);
        let is_supported = (MIN_SIDE..=MAX_COLUMNS).contains(&columns)
            && rows >= MIN_SIDE
            && u64::from(columns) * u64::from(rows) <= u64::from(MAX_MACROBLOCKS);
        is_supported.then(|| Layout {
            columns,
            rows,
            stride: columns * MACROBLOCK,
            lines: rows.next_multiple_of(ROW_UNIT) * MACROBLOCK,
        })
    }

    const fn luma_bytes(&self) -> usize {
        (self.stride * self.lines) as usize
    }

    pub const fn picture_bytes(&self) -> usize {
        self.luma_bytes() * 3 / 2
    }

    /// Bytes of the stream buffer: a picture's bytes and `STREAM_SLACK`.
    pub const fn stream_bytes(&self) -> usize {
        self.picture_bytes() + STREAM_SLACK
    }

    /// Bytes of a picture's motion buffer.
    pub const fn motion_bytes(&self) -> usize {
        (self.columns * self.lines / MACROBLOCK) as usize * MOTION_BYTES + MOTION_EXTRA
    }

    pub(super) const fn at(&self, is_chroma: bool, column: u32, line: u32) -> usize {
        let (lines, start) = if is_chroma { (CHROMA_BLOCK_LINES, self.luma_bytes()) } else { (LUMA_BLOCK_LINES, 0) };
        let block = line / lines * (self.stride / BLOCK_COLUMNS) + column / BLOCK_COLUMNS;
        start + (block * BLOCK_COLUMNS * lines + line % lines * BLOCK_COLUMNS + column % BLOCK_COLUMNS) as usize
    }
}

/// The device addresses of the decoder's buffers.
#[derive(Clone, Copy, Debug)]
pub struct Work<'a> {
    /// The picture's slices behind start codes, and `STREAM_SLACK` bytes more.
    pub stream: u32,
    /// `READER_BYTES` for the bit reader.
    pub reader: u32,
    /// `LINE_BYTES` for the line above.
    pub lines: [u32; 2],
    /// A motion buffer for each slot.
    pub motion: &'a [u32],
    pub pictures: &'a [u32],
}

/// A command the decoder did not answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stall {
    // The picture is not one the decoder takes, or a buffer is missing for it.
    Unsupported,
    Fetch,
    StartCode,
    Lists,
    Changes,
    Weights,
    Arithmetic,
}

/// How a picture ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ending {
    is_complete: bool,
    errors: u32,
    column: u32,
    row: u32,
}

/// How the start of a sample went, short of a system failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartOutcome {
    // The decoder works on the sample's picture, or the sample has none.
    Done,
    // Every picture buffer is in use: the sample starts again once one is given back.
    Full,
    Unsupported(Unsupported),
}

/// Why the decoder can make no picture of a sample.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
    Unreadable(Flaw),
    // Its layout or frame count differs from what the buffers were made for.
    Mismatch,
    // Its slices are larger than its picture.
    TooLarge,
}

/// The result of one sample.
#[derive(Default, Debug)]
pub struct Outcome {
    /// The pictures due to show, oldest first; each buffer stays out until it is given back.
    pub outputs: Vec<Output>,
    /// The buffer of the sample's picture; none for a sample without one.
    pub buffer: Option<usize>,
    /// How the decoder ended the picture; none if it never did.
    pub ending: Option<Ending>,
    /// A start command the decoder did not answer.
    pub stall: Option<Stall>,
    is_in_step: bool,
    /// Microseconds the codec block was held for the sample.
    pub lock_micros: u64,
}

impl Outcome {
    /// True for a sample with a picture that did not end complete, in step and without an error.
    pub fn is_flawed(&self) -> bool {
        let is_clean = self.stall.is_none()
            && self.is_in_step
            && self.ending.is_some_and(|ending| ending.is_complete && ending.errors == 0);
        self.buffer.is_some() && !is_clean
    }
}

fn slices<'a>(units: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = (&'a [u8], Header)> {
    units.filter_map(|unit| Header::parse(unit).filter(|header| header.is_slice()).map(|header| (unit, header)))
}

fn write_slices<'a>(units: impl Iterator<Item = &'a [u8]>, buffer: &mut [u8]) -> Option<usize> {
    let mut at = 0;
    for (unit, _) in slices(units) {
        let end = at + START_CODE.len() + unit.len();
        let (code, body) = buffer.get_mut(at..end)?.split_at_mut(START_CODE.len());
        code.copy_from_slice(&START_CODE);
        body.copy_from_slice(unit);
        at = end;
    }
    let slack = buffer.get_mut(at..at + STREAM_SLACK)?;
    slack.fill(0);
    slack[..START_CODE.len()].copy_from_slice(&START_CODE);
    slack[START_CODE.len()] = END_OF_PICTURE;
    Some(at)
}

/// A stream in decoding: its layout, buffer counts, parameter sets and frames.
pub struct Stream {
    layout: Layout,
    capacity: usize,
    pictures: usize,
    sets: Sets,
    dpb: Dpb,
}

impl Stream {
    /// A stream whose buffers hold its frames, one picture in work and `spare` more out; none for pictures the decoder does not take.
    pub fn new(sequence: &Sequence, spare: usize) -> Option<Stream> {
        let (layout, (capacity, _)) = (Layout::from_sequence(sequence)?, dpb_size(sequence));
        let pictures = capacity + 1 + spare;
        Some(Stream { layout, capacity, pictures, sets: Sets::default(), dpb: Dpb::new(pictures) })
    }

    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// The counts of motion and picture buffers.
    pub const fn buffers(&self) -> (usize, usize) {
        (self.capacity + 1, self.pictures)
    }

    /// True if its buffers hold the pictures of a stream with these sequence parameters.
    pub fn fits(&self, sequence: &Sequence) -> bool {
        Layout::from_sequence(sequence) == Some(self.layout) && dpb_size(sequence).0 <= self.capacity
    }

    /// Forgets the parameter sets, the frames and the pictures out, for another stream that fits the buffers.
    pub fn reset(&mut self) {
        (self.sets, self.dpb) = (Sets::default(), Dpb::new(self.pictures));
    }

    /// Takes a parameter set from outside the samples.
    pub fn add_set(&mut self, unit: &[u8]) -> Result<(), Flaw> {
        self.sets.add(unit).map(drop)
    }

    /// Writes a sample's picture into `buffer`, for the decoder to start on; else how the sample went without it.
    pub fn begin<'a>(&'a mut self, units: &[&'a [u8]], stamp: u64, buffer: &mut [u8]) -> Result<Job<'a>, StartOutcome> {
        let unreadable = |flaw| StartOutcome::Unsupported(Unsupported::Unreadable(flaw));
        units.iter().try_for_each(|unit| self.add_set(unit)).map_err(unreadable)?;
        let (first, header) = slices(units.iter().copied()).next().ok_or(StartOutcome::Done)?;
        let slice = Slice::read(first, &self.sets).map_err(unreadable)?;
        let (sequence, picture) = self.sets.get(slice.picture).map_err(unreadable)?;
        if !self.fits(sequence) {
            return Err(StartOutcome::Unsupported(Unsupported::Mismatch));
        }
        let bytes =
            write_slices(units.iter().copied(), buffer).ok_or(StartOutcome::Unsupported(Unsupported::TooLarge))?;
        let mut outputs = Vec::new();
        let target = self.dpb.begin(sequence, header, &slice, stamp, &mut outputs).ok_or(StartOutcome::Full)?;
        Ok(Job { sequence, picture, layout: self.layout, unit: first, header, slice, bytes, target, outputs })
    }

    /// How the picture in work ended, once its interrupt has come.
    pub fn ending<H: Hardware>(&self, hw: &mut H) -> Ending {
        finish(hw, &self.layout)
    }

    /// Ends the sample begun last, adding the pictures now due.
    pub fn finish(&mut self, outcome: &mut Outcome) {
        self.dpb.finish(&mut outcome.outputs);
    }

    /// The pictures still due when the stream has ended.
    pub fn drain(&mut self) -> Vec<Output> {
        let mut outputs = Vec::new();
        self.dpb.drain(&mut outputs);
        outputs
    }

    /// Forgets the frames, to decode from a key picture; pictures out stay out.
    pub fn rewind(&mut self) {
        self.dpb.clear();
    }

    pub fn give_back(&mut self, buffer: usize) {
        self.dpb.give_back(buffer);
    }
}

/// A sample's picture in the stream buffer, for the decoder to start on.
pub struct Job<'a> {
    sequence: &'a Sequence,
    picture: &'a Picture,
    layout: Layout,
    unit: &'a [u8],
    header: Header,
    slice: Slice,
    bytes: usize,
    target: Target,
    outputs: Vec<Output>,
}

impl Job<'_> {
    /// Bytes of the stream buffer the decoder reads.
    pub const fn feed_bytes(&self) -> usize {
        self.bytes + STREAM_SLACK
    }

    /// Starts the decoder under the held codec lock; returns the sample's outcome so far.
    pub fn start<H: Hardware>(self, hw: &mut H, work: &Work) -> Outcome {
        let result = start(hw, work, &self);
        let (stall, is_in_step) = (result.err(), result == Ok(true));
        Outcome { outputs: self.outputs, buffer: Some(self.target.buffer), stall, is_in_step, ..Outcome::default() }
    }
}

/// The wait for a picture's interrupt: polls its position and breaks off a picture that stopped moving or outlasted the timeout.
#[derive(Default)]
pub struct Watch {
    last: Option<((u32, u32), u32)>,
}

impl Watch {
    /// Milliseconds to wait for the interrupt next: the first wait with no `position` looked at yet, then a poll's by where the picture stands; none where it is to be broken off.
    pub fn next_wait(&mut self, position: Option<(u32, u32)>) -> Option<u32> {
        let Some(position) = position else {
            return Some(FIRST_WAIT_MS);
        };
        let elapsed = self.last.map_or(FIRST_WAIT_MS, |(_, elapsed)| elapsed + POLL_MS);
        let previous = self.last.replace((position, elapsed));
        previous.is_none_or(|(last, _)| last != position && elapsed < TIMEOUT_MS).then_some(POLL_MS)
    }
}

fn wait<H: Hardware>(hw: &mut H, offset: usize, reads: u32, done: impl Fn(u32) -> bool) -> bool {
    (0..reads).any(|_| done(hw.read(Block::Vdec, offset)))
}

fn await_rest<H: Hardware>(hw: &mut H) {
    if hw.read(Block::Vdec, FEED_BUSY) & FETCH_BUSY != 0 {
        wait(hw, FEED_BUSY, ANSWER_READS, |state| state & FEED_REST != 0);
    }
}

fn reset<H: Hardware>(hw: &mut H) {
    hw.read(Block::Codec, 0);
    await_rest(hw);
    hw.write(Block::Vdec, FEED_RESET, HOLD_IN_RESET);
    hw.write(Block::Vdec, MISC_84, 2);
    let clocks = hw.read(Block::Vdec, CLOCKS);
    hw.write(Block::Vdec, CLOCKS, clocks & CLOCKS_MASK | CLOCKS_ON);
    hw.read(Block::Vdec, CLOCKS);
    for (offset, value) in RESET_WRITES {
        hw.write(Block::Vdec, offset, value);
    }
}

fn start_feed<H: Hardware>(hw: &mut H, work: &Work, feed_bytes: usize) -> Result<(), Stall> {
    let end = work.stream + feed_bytes as u32;
    hw.write(Block::Vdec, VLD + 0xC0, 0x4_0000);
    hw.write(Block::Vdec, READER_MODE, HEAD_MODE);
    hw.update(Block::Vdec, VLD_TOP + 0x54, 0, 1);
    hw.write(Block::Vdec, MC + 0x24, 6);
    hw.write(Block::Vdec, FEED_START, work.stream >> FEED_UNIT_SHIFT);
    hw.write(Block::Vdec, FEED_END, end >> FEED_UNIT_SHIFT);
    hw.update(Block::Vdec, VLD + 0xEC, 0, 0x1000_0000);
    hw.write(Block::Vdec, READER_BUFFER, work.reader);
    hw.write(Block::Vdec, READER_HOLD, 1);
    hw.write(Block::Vdec, READER_HOLD, 0);
    await_rest(hw);
    hw.update(Block::Vdec, FEED_WRITE, 0, 1);
    hw.write(Block::Vdec, FEED_READ, work.stream);
    hw.write(Block::Vdec, FEED_READ, work.stream);
    hw.write(Block::Vdec, FEED_WRITE, end);
    hw.write(Block::Vdec, READER_HOLD, 1);
    hw.write(Block::Vdec, FEED_RESET, RESTART);
    hw.write(Block::Vdec, FEED_RESET, 0);
    hw.write(Block::Vdec, VLD + 0xCC, 0);
    hw.write(Block::Vdec, FEED_TRIGGER, FETCH);
    if !wait(hw, FEED_FETCH, FETCH_READS, |state| state & 1 != 0) {
        return Err(Stall::Fetch);
    }
    hw.write(Block::Vdec, FEED_TRIGGER, FILL_WINDOW);
    hw.write(Block::Vdec, READER_HOLD, 0);
    hw.read(Block::Vdec, WINDOW);
    Ok(())
}

fn find_start<H: Hardware>(hw: &mut H) -> Result<(), Stall> {
    let (window, mode) = (hw.read(Block::Vdec, WINDOW), hw.read(Block::Vdec, READER_MODE));
    if window >> 8 != 1 || mode >> 31 != 0 {
        hw.write(Block::Vdec, FIND_START_CODE, 1);
        let is_found = wait(hw, FIND_START_CODE, SEARCH_READS, |state| state & 1 == 0);
        if hw.read(Block::Vdec, SEARCH_STATUS) & SEARCH_FOUND == 0 && !is_found {
            return Err(Stall::StartCode);
        }
    }
    shift(hw, SHIFT_LIMIT);
    Ok(())
}

fn shift<H: Hardware>(hw: &mut H, bits: usize) -> u32 {
    let window = hw.read(Block::Vdec, WINDOW);
    let mut remaining = bits;
    while remaining > 0 {
        let step = remaining.min(SHIFT_LIMIT);
        if remaining < bits {
            hw.read(Block::Vdec, WINDOW);
        }
        hw.read(Block::Vdec, WINDOW + 4 * step);
        remaining -= step;
    }
    window
}

fn agrees(unit: &[u8], offset: usize, window: u32) -> bool {
    let mut bits = Bits::new(unit.get(1..).unwrap_or_default());
    if bits.skip(offset).is_err() {
        return false;
    }
    let (expected, count) = bits.peek();
    (window ^ expected).checked_shr(u32::BITS - count).unwrap_or(0) == 0
}

fn to_number(entry: &Entry) -> u32 {
    entry.number as u32 & NUMBER_MASK
}

fn to_field(entry: &Entry, is_bottom: bool, has_count: bool) -> u32 {
    let count = if is_bottom { entry.bottom } else { entry.top };
    let is_top_first = entry.top <= entry.bottom;
    (entry.slot as u32) << FIELD_SLOT_SHIFT
        | if is_bottom { FIELD_BOTTOM } else { 0 }
        | if is_top_first { FIELD_TOP_FIRST } else { 0 }
        | if has_count { count as u32 & ORDER_MASK } else { 0 }
}

fn write_lists<H: Hardware>(hw: &mut H, work: &Work, job: &Job) -> Result<(), Stall> {
    let target = &job.target;
    let address = |buffer: Option<usize>| {
        buffer
            .and_then(|buffer| work.pictures.get(buffer))
            .or(work.pictures.get(target.spare))
            .copied()
            .ok_or(Stall::Unsupported)
    };
    let motion = |slot: usize| work.motion.get(slot).copied().ok_or(Stall::Unsupported);
    let (spare, own_motion) = (address(None)?, motion(target.slot)? >> 4);
    let second_field = if job.sequence.direct_8x8 { SECOND_FIELD_REDUCED } else { SECOND_FIELD_FULL };

    hw.write(Block::Vdec, LIST_MARKS[0], 0);
    hw.write(Block::Vdec, CLEAR_LISTS, 1);
    if !wait(hw, CLEAR_LISTS, ANSWER_READS, |state| state & 1 == 0) {
        return Err(Stall::Lists);
    }
    hw.write(Block::Vdec, NUMBERS, target.numbers);
    hw.write(Block::Vdec, NUMBER, target.number);
    for place in 0..LIST_SIZE {
        hw.write(Block::Vdec, LIST_ADDRESSES[0] + 4 * place, 0);
        hw.write(Block::Vdec, LIST_FIELDS[0] + 4 * place, 0);
    }
    for place in 0..LIST_SIZE {
        hw.write(Block::Vdec, LIST_ADDRESSES[0] + 4 * place, spare);
    }
    for place in 0..LIST_SIZE {
        hw.write(Block::Vdec, LIST_MOTION + 4 * place, own_motion);
    }
    for (place, entry) in target.forward.iter().enumerate().take(ENTRY_LIMIT) {
        hw.write(Block::Vdec, LIST_ADDRESSES[0] + 4 * place, address(entry.buffer)?);
        hw.write(Block::Vdec, LIST_NUMBERS[0] + 4 * place, to_number(entry));
        hw.write(Block::Vdec, LIST_FIELDS[0] + 8 * place, to_field(entry, false, false));
        hw.write(Block::Vdec, LIST_FIELDS[0] + 8 * place + 4, to_field(entry, true, false));
    }
    hw.write(Block::Vdec, TOP_ORDER, target.top as u32 & ORDER_MASK);
    hw.write(Block::Vdec, BOTTOM_ORDER, target.bottom as u32 & ORDER_MASK);
    hw.write(Block::Vdec, PICTURE_ORDER, target.top.min(target.bottom) as u32 & ORDER_MASK);

    hw.write(Block::Vdec, LIST_MARKS[1], 0);
    hw.write(Block::Vdec, LIST_MARKS[2], 0);
    for place in 0..LIST_SIZE {
        hw.write(Block::Vdec, LIST_ADDRESSES[1] + 4 * place, 0);
        hw.write(Block::Vdec, LIST_ADDRESSES[2] + 4 * place, 0);
        hw.write(Block::Vdec, LIST_FIELDS[1] + 4 * place, 0);
        hw.write(Block::Vdec, LIST_FIELDS[2] + 4 * place, 0);
    }
    for place in 0..2 * LIST_SIZE {
        hw.write(Block::Vdec, LIST_ADDRESSES[1] + 4 * place, spare);
    }
    // B slices are refused, but Rabbit's driver writes their two lists for every picture: for our recordings, of one reference, each is the list above.
    for (place, entry) in target.forward.iter().enumerate().take(ENTRY_LIMIT) {
        if job.sequence.direct_8x8 {
            hw.update(Block::Vdec, MOTION_MODE, 0, REDUCED_MOTION);
        }
        for list in [1, 2] {
            hw.write(Block::Vdec, LIST_ADDRESSES[list] + 4 * place, address(entry.buffer)?);
            hw.write(Block::Vdec, LIST_NUMBERS[list] + 4 * place, to_number(entry));
            hw.write(Block::Vdec, LIST_FIELDS[list] + 8 * place, to_field(entry, false, true));
            hw.write(Block::Vdec, LIST_FIELDS[list] + 8 * place + 4, to_field(entry, true, true));
        }
        let reference_motion = motion(entry.slot)? >> 4;
        hw.write(Block::Vdec, LIST_MOTION + 8 * place, reference_motion);
        hw.write(Block::Vdec, LIST_MOTION + 8 * place + 4, reference_motion + second_field);
    }
    Ok(())
}

fn write_parameters<H: Hardware>(hw: &mut H, layout: &Layout, job: &Job) {
    let (sequence, picture, slice) = (job.sequence, job.picture, &job.slice);
    let order_bits = if let Order::Lsb { bits } = sequence.order { bits - SMALLEST_COUNT_BITS } else { 0 };
    let word = 1
        | (sequence.number_bits - SMALLEST_COUNT_BITS) << 2
        | sequence.order.kind() << 6
        | order_bits << 8
        | sequence.references << 13
        | 1 << 18
        | u32::from(sequence.direct_8x8) << 20
        | 1 << 21;
    hw.write(Block::Vdec, SEQUENCE_WORD, word);
    hw.write(Block::Vdec, LAST_MACROBLOCK, (layout.rows - 1) << 16 | (layout.columns - 1));
    hw.write(Block::Vdec, BIT_DEPTHS, 0);
    let word = u32::from(picture.is_cabac)
        | u32::from(picture.has_bottom_order) << 1
        | u32::from(picture.is_weighted) << 2
        | picture.bipred_idc << 3
        | (picture.quantiser as u32 & 0x3F) << 5
        | (picture.chroma_offset as u32 & 0x1F) << 11
        | u32::from(picture.has_deblocking_control) << 16
        | u32::from(picture.constrained_intra) << 17
        | u32::from(picture.transform_8x8) << 18
        | (picture.second_chroma_offset as u32 & 0x1F) << 19;
    hw.write(Block::Vdec, PICTURE_WORD, word);
    hw.write(Block::Vdec, LISTS_WORD, picture.last_reference[0] | picture.last_reference[1] << 5);
    let previous = hw.read(Block::Vdec, FIRST_MACROBLOCK);
    hw.write(Block::Vdec, FIRST_MACROBLOCK, (previous & !0x1_FFFF) + (slice.first_mb & 0x1FFF));
    let previous = hw.read(Block::Vdec, SLICE_WORD);
    let word = (slice.kind & 0xF) << 13 | (slice.last_reference & 0x1F) << 20;
    hw.write(Block::Vdec, SLICE_WORD, (previous & 0x1FFF) + word);
}

fn launch<H: Hardware>(hw: &mut H, layout: &Layout, work: &Work, job: &Job) -> Result<u32, Stall> {
    let (sequence, picture, slice, target) = (job.sequence, job.picture, &job.slice, &job.target);
    let (luma, motion) = (
        *work.pictures.get(target.buffer).ok_or(Stall::Unsupported)?,
        *work.motion.get(target.slot).ok_or(Stall::Unsupported)?,
    );
    let (chroma_at, picture_bytes) = (layout.luma_bytes() as u32, layout.picture_bytes() as u32);
    if sequence.direct_8x8 {
        hw.update(Block::Vdec, MOTION_MODE, 0, REDUCED_MOTION);
    }
    hw.write(Block::Vdec, MISC_04, 3);
    hw.write(Block::Vdec, MC + 0x90, 4);
    hw.write(Block::Vdec, VLD_TOP + 0x58, u32::MAX);
    hw.write(Block::Vdec, PICTURE_WIDTH, layout.stride);
    hw.write(Block::Vdec, PICTURE_HEIGHT, layout.rows * MACROBLOCK);
    hw.write(Block::Vdec, MISC_04, 0x10);
    hw.write(Block::Vdec, PICTURE_SIZE, (layout.rows * MACROBLOCK) << 16 | layout.stride);
    hw.write(Block::Vdec, ERROR_MASK, ERROR_BITS);
    hw.write(Block::Vdec, AVC_VLD + 0x26C, ERROR_BITS);
    hw.write(Block::Vdec, MC + 0x220, 1);
    hw.write(Block::Vdec, MC + 0x224, luma >> 9);
    hw.write(Block::Vdec, MC + 0x228, (luma + chroma_at) >> 8);
    hw.write(Block::Vdec, MC + 0x250, 1);
    hw.write(Block::Vdec, MC + 0x238, 3);
    hw.write(Block::Vdec, MC + 0x260, layout.columns - 1);
    hw.write(Block::Vdec, MC + 0x264, layout.rows - 1);
    hw.write(Block::Vdec, MC + 0x22C, layout.columns);
    hw.write(Block::Vdec, MC + 0x850, (luma + picture_bytes) >> 4);
    hw.write(Block::Vdec, MC + 0x854, luma >> 4);
    hw.write(Block::Vdec, MC + 0x858, 1);
    hw.write(Block::Vdec, MC_5E4, 0x2_0000);
    hw.write(Block::Vdec, LUMA, luma);
    hw.write(Block::Vdec, CHROMA_OFFSET, chroma_at);
    hw.write(Block::Vdec, MC + 0x980, layout.columns);
    hw.write(Block::Vdec, SEARCH_STATUS, 0xF900_A03B);
    hw.write(Block::Vdec, PICTURE_MOTION, motion >> 4);
    let word = slice.quantiser_step as u32 & 0x7F
        | (slice.deblocking & 3) << 7
        | (slice.alpha as u32 & 0xF) << 9
        | (slice.beta as u32 & 0xF) << 13
        | u32::from(job.header.importance) << 17
        | u32::from(job.header.is_key_frame()) << 19
        | (slice.arithmetic_table & 3) << 20;
    hw.write(Block::Vdec, FILTER_WORD, word);
    if picture.is_cabac {
        hw.write(Block::Vdec, START_ARITHMETIC, 1);
    }
    if !wait(hw, ARITHMETIC_STATE, ANSWER_READS, |state| state & ARITHMETIC_BUSY == 0) {
        return Err(Stall::Arithmetic);
    }
    hw.update(Block::Vdec, MC_5E4, 0, 0x1000);
    hw.update(Block::Vdec, MC + 0x920, 0, 0x100_0000);
    hw.update(Block::Vdec, MC + 0x768, 7, 4);
    hw.update(Block::Vdec, MISC_F0, 0, 1);
    hw.write(Block::Vdec, MACROBLOCKS, layout.columns * layout.rows);
    hw.write(Block::Vdec, VLD_TOP + 0x50, u32::MAX);
    hw.write(Block::Vdec, LINES[0], work.lines[0]);
    hw.write(Block::Vdec, LINES[1], work.lines[1]);
    hw.write(Block::Vdec, AVC_VLD + 0x2D0, 0x1201);
    hw.write(Block::Vdec, AVC_VLD + 0x2D4, 0x0020_4008);
    let window = hw.read(Block::Vdec, WINDOW);
    hw.update(Block::Vdec, VLD_TOP + 0x9C, 0x1FFF, 0x400);
    hw.update(Block::Vdec, MISC + 0x28, 0, 1);
    hw.write(Block::Vdec, PICTURE_START, luma);
    hw.write(Block::Vdec, PICTURE_MIDDLE, luma + (picture_bytes / 2).next_multiple_of(MIDDLE_UNIT));
    let (first, last) = work.motion.iter().fold((u32::MAX, 0), |(first, last), &at| (first.min(at), last.max(at)));
    hw.write(Block::Vdec, MOTION_START, first);
    hw.write(Block::Vdec, MOTION_END, last + layout.motion_bytes() as u32);
    hw.write(Block::Vdec, READER_MODE, DECODE_MODE);
    hw.write(Block::Vdec, DECODE, 1);
    Ok(window)
}

fn start<H: Hardware>(hw: &mut H, work: &Work, job: &Job) -> Result<bool, Stall> {
    let (layout, slice) = (job.layout, &job.slice);
    reset(hw);
    start_feed(hw, work, job.feed_bytes())?;
    find_start(hw)?;
    let mut is_in_step = agrees(job.unit, 0, shift(hw, slice.changes_at));
    write_lists(hw, work, job)?;
    write_parameters(hw, &layout, job);
    if slice.is_inter() {
        hw.write(Block::Vdec, READ_CHANGES, 1);
        if !wait(hw, READ_CHANGES, ANSWER_READS, |state| state != 0) {
            return Err(Stall::Changes);
        }
    }
    if slice.has_weights {
        let previous = hw.read(Block::Vdec, MISC_F0);
        hw.write(Block::Vdec, MISC_F0, previous | 1);
        hw.write(Block::Vdec, READ_WEIGHTS, 1);
        if !wait(hw, READ_WEIGHTS, ANSWER_READS, |state| state != 0) {
            return Err(Stall::Weights);
        }
        // Rabbit's driver puts the word back with its bit cleared at the bit reader's offset of that number, not where it read it.
        hw.write(Block::Vdec, AVC_VLD + 0xF0, previous & !1);
    }
    is_in_step &= agrees(job.unit, slice.rest_at, shift(hw, slice.end - slice.rest_at));
    is_in_step &= agrees(job.unit, slice.end, launch(hw, &layout, work, job)?);
    Ok(is_in_step)
}

fn finish<H: Hardware>(hw: &mut H, layout: &Layout) -> Ending {
    hw.read(Block::Vdec, INTERRUPT);
    for (offset, value) in END_WRITES {
        hw.write(Block::Vdec, offset, value);
    }
    let (column, row) = last_macroblock(hw);
    let errors = hw.read(Block::Vdec, ERROR_MASK) & hw.read(Block::Vdec, ERRORS);
    Ending { is_complete: column + 1 >= layout.columns && row + 1 >= layout.rows, errors, column, row }
}

/// The column and row of the last finished macroblock; a damaged picture stalls there with neither interrupt nor error.
pub fn last_macroblock<H: Hardware>(hw: &mut H) -> (u32, u32) {
    (hw.read(Block::Vdec, COLUMN_DONE), hw.read(Block::Vdec, ROW_DONE))
}

fn halt<H: Hardware>(hw: &mut H) -> bool {
    hw.update(Block::Vdec, STOP, 0, 1);
    wait(hw, STOP_STATUS, ANSWER_READS, |state| state & STOP_BITS == STOP_BITS);
    hw.write(Block::Vdec, FEED_RESET, HOLD_IN_RESET);
    hw.write(Block::Vdec, FEED_RESET, 0);
    is_idle(hw)
}

fn is_idle<H: Hardware>(hw: &mut H) -> bool {
    let state = hw.read(Block::Vdec, FEED_BUSY);
    state & FETCH_BUSY == 0 || state & FEED_REST != 0
}

/// True where the feed rests after a picture, broken off first where `is_broken`; only then may the lock go.
pub fn is_at_rest<H: Hardware>(hw: &mut H, is_broken: bool) -> bool {
    if is_broken { halt(hw) } else { is_idle(hw) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::h264::read;
    use crate::codec::h264::read::samples::*;
    use crate::hw::last_write;
    use alloc::collections::BTreeMap;
    use alloc::vec;

    const PICTURES: [u32; 4] = [0x0250_0000, 0x0270_0000, 0x0290_0000, 0x02B0_0000];
    const MOTION: [u32; 17] = {
        let mut motion = [0; 17];
        let mut slot = 0;
        while slot < 17 {
            motion[slot] = 0x0080_0000 + 0x0010_0000 * slot as u32;
            slot += 1;
        }
        motion
    };
    const WORK: Work = Work {
        stream: 0x01B0_0000,
        reader: 0x0070_0000,
        lines: [0x01A0_0000, 0x0190_0000],
        motion: &MOTION,
        pictures: &PICTURES,
    };
    const REGISTERS: [(usize, u32); 9] = [
        (FEED_WRITE, 0xFFFF_FFFF),
        (FEED_FETCH, 0x8000_0411),
        (FEED_BUSY, 0x3_0000),
        (MOTION_MODE, 0x400),
        (MC + 0x920, 0x0100_0111),
        (MC + 0x768, 0x0100_0004),
        (MISC + 0xF0, 0x1_7141),
        (VLD_TOP + 0x9C, 0x0480_0300),
        (ARITHMETIC_STATE, 0x201),
    ];
    const MODE_MASK: u32 = 0x5FFF_DFFF;

    struct Decoder<'a> {
        memory: BTreeMap<usize, u32>,
        writes: Vec<(usize, u32)>,
        unit: &'a [u8],
        jumps: [(usize, usize); 2],
        position: Option<usize>,
        is_found: bool,
        dead_command: Option<usize>,
    }

    impl<'a> Decoder<'a> {
        fn new(unit: &'a [u8], slice: &Slice) -> Decoder<'a> {
            let changes_end = if slice.has_weights { slice.changes_at } else { slice.rest_at };
            Decoder {
                memory: REGISTERS.into_iter().collect(),
                writes: Vec::new(),
                unit,
                jumps: [(READ_CHANGES, changes_end), (READ_WEIGHTS, slice.rest_at)],
                position: None,
                is_found: false,
                dead_command: None,
            }
        }
    }

    impl Hardware for Decoder<'_> {
        fn read(&mut self, block: Block, offset: usize) -> u32 {
            if block != Block::Vdec {
                return 0x1010;
            }
            if !(WINDOW..=WINDOW + 4 * SHIFT_LIMIT).contains(&offset) {
                return self.memory.get(&offset).copied().unwrap_or(0);
            }
            let count = (offset - WINDOW) / 4;
            if count > 0 {
                self.position = Some(self.position.map_or(0, |position| position + count));
            }
            match self.position {
                None if self.is_found => 0x100 | u32::from(self.unit[0]),
                None => 1,
                Some(position) => {
                    let mut bits = Bits::new(&self.unit[1..]);
                    bits.skip(position).unwrap();
                    bits.peek().0
                }
            }
        }

        fn write(&mut self, block: Block, offset: usize, value: u32) {
            assert_eq!(block, Block::Vdec);
            self.writes.push((offset, value));
            let is_answered = self.dead_command != Some(offset);
            match offset {
                FIND_START_CODE | CLEAR_LISTS => drop(self.memory.insert(offset, u32::from(!is_answered))),
                READ_CHANGES | READ_WEIGHTS => drop(self.memory.insert(offset, u32::from(is_answered))),
                READER_MODE => drop(self.memory.insert(offset, value & MODE_MASK)),
                _ => drop(self.memory.insert(offset, value)),
            }
            self.is_found |= offset == FIND_START_CODE && is_answered;
            if let Some((_, position)) = self.jumps.iter().find(|(command, _)| *command == offset) {
                self.position = Some(*position);
            }
        }

        fn pause(&mut self, _micros: u32) {}
    }

    fn run(count: usize, places: impl Fn(usize) -> (usize, u32)) -> Vec<(usize, u32)> {
        (0..count).map(places).collect()
    }

    type Case = (&'static [u8], Slice, Target);

    fn recording() -> (Sets, [Case; 2]) {
        let sets = sets(&[&OURS.sequence_parameters(), &OURS.picture_parameters()]);
        let (mut dpb, mut outputs) = (Dpb::new(PICTURES.len()), Vec::new());
        let pictures = [&OUR_KEY[..], &OUR_NEXT[..]].map(|unit| {
            let slice = Slice::read(unit, &sets).unwrap();
            let target =
                dpb.begin(sets.get(0).unwrap().0, Header::parse(unit).unwrap(), &slice, 0, &mut outputs).unwrap();
            (unit, slice, target)
        });
        (sets, pictures)
    }

    fn job<'a>(
        sequence: &'a Sequence,
        picture: &'a Picture,
        unit: &'a [u8],
        slice: &Slice,
        target: &Target,
        bytes: usize,
    ) -> Job<'a> {
        let (layout, header) = (Layout::from_sequence(sequence).unwrap(), Header::parse(unit).unwrap());
        let (slice, target) = (slice.clone(), target.clone());
        Job { sequence, picture, layout, unit, header, slice, bytes, target, outputs: Vec::new() }
    }

    fn writes(
        sequence: &Sequence,
        picture: &Picture,
        unit: &[u8],
        slice: &Slice,
        target: &Target,
        bytes: usize,
    ) -> Vec<(usize, u32)> {
        let mut hw = Decoder::new(unit, slice);
        assert_eq!(start(&mut hw, &WORK, &job(sequence, picture, unit, slice, target, bytes)), Ok(true));
        hw.writes
    }

    #[test]
    fn picture_writes() {
        let (sets, [_, (unit, slice, target)]) = recording();
        let (sequence, picture) = sets.get(0).unwrap();
        #[rustfmt::skip]
        let reset = vec![
            (VLD + 0x108, 0x101), (MISC + 0x84, 2), (MISC, 0x0080_1FF0), (MISC + 0xC8, 0x1F7), (MISC + 0xCC, 0x53E3_0182), (MISC + 0x178, 0xFFFF_FFE1),
            (MISC + 0xF4, 0), (MISC + 0x84, 2), (VLD + 0x108, 0), (MISC + 0xEC, 1), (MISC + 0x4, 0x10), (MISC + 0x160, 0x0065_FF10),
        ];
        #[rustfmt::skip]
        let stream = vec![
            (VLD + 0xC0, 0x4_0000), (AVC_VLD + 0x84, 0xAE00_E007), (VLD_TOP + 0x54, 1), (MC + 0x24, 6), (VLD + 0xB4, 0x6_C000), (VLD + 0xB8, 0x6_C0DB),
            (VLD + 0xEC, 0x1000_0000), (VLD_TOP + 0x28, 0x0070_0000), (AVC_VLD + 0x28C, 1), (AVC_VLD + 0x28C, 0), (VLD + 0x110, 0xFFFF_FFFF),
            (VLD + 0xB0, 0x01B0_0000), (VLD + 0xB0, 0x01B0_0000), (VLD + 0x110, 0x01B0_36E0), (AVC_VLD + 0x28C, 1), (VLD + 0x108, 0x100), (VLD + 0x108, 0),
            (VLD + 0xCC, 0), (VLD + 0x8C, 0x10_0000), (VLD + 0x8C, 0x80_0000), (AVC_VLD + 0x28C, 0), (AVC_VLD + 0x2D8, 1),
        ];
        #[rustfmt::skip]
        let one_way = vec![(MC + 0x3DC, 0x0250_0000), (AVC_VLD + 0xA8, 0), (AVC_MV, 0x20_0000), (AVC_MV + 0x4, 0x60_0000), (AVC_MV + 0x204, 2), (AVC_MV + 0x208, 2), (AVC_MV + 0x200, 2), (MC + 0x568, 0), (MC + 0x56C, 0)];
        #[rustfmt::skip]
        let both_ways = vec![
            (AVC_MV + 0x220, 0x420), (MC + 0x45C, 0x0250_0000), (AVC_VLD + 0x128, 0), (AVC_MV + 0x80, 0x20_0000), (AVC_MV + 0x84, 0x60_0000),
            (MC + 0x4DC, 0x0250_0000), (AVC_VLD + 0x1A8, 0), (AVC_MV + 0x100, 0x20_0000), (AVC_MV + 0x104, 0x60_0000), (AVC_MV + 0x180, 0x8_0000), (AVC_MV + 0x184, 0x8_0002),
        ];
        #[rustfmt::skip]
        let sets_and_launch = vec![
            (AVC_VLD + 0x88, 0x0034_2C31), (VLD_TOP + 0x68, 0x002C_004F), (VLD_TOP + 0x100, 0), (AVC_VLD + 0x90, 0x5_0001), (AVC_VLD + 0x94, 0x21), (AVC_VLD + 0x8C, 0), (AVC_VLD + 0x98, 0xA000),
            (AVC_VLD + 0x22C, 1),
            (AVC_MV + 0x220, 0x420), (MISC + 0x4, 3), (MC + 0x90, 4), (VLD_TOP + 0x58, 0xFFFF_FFFF), (MC + 0x208, 0x500), (MC + 0x20C, 0x2D0), (MISC + 0x4, 0x10), (VLD_TOP + 0x70, 0x02D0_0500),
            (AVC_VLD + 0x280, 0xBFDF_B0F7), (AVC_VLD + 0x26C, 0xBFDF_B0F7), (MC + 0x220, 1), (MC + 0x224, 0x1_3800), (MC + 0x228, 0x2_7E60), (MC + 0x250, 1), (MC + 0x238, 3),
            (MC + 0x260, 0x4F), (MC + 0x264, 0x2C), (MC + 0x22C, 0x50), (MC + 0x850, 0x28_5900), (MC + 0x854, 0x27_0000), (MC + 0x858, 1), (MC + 0x5E4, 0x2_0000),
            (MC + 0x560, 0x0270_0000), (MC + 0x55C, 0xE_6000), (MC + 0x980, 0x50), (AVC_VLD + 0x2C8, 0xF900_A03B), (AVC_MV + 0x20C, 0x9_0000), (AVC_VLD + 0x9C, 0x4_0004),
            (AVC_VLD + 0x234, 1), (MC + 0x5E4, 0x2_1000), (MC + 0x920, 0x0100_0111), (MC + 0x768, 0x0100_0004), (MISC + 0xF0, 0x1_7141), (VLD_TOP + 0x6C, 0xE10),
            (VLD_TOP + 0x50, 0xFFFF_FFFF), (MC + 0xA20, 0x01A0_0000), (MC + 0xA24, 0x0190_0000), (AVC_VLD + 0x2D0, 0x1201), (AVC_VLD + 0x2D4, 0x0020_4008), (VLD_TOP + 0x9C, 0x0480_0400),
            (MISC + 0x28, 1), (MISC + 0x3C, 0x0270_0000), (MISC + 0x40, 0x027A_C800), (MISC + 0x54, 0x0080_0000), (MISC + 0x58, 0x0183_9A00), (AVC_VLD + 0x84, 0xAE80_E007), (AVC_VLD + 0x238, 1),
        ];
        let expected: Vec<(usize, u32)> = [
            reset,
            stream,
            vec![(MC + 0x564, 0), (AVC_VLD + 0x228, 1), (AVC_VLD + 0xA0, 0x1_0000), (AVC_VLD + 0xA4, 1)],
            (0..32).flat_map(|place| [(MC + 0x3DC + 4 * place, 0), (AVC_MV + 4 * place, 0)]).collect(),
            run(32, |place| (MC + 0x3DC + 4 * place, 0x0250_0000)),
            run(32, |place| (AVC_MV + 0x180 + 4 * place, 0x9_0000)),
            one_way,
            (0..32)
                .flat_map(|place| {
                    [
                        (MC + 0x45C + 4 * place, 0),
                        (MC + 0x4DC + 4 * place, 0),
                        (AVC_MV + 0x80 + 4 * place, 0),
                        (AVC_MV + 0x100 + 4 * place, 0),
                    ]
                })
                .collect(),
            run(64, |place| (MC + 0x45C + 4 * place, 0x0250_0000)),
            both_ways,
            sets_and_launch,
        ]
        .concat();
        let actual = writes(sequence, picture, unit, &slice, &target, 13792);
        assert_eq!(actual.len(), 433);
        for (index, (write, expected)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(
                write, expected,
                "write {index}: {:#x} = {:#x} where the driver writes {:#x} = {:#x}",
                write.0, write.1, expected.0, expected.1
            );
        }
        assert_eq!(actual.len(), expected.len());
    }

    #[test]
    fn key_picture_writes() {
        let (sets, [(unit, slice, target), _]) = recording();
        let (sequence, picture) = sets.get(0).unwrap();
        let actual = writes(sequence, picture, unit, &slice, &target, 38721);
        assert_eq!(actual.len(), 417, "sixteen writes less than a picture with one reference");
        assert!(actual.iter().all(|write| write.0 != READ_CHANGES && write.0 != READ_WEIGHTS));
        assert_eq!(
            (last_write(&actual, FILTER_WORD), last_write(&actual, SLICE_WORD), last_write(&actual, NUMBER)),
            (0xE_0002, 0x10_E000, 0),
            "the default count of references stays in the slice word"
        );
        assert_eq!((last_write(&actual, FEED_END), last_write(&actual, FEED_WRITE)), (0x6_C261, 0x01B0_9841));
        assert_eq!(
            (
                last_write(&actual, LIST_ADDRESSES[0]),
                last_write(&actual, LIST_ADDRESSES[1] + 4 * 63),
                last_write(&actual, LUMA)
            ),
            (PICTURES[0], PICTURES[0], PICTURES[0]),
            "places that name nothing point at the picture itself"
        );
        assert_eq!(
            (last_write(&actual, PICTURE_MOTION), last_write(&actual, LIST_MOTION + 4 * 31)),
            (MOTION[0] >> 4, MOTION[0] >> 4)
        );
    }

    fn entry(slot: usize, number: i32, order: i32) -> Entry {
        Entry { slot, buffer: Some(slot), number, top: order, bottom: order }
    }

    #[test]
    fn list_writes() {
        let (sets, [_, (unit, slice, target)]) = recording();
        let (sequence, picture) = sets.get(0).unwrap();
        let (near, far, below) = (entry(1, 3, 8), entry(2, 2, 4), Entry { bottom: -3, ..entry(0, 1, -5) });
        let three = Target { slot: 3, buffer: 3, forward: vec![near, far, below], spare: 2, ..target };
        let actual = writes(sequence, picture, unit, &slice, &three, 100);
        let first: Vec<u32> = actual.iter().filter(|write| write.0 == LIST_ADDRESSES[2]).map(|write| write.1).collect();
        assert_eq!(first, [0, PICTURES[2], PICTURES[1]], "cleared, pointed at the picture decoded before, written");
        assert_eq!(
            actual.iter().filter(|write| write.0 == LIST_ADDRESSES[1] && write.1 == PICTURES[2]).count(),
            1,
            "places that name nothing point at the picture decoded before"
        );
        let words =
            [LIST_NUMBERS[0] + 8, LIST_FIELDS[0] + 16, LIST_FIELDS[0] + 20, LIST_FIELDS[1] + 16, LIST_FIELDS[1] + 20]
                .map(|offset| last_write(&actual, offset));
        assert_eq!(
            words,
            [1, 0x20_0000, 0x60_0000, 0x0023_FFFB, 0x0063_FFFD],
            "a frame whose order counts lie below zero, cut to 18 bits"
        );
        assert_eq!(last_write(&actual, LIST_NUMBERS[0]), 3);
        let before_wrap = Target { forward: vec![Entry { number: -2, top: 9, bottom: 7, ..near }], ..three };
        let actual = writes(sequence, picture, unit, &slice, &before_wrap, 100);
        assert_eq!(
            (
                last_write(&actual, LIST_NUMBERS[0]),
                last_write(&actual, LIST_FIELDS[0]),
                last_write(&actual, LIST_FIELDS[0] + 4)
            ),
            (0x7_FFFE, 0x0080_0000, 0x00C0_0000),
            "a number from before the wrap, and a frame whose bottom field comes first"
        );
    }

    #[test]
    fn weights_and_plain_coding() {
        let sets = sets(&[&X264_SEQUENCE, &X264_PICTURE]);
        let (sequence, picture) = sets.get(0).unwrap();
        let (unit, mut dpb, mut outputs) = (&X264_SLICES[5], Dpb::new(4), Vec::new());
        let slice = Slice::read(unit, &sets).unwrap();
        let target = dpb.begin(sequence, Header::parse(unit).unwrap(), &slice, 0, &mut outputs).unwrap();
        let actual = writes(sequence, picture, unit, &slice, &target, 15796);
        let at = actual.iter().position(|write| write.0 == READ_CHANGES).unwrap();
        assert_eq!(
            actual[at..at + 4],
            [(READ_CHANGES, 1), (MISC + 0xF0, 0x1_7141), (READ_WEIGHTS, 1), (AVC_VLD + 0xF0, 0x1_7140)]
        );
        assert_eq!(
            (
                last_write(&actual, SEQUENCE_WORD),
                last_write(&actual, PICTURE_WORD),
                last_write(&actual, LISTS_WORD),
                last_write(&actual, SLICE_WORD),
                last_write(&actual, FILTER_WORD)
            ),
            (0x0034_8201, 0x00F5_F7B5, 3, 0x30_A000, 0x4_0002)
        );
        assert_eq!(
            (last_write(&actual, NUMBERS), last_write(&actual, NUMBER), last_write(&actual, PICTURE_ORDER)),
            (16, 3, 16)
        );
        let plain = Picture { is_cabac: false, ..picture.clone() };
        let actual = writes(sequence, &plain, unit, &slice, &target, 15796);
        assert!(actual.iter().all(|write| write.0 != START_ARITHMETIC));
        assert_eq!(last_write(&actual, PICTURE_WORD), 0x00F5_F7B4);
        let coarse = Sequence { direct_8x8: false, order: Order::Frame, ..sequence.clone() };
        let back = Target { forward: vec![entry(1, 0, 0)], ..target };
        let actual = writes(&coarse, picture, unit, &slice, &back, 15796);
        assert!(actual.iter().all(|write| write.0 != MOTION_MODE), "motion is kept for every block");
        assert_eq!(
            (last_write(&actual, LIST_MOTION + 4), last_write(&actual, SEQUENCE_WORD)),
            ((MOTION[1] >> 4) + 4, 0x0024_8081)
        );
    }

    #[test]
    fn stalls_and_step() {
        let (sets, [_, (unit, slice, target)]) = recording();
        let (sequence, picture) = sets.get(0).unwrap();
        let job = job(sequence, picture, unit, &slice, &target, 13792);
        for (command, stall) in
            [(FIND_START_CODE, Stall::StartCode), (CLEAR_LISTS, Stall::Lists), (READ_CHANGES, Stall::Changes)]
        {
            let mut hw = Decoder { dead_command: Some(command), ..Decoder::new(unit, &slice) };
            assert_eq!(start(&mut hw, &WORK, &job), Err(stall));
            assert_ne!(hw.writes.last(), Some(&(DECODE, 1)));
        }
        let mut marked = Decoder { dead_command: Some(FIND_START_CODE), ..Decoder::new(unit, &slice) };
        marked.memory.insert(SEARCH_STATUS, SEARCH_FOUND);
        assert_eq!(
            start(&mut marked, &WORK, &job).map(|_| ()),
            Ok(()),
            "a search that ran out with its mark set counts as found"
        );
        let mut starved = Decoder::new(unit, &slice);
        starved.memory.insert(FEED_FETCH, 0x8000_0410);
        assert_eq!(start(&mut starved, &WORK, &job), Err(Stall::Fetch));
        let mut busy = Decoder::new(unit, &slice);
        busy.memory.insert(ARITHMETIC_STATE, ARITHMETIC_BUSY);
        assert_eq!(start(&mut busy, &WORK, &job), Err(Stall::Arithmetic));

        let mut astray = Decoder::new(unit, &slice);
        astray.jumps[0].1 += 3;
        assert_eq!(
            start(&mut astray, &WORK, &job),
            Ok(false),
            "the decoder read three bits more of the head than the slice has there"
        );
        assert_eq!(astray.writes.last(), Some(&(DECODE, 1)), "the picture is started all the same");
        let mut aligned = Decoder { is_found: true, ..Decoder::new(unit, &slice) };
        assert_eq!(start(&mut aligned, &WORK, &job), Ok(true));
        assert!(
            aligned.writes.iter().all(|write| write.0 != FIND_START_CODE),
            "a reader that stands at a start code is not sent searching"
        );

        let far = Job { target: Target { slot: 17, ..target.clone() }, ..job };
        assert_eq!(start(&mut Decoder::new(unit, &slice), &WORK, &far), Err(Stall::Unsupported));
        let nowhere = Target { buffer: 4, spare: 4, forward: vec![], ..target.clone() };
        let nowhere = Job { target: nowhere, ..far };
        assert_eq!(start(&mut Decoder::new(unit, &slice), &WORK, &nowhere), Err(Stall::Unsupported));
    }

    fn attempt(sets: &Sets, before: [&[u8]; 2], unit: &[u8]) -> (u32, u32) {
        let (mut dpb, mut outputs) = (Dpb::new(PICTURES.len()), Vec::new());
        let mut counts = (0, 0);
        for unit in before.into_iter().chain([unit]) {
            let (Some(header), Ok(slice)) = (Header::parse(unit), Slice::read(unit, sets)) else {
                continue;
            };
            let Ok((sequence, picture)) = sets.get(slice.picture) else {
                continue;
            };
            counts.0 += 1;
            let Some(target) = dpb.begin(sequence, header, &slice, 0, &mut outputs) else {
                continue;
            };
            // A stream begins no job on pictures its buffers were not laid out for.
            if Layout::from_sequence(sequence).is_some() {
                let job = job(sequence, picture, unit, &slice, &target, unit.len());
                counts.1 += u32::from(start(&mut Decoder::new(unit, &slice), &WORK, &job).is_ok());
            }
            dpb.finish(&mut outputs);
            outputs.drain(..).for_each(|picture| dpb.give_back(picture.buffer));
        }
        counts
    }

    fn damaged(bytes: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
        (0..bytes.len() * 8).flat_map(move |bit| {
            let mut flipped = bytes.to_vec();
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
            let short = flipped[..=bit / 8].to_vec();
            [flipped, short]
        })
    }

    #[test]
    fn damaged_units() {
        let sets = sets(&[&X264_SEQUENCE, &X264_PICTURE]);
        let before = [&X264_SLICES[0][..], &X264_SLICES[1][..]];
        assert_eq!(attempt(&sets, before, &X264_SLICES[5]), (3, 3), "the stream as it is");
        assert_eq!(attempt(&sets, before, &X264_SLICES[2]), (2, 2), "a B slice is refused");
        let (mut heads, mut started, mut tried) = (0, 0, 0);
        for unit in [&X264_SLICES[1], &X264_SLICES[5]] {
            for unit in damaged(unit) {
                let (read, starts) = attempt(&sets, before, &unit);
                (heads, started, tried) = (heads + read - 2, started + starts - 2, tried + 1);
            }
        }
        assert!(
            heads > tried / 4 && heads < tried && started > 0,
            "{heads} of {tried} damaged heads were read, {started} started"
        );
        let mut count = 0;
        for set in [&X264_SEQUENCE[..], &X264_PICTURE[..]] {
            for unit in damaged(set) {
                let mut sets = sets.clone();
                if sets.add(&unit) == Ok(true) {
                    count += 1;
                    for (sequence, _) in (0..read::PICTURES).filter_map(|picture| sets.get(picture).ok()) {
                        let _ =
                            (Layout::from_sequence(sequence), dpb_size(sequence), sequence.size(), sequence.numbers());
                    }
                    attempt(&sets, before, &X264_SLICES[5]);
                }
            }
        }
        assert!(count > 50, "{count} damaged parameter sets were taken for good ones");
    }

    #[test]
    fn stream_buffer() {
        let units: [&[u8]; 5] = [&[0x06, 5, 0x80], &[0x67, 1], &[0x65, 0x88, 0x84], &[0x09, 0xF0], &[0x41, 0x9A]];
        let found: Vec<&[u8]> = slices(units.into_iter()).map(|(unit, _)| unit).collect();
        assert_eq!(found, [&[0x65, 0x88, 0x84][..], &[0x41, 0x9A]]);
        let mut buffer = [0xEE; 300];
        assert_eq!(write_slices(units.into_iter(), &mut buffer), Some(13));
        assert_eq!(
            buffer[..18],
            [0, 0, 0, 1, 0x65, 0x88, 0x84, 0, 0, 0, 1, 0x41, 0x9A, 0, 0, 0, 1, 9],
            "other units are left out"
        );
        assert!(
            buffer[18..13 + STREAM_SLACK].iter().all(|&byte| byte == 0)
                && buffer[13 + STREAM_SLACK..].iter().all(|&byte| byte == 0xEE)
        );
        assert_eq!(write_slices(units.into_iter(), &mut [0; 13 + STREAM_SLACK - 1]), None);
        assert_eq!(write_slices(units.into_iter(), &mut [0; 5]), None);
        assert_eq!(write_slices([].into_iter(), &mut buffer), Some(0));
    }

    #[test]
    fn layout_sizes() {
        let sequence = Sequence::read(&OURS.sequence_parameters()).unwrap();
        let layout = Layout::from_sequence(&sequence).unwrap();
        assert_eq!(layout, Layout { columns: 80, rows: 45, stride: 1280, lines: 736 });
        assert_eq!(
            (layout.luma_bytes(), layout.picture_bytes(), layout.motion_bytes()),
            (0xE_6000, 1_413_120, 0x3_9A00)
        );
        let sized = |columns: u32, rows: u32| Layout::from_sequence(&Sequence { columns, rows, ..sequence.clone() });
        let full_hd = sized(120, 68).unwrap();
        assert_eq!(
            (full_hd.stride, full_hd.lines, full_hd.picture_bytes(), full_hd.motion_bytes()),
            (1920, 1088, 3_133_440, 522_752)
        );
        let small = sized(40, 23).unwrap();
        assert_eq!((small.lines, small.motion_bytes()), (384, 61_952));
        assert!(sized(68, 120).is_some() && sized(4, 4).is_some() && sized(128, 63).is_some());
        assert_eq!([sized(3, 40), sized(40, 3), sized(129, 4), sized(120, 69)], [None; 4]);
    }

    #[test]
    fn buffer_counts() {
        let sequence = Sequence::read(&OURS.sequence_parameters()).unwrap();
        let mut stream = Stream::new(&sequence, 2).unwrap();
        assert_eq!(
            (stream.layout(), stream.buffers()),
            (Layout::from_sequence(&sequence).unwrap(), (3, 5)),
            "two frames and one in work, and two pictures out to be shown besides"
        );
        assert_eq!(stream.layout().stream_bytes(), 1_413_120 + STREAM_SLACK);
        let wider = Sequence { columns: sequence.columns + 1, ..sequence };
        assert!(stream.fits(&sequence) && !stream.fits(&wider), "buffers laid out otherwise do not");
        stream.capacity -= 1;
        assert!(!stream.fits(&sequence), "nor buffers made for fewer frames");
    }

    #[test]
    fn flawed_samples() {
        let complete = Ending { is_complete: true, errors: 0, column: 79, row: 44 };
        let clean = || Outcome { buffer: Some(1), ending: Some(complete), is_in_step: true, ..Outcome::default() };
        assert!(
            !clean().is_flawed() && !Outcome::default().is_flawed(),
            "a picture ended whole is none, nor a sample without one"
        );
        let flaws = [
            Outcome { ending: Some(Ending { is_complete: false, ..complete }), ..clean() },
            Outcome { ending: Some(Ending { errors: 3, ..complete }), ..clean() },
            Outcome { is_in_step: false, ..clean() },
            Outcome { stall: Some(Stall::Fetch), ..clean() },
            Outcome { ending: None, ..clean() },
        ];
        assert!(flaws.iter().all(Outcome::is_flawed), "cut short, with an error, out of step, stalled, never ended");
    }

    #[test]
    fn block_layout() {
        let ours = Sequence::read(&OURS.sequence_parameters()).unwrap();
        let layout = Layout::from_sequence(&Sequence { columns: 4, rows: 4, ..ours }).unwrap();
        assert_eq!((layout.stride, layout.lines, layout.luma_bytes()), (64, 64, 4096));
        let luma = [(0, 0), (17, 0), (15, 1), (0, 31), (63, 31), (0, 32), (20, 40)];
        assert_eq!(luma.map(|(column, line)| layout.at(false, column, line)), [0, 513, 31, 496, 2047, 2048, 2692]);
        let chroma = [(0, 0), (17, 0), (0, 15), (0, 16), (63, 31)];
        assert_eq!(chroma.map(|(column, line)| layout.at(true, column, line)), [4096, 4353, 4336, 5120, 6143]);
    }

    #[test]
    fn picture_end() {
        let layout = &Layout::from_sequence(&Sequence::read(&OURS.sequence_parameters()).unwrap()).unwrap();
        let done = [(COLUMN_DONE, 79), (ROW_DONE, 44), (ERROR_MASK, ERROR_BITS), (ERRORS, 0x4000)];
        let mut hw = Decoder { memory: done.into_iter().collect(), ..Decoder::new(&OUR_KEY, &Slice::default()) };
        assert_eq!(
            finish(&mut hw, layout),
            Ending { is_complete: true, errors: 0, column: 79, row: 44 },
            "an error the mask leaves out does not count"
        );
        assert_eq!(hw.writes, [(MISC + 0xC8, 0xF7), (MISC + 0xCC, 0x13A2_0100)]);
        hw.memory.extend([(ROW_DONE, 12), (ERRORS, 0x4003)]);
        assert_eq!(last_macroblock(&mut hw), (79, 12), "where a picture in work stands");
        assert_eq!(finish(&mut hw, layout), Ending { is_complete: false, errors: 3, column: 79, row: 12 });
    }

    #[test]
    fn retry_line() {
        assert_eq!((to_retry_line(2, 1), to_retry_line(1, 1)), (Some(STOPPED_LINE), None));
        assert_eq!(to_unfit_text(&sized(200, 45)), "pictures of 200x45 macroblocks");
    }

    #[test]
    fn break_off() {
        let slice = Slice::default();
        let mut hw = Decoder {
            memory: [(STOP_STATUS, 0x11), (FEED_BUSY, 0x8001)].into_iter().collect(),
            ..Decoder::new(&OUR_KEY, &slice)
        };
        assert!(halt(&mut hw) && is_idle(&mut hw));
        assert_eq!(hw.writes, [(STOP, 1), (FEED_RESET, HOLD_IN_RESET), (FEED_RESET, 0)]);
        hw.memory.insert(STOP_STATUS, 0x10);
        assert!(halt(&mut hw), "a decoder that never says it stopped is at rest all the same");
        hw.memory.extend([(STOP_STATUS, 0x11), (FEED_BUSY, 0x8000)]);
        assert!(!halt(&mut hw) && !is_idle(&mut hw), "a fetch is still under way");
        hw.memory.insert(FEED_BUSY, 0x3_0000);
        assert!(is_idle(&mut hw));
        hw.memory.insert(FEED_BUSY, 0x8000);
        hw.writes.clear();
        assert!(
            !is_at_rest(&mut hw, false) && hw.writes.is_empty(),
            "a picture that ended is looked at, and not broken off"
        );
        assert!(!is_at_rest(&mut hw, true) && hw.writes.len() == 3, "one that did not end is broken off first");
        hw.memory.insert(FEED_BUSY, 0x8001);
        assert!(is_at_rest(&mut hw, false) && is_at_rest(&mut hw, true), "a feed at rest has rested either way");
        let mut decoder = Decoder::new(&OUR_KEY, &slice);
        reset(&mut decoder);
        assert_eq!(decoder.writes.len(), 12);
        assert_eq!(decoder.writes[..3], [(FEED_RESET, HOLD_IN_RESET), (MISC + 0x84, 2), (CLOCKS, CLOCKS_ON)]);
    }

    #[test]
    fn sample_start() {
        let (sequence_set, picture_set) = (OURS.sequence_parameters(), OURS.picture_parameters());
        let sequence = Sequence::read(&sequence_set).unwrap();
        let mut stream = Stream::new(&sequence, 2).unwrap();
        let mut buffer = vec![0xEE; stream.layout().stream_bytes()];
        let is_unreadable = |outcome: Option<StartOutcome>| {
            matches!(outcome, Some(StartOutcome::Unsupported(Unsupported::Unreadable(_))))
        };
        assert!(
            is_unreadable(stream.begin(&[&OUR_KEY], 0, &mut buffer).err()),
            "a slice whose parameter sets never came"
        );
        assert!(is_unreadable(stream.begin(&[&sequence_set[..3]], 0, &mut buffer).err()), "a parameter set cut short");
        assert_eq!(
            stream.begin(&[&sequence_set, &picture_set], 0, &mut buffer).err(),
            Some(StartOutcome::Done),
            "a sample without a picture, its parameter sets taken"
        );
        assert!(is_unreadable(stream.begin(&[&OUR_KEY[..2]], 0, &mut buffer).err()), "a head that cannot be read");
        assert_eq!(
            stream.begin(&[&OUR_KEY], 0, &mut [0; 40]).err(),
            Some(StartOutcome::Unsupported(Unsupported::TooLarge))
        );
        let mut smaller = Stream::new(&sized(40, 23), 2).unwrap();
        smaller.add_set(&sequence_set).unwrap();
        smaller.add_set(&picture_set).unwrap();
        assert_eq!(
            smaller.begin(&[&OUR_KEY], 0, &mut buffer).err(),
            Some(StartOutcome::Unsupported(Unsupported::Mismatch)),
            "buffers made for smaller pictures"
        );
        let job = stream.begin(&[&OUR_KEY], 7, &mut buffer).unwrap_or_else(|outcome| panic!("{outcome:?}"));
        assert_eq!(job.feed_bytes(), 4 + OUR_KEY.len() + STREAM_SLACK);
        assert_eq!((&buffer[..4], &buffer[4..14]), (&START_CODE[..], &OUR_KEY[..]), "its slice behind a start code");
        let programmed = writes(job.sequence, job.picture, job.unit, &job.slice, &job.target, job.bytes);
        let mut hw = Decoder::new(&OUR_KEY, &job.slice);
        let target = job.target.buffer;
        let outcome = job.start(&mut hw, &WORK);
        assert_eq!(hw.writes, programmed, "the decoder is started on the job the sample makes");
        assert_eq!(
            (outcome.buffer, outcome.is_in_step, outcome.stall, outcome.outputs.len()),
            (Some(target), true, None, 0)
        );
        let done = [(COLUMN_DONE, 79), (ROW_DONE, 44), (ERROR_MASK, ERROR_BITS), (ERRORS, 0)];
        hw.memory.extend(done);
        assert_eq!(stream.ending(&mut hw), Ending { is_complete: true, errors: 0, column: 79, row: 44 });
        stream.finish(&mut Outcome::default());
        let job = stream.begin(&[&OUR_KEY], 8, &mut buffer).unwrap_or_else(|outcome| panic!("{outcome:?}"));
        let mut dead = Decoder { dead_command: Some(FIND_START_CODE), ..Decoder::new(&OUR_KEY, &job.slice) };
        let stalled = job.start(&mut dead, &WORK);
        assert_eq!(
            (stalled.stall, stalled.is_in_step),
            (Some(Stall::StartCode), false),
            "a start the decoder did not answer"
        );
        stream.finish(&mut Outcome::default());
        stream.reset();
        assert!(
            is_unreadable(stream.begin(&[&OUR_KEY], 0, &mut buffer).err()),
            "a stream begun anew has no parameter sets"
        );
    }

    #[test]
    fn full_buffers() {
        let mut stream = Stream::new(&Sequence::read(&OURS.sequence_parameters()).unwrap(), 0).unwrap();
        stream.add_set(&OURS.sequence_parameters()).unwrap();
        stream.add_set(&OURS.picture_parameters()).unwrap();
        let mut buffer = vec![0; stream.layout().stream_bytes()];
        let (mut out, mut count) = (Vec::new(), 0);
        let full = loop {
            match stream.begin(&[&OUR_KEY], count, &mut buffer) {
                Ok(job) => out.extend(job.outputs.iter().map(|output| output.buffer)),
                Err(outcome) => break outcome,
            }
            let mut outcome = Outcome::default();
            stream.finish(&mut outcome);
            out.extend(outcome.outputs.iter().map(|output| output.buffer));
            count += 1;
            assert!(count as usize <= stream.buffers().1, "no more pictures than buffers are begun");
        };
        assert_eq!(full, StartOutcome::Full);
        stream.give_back(out[0]);
        assert!(stream.begin(&[&OUR_KEY], count, &mut buffer).is_ok(), "a picture given back makes room");
        stream.finish(&mut Outcome::default());
        stream.rewind();
        let outputs = stream.drain();
        assert!(outputs.is_empty(), "a stream rewound holds no frame: {outputs:?}");
    }

    #[test]
    fn watch_polls() {
        let mut watch = Watch::default();
        assert_eq!(watch.next_wait(None), Some(FIRST_WAIT_MS), "before any look, the first wait");
        assert_eq!(watch.next_wait(Some((3, 0))), Some(POLL_MS), "the first look sees where it stands");
        assert_eq!(watch.next_wait(Some((9, 0))), Some(POLL_MS), "one that came on is waited for");
        assert_eq!(
            watch.next_wait(Some((9, 0))),
            None,
            "one that came no further from one look to the next is broken off"
        );
        let mut moving = Watch::default();
        let (mut elapsed, mut position) = (0, None);
        while let Some(ms) = moving.next_wait(position) {
            (elapsed, position) = (elapsed + ms, Some((position.map_or(0, |(column, _)| column + 1), 0)));
        }
        assert_eq!(
            (elapsed, TIMEOUT_MS),
            (200, 200),
            "one that still comes on is broken off at the end of the patience"
        );
    }
}
