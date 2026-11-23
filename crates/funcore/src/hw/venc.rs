//! The MT6765's H.264 encoder, programmed frame by frame as Rabbit's driver does with its rate control off.

use crate::codec::h264::MACROBLOCK;
use crate::hw::Block;
use crate::hw::Hardware;
use alloc::format;
use alloc::string::String;

const MODE: usize = 0x00;
const GROUP: usize = 0x04;
const TOOLS: usize = 0x08;
const WORD_0C: usize = 0x0C;
const FRAME_KIND: usize = 0x10;
const FRAME_NUMBER: usize = 0x14;
const FRAME_ORDER: usize = 0x18;
const KEY_NUMBER: usize = 0x1C;
const SIZE: usize = 0x24;
const PITCH: usize = 0x28;
const PROFILE: usize = 0x30;
const BIT_RATE: usize = 0x48;
const FRAME_RATE: usize = 0x4C;
const QUANTISER_RANGE: usize = 0x50;
const WORD_54: usize = 0x54;
const TRIGGER: usize = 0x58;
const STREAM: usize = 0x64;
const STREAM_UNITS: usize = 0x68;
const SOURCE_LUMA: usize = 0x6C;
const SOURCE_CHROMA: usize = 0x70;
const REBUILT_LUMA: usize = 0x74;
const REBUILT_CHROMA: usize = 0x78;
const REFERENCE_LUMA: usize = 0x7C;
const REFERENCE_CHROMA: usize = 0x80;
const MOTION_OUT: usize = 0x84;
const MOTION_IN: usize = 0x88;
const SOURCE_THIRD: usize = 0x94;
const BYTE_COUNT: usize = 0x98;
const CYCLES: usize = 0x9C;
const INTERRUPT_ENABLE: usize = 0xA4;
const RESET: usize = 0xA8;
const CLOCK: usize = 0xEC;
const WORD_124: usize = 0x124;
const WORD_128: usize = 0x128;
const BUS_PRIORITY: usize = 0x12C;

const ADDRESS_SHIFT: u32 = 4;
const ADDRESS_ALIGNMENT: u32 = 1 << ADDRESS_SHIFT;
const ROWS_SHIFT: u32 = 8;
const BLOCKS_SHIFT: u32 = 16;
const SIDE_LIMIT: u32 = u8::MAX as u32;
const PITCH_COLUMN_SHIFTS: [u32; 2] = [21, 11];
const PITCH_RIGHT_SHIFT: u32 = 5;
/// The stream buffer's size unit of 128 bytes, as a shift.
pub const STREAM_UNIT_SHIFT: u32 = 7;
/// Milliseconds before a frame counts as stuck.
pub const TIMEOUT_MS: u32 = 100;
const LUMA_BYTES: usize = 256;
const CHROMA_BYTES: usize = 128;
const LONGEST_GROUP: u32 = 1023;
const SHORT_GROUP: u32 = 30;
pub(super) const COARSEST: u32 = 51;
const INTRA_QUANTISER_SHIFT: u32 = 26;
const SHORT_GROUP_SHIFT: u32 = 14;
const INTER_QUANTISER_SHIFT: u32 = 10;
const TOOLS_FIXED: u32 = 0x626E_03AE;
const WORD_0C_FIXED: u32 = 0x924E_0000;
const PROFILE_FIXED: u32 = 0xC020_0000;
const LEVEL_SHIFT: u32 = 22;
const SIGNALLED_RATE: u32 = 30;
const FRAME_RATE_SHIFT: u32 = 16;
const QUANTISER_RANGE_FIXED: u32 = COARSEST << 8 | 8;
const WORD_54_FIXED: u32 = 0x800;
const WORD_128_FIXED: u32 = 0x0001_0000;
const BUS_PRIORITY_FIXED: u32 = 0x1F4;
const INTER_FRAME: u32 = 0x0010_8400;
const KEY_FRAME: u32 = 0x3006;
const KEY_COUNT_SHIFT: u32 = 3;
const LATER_INTER_FRAME: u32 = 0x100;
const ALL_INTERRUPTS: u32 = 0x3F;
const MODE_FRAME: u32 = 0x0200_BFE0;
const START_FRAME: u32 = 4;
const STATUS_FRAME: u32 = 0x4;
const STATUS_FULL: u32 = 0x8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shape {
    pub(super) width: u32,
    pub(super) height: u32,
}

impl Shape {
    const fn columns(self) -> u32 {
        self.width.div_ceil(MACROBLOCK)
    }

    const fn rows(self) -> u32 {
        self.height.div_ceil(MACROBLOCK)
    }

    /// Bytes of a source line: the width in whole macroblocks.
    pub const fn stride(self) -> usize {
        (self.columns() * MACROBLOCK) as usize
    }

    /// Luma lines of a source picture: the height in whole macroblocks.
    pub const fn lines(self) -> usize {
        (self.rows() * MACROBLOCK) as usize
    }

    /// Bytes of a source picture: luma, then interleaved chroma of half the lines.
    pub const fn source_bytes(self) -> usize {
        self.stride() * self.lines() * 3 / 2
    }

    /// Bytes of a frame's stream buffer: a frame that needs more fails.
    pub const fn stream_bytes(self) -> usize {
        self.source_bytes()
    }

    /// Bytes of a rebuilt luma picture, a rebuilt chroma picture and a motion buffer as Rabbit's driver allocates them; the encoder needs two of each.
    pub const fn work(self) -> (usize, usize, usize) {
        let padded_rows = ((self.rows() * MACROBLOCK + 2 * MACROBLOCK - 1) / MACROBLOCK) & !1;
        let blocks = (padded_rows * self.columns()) as usize;
        let motion_lines = ((self.height + MACROBLOCK - 1) >> 2) & !3;
        let motion_columns = ((self.width + 8 * MACROBLOCK - 1) >> 4) & !7;
        (blocks * LUMA_BYTES, blocks * CHROMA_BYTES, ((2 * motion_lines * motion_columns + 0x7E) & !0x7E) as usize)
    }
}

/// The settings of a stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub shape: Shape,
    pub(super) level: u32,
    /// Frames from one key frame to the next.
    pub group: u32,
    pub(super) frames_per_second: u32,
}

impl Settings {
    pub(super) const fn is_valid(&self) -> bool {
        let Shape { width, height } = self.shape;
        let is_even = width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0;
        let is_small = self.shape.columns() <= SIDE_LIMIT && self.shape.rows() <= SIDE_LIMIT;
        is_even && is_small && self.group >= 1 && self.group <= LONGEST_GROUP
    }

    pub const fn is_key_frame(&self, index: u32) -> bool {
        index.is_multiple_of(self.group)
    }
}

/// The device addresses of a stream's buffers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Memory {
    /// The picture to encode.
    pub source: u32,
    pub stream: u32,
    pub stream_bytes: u32,
    pub luma: [u32; 2],
    pub chroma: [u32; 2],
    pub motion: [u32; 2],
}

impl Memory {
    /// True if the registers can hold every address and the stream's size.
    pub fn is_aligned(&self) -> bool {
        let addresses = [self.source, self.stream].into_iter().chain(self.luma).chain(self.chroma).chain(self.motion);
        addresses.fold(0, |bits, address| bits | address).is_multiple_of(ADDRESS_ALIGNMENT)
            && self.stream_bytes.is_multiple_of(1 << STREAM_UNIT_SHIFT)
    }

    /// The error text of buffers the registers cannot hold.
    pub fn to_unaligned_text(&self) -> String {
        format!("the encoder cannot be given these buffers: {self:x?}")
    }
}

/// The error text of a frame that took too long.
pub fn to_late_text() -> String {
    format!("the encoder did not finish a frame in {TIMEOUT_MS} ms")
}

/// How a frame ended, by the interrupt status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending {
    Done,
    Full,
    Other(u32),
}

impl Ending {
    pub const fn from_status(status: u32) -> Ending {
        match status {
            STATUS_FRAME => Ending::Done,
            STATUS_FULL => Ending::Full,
            other => Ending::Other(other),
        }
    }

    /// True for a finished frame of some bytes, within the `room` of its buffer.
    pub const fn fits(self, bytes: usize, room: usize) -> bool {
        matches!(self, Ending::Done) && bytes > 0 && bytes <= room
    }

    /// The error text of a frame of that many bytes that ended so.
    pub fn to_unfit_text(self, bytes: usize) -> String {
        format!("the encoder ended it as {self:?} with {bytes} bytes")
    }
}

/// Resets the block, which ends its job.
pub fn reset<H: Hardware>(hw: &mut H) {
    for value in [0, 0] {
        hw.write(Block::Venc, RESET, value);
    }
    hw.read(Block::Venc, RESET);
    hw.write(Block::Venc, RESET, 1);
    hw.read(Block::Venc, RESET);
}

/// Programs and starts the block for frame `index` under the held codec lock.
pub fn start<H: Hardware>(hw: &mut H, settings: &Settings, memory: &Memory, index: u32, quantiser: u32) {
    let Settings { shape, level, group, frames_per_second } = *settings;
    let quantiser = quantiser.min(COARSEST);
    reset(hw);

    let short_group = if group <= SHORT_GROUP { group << SHORT_GROUP_SHIFT } else { 0 };
    hw.write(Block::Venc, GROUP, quantiser << INTRA_QUANTISER_SHIFT | short_group | group);
    hw.write(Block::Venc, TOOLS, TOOLS_FIXED | quantiser << INTER_QUANTISER_SHIFT);
    hw.write(Block::Venc, WORD_0C, WORD_0C_FIXED);
    let (columns, rows) = (shape.columns(), shape.rows());
    hw.write(Block::Venc, SIZE, (columns * rows) << BLOCKS_SHIFT | rows << ROWS_SHIFT | columns);
    let (right, bottom) = (shape.stride() as u32 - shape.width, shape.lines() as u32 - shape.height);
    let [first, second] = PITCH_COLUMN_SHIFTS;
    hw.write(Block::Venc, PITCH, columns << first | columns << second | right << PITCH_RIGHT_SHIFT | bottom);
    hw.write(Block::Venc, PROFILE, PROFILE_FIXED | level << LEVEL_SHIFT);
    let rate = [
        (BIT_RATE, 0),
        (FRAME_RATE, if frames_per_second == SIGNALLED_RATE { SIGNALLED_RATE << FRAME_RATE_SHIFT } else { 0 }),
        (QUANTISER_RANGE, QUANTISER_RANGE_FIXED),
        (WORD_54, WORD_54_FIXED),
    ];
    for (offset, value) in rate {
        hw.write(Block::Venc, offset, value);
    }
    hw.write(Block::Venc, BUS_PRIORITY, BUS_PRIORITY_FIXED);

    let group_index = index % group;
    let kind = match group_index {
        0 => INTER_FRAME | KEY_FRAME | ((index / group) & 1) << KEY_COUNT_SHIFT,
        1 => INTER_FRAME,
        _ => INTER_FRAME | LATER_INTER_FRAME,
    };
    hw.write(Block::Venc, FRAME_KIND, kind);
    hw.write(Block::Venc, FRAME_NUMBER, index);
    hw.write(Block::Venc, FRAME_ORDER, index);
    hw.write(Block::Venc, KEY_NUMBER, index - group_index);
    for (offset, value) in rate {
        hw.write(Block::Venc, offset, value);
    }
    hw.write(Block::Venc, WORD_124, 0);
    hw.write(Block::Venc, WORD_128, WORD_128_FIXED);

    hw.write(Block::Venc, STREAM_UNITS, memory.stream_bytes >> STREAM_UNIT_SHIFT);
    hw.write(Block::Venc, STREAM, memory.stream >> ADDRESS_SHIFT);
    hw.write(Block::Venc, SOURCE_LUMA, memory.source >> ADDRESS_SHIFT);
    hw.write(Block::Venc, SOURCE_CHROMA, (memory.source + (shape.stride() * shape.lines()) as u32) >> ADDRESS_SHIFT);
    hw.write(Block::Venc, SOURCE_THIRD, 0);
    let (rebuilt, reference) = if index.is_multiple_of(2) { (1, 0) } else { (0, 1) };
    hw.write(Block::Venc, REBUILT_LUMA, memory.luma[rebuilt] >> ADDRESS_SHIFT);
    hw.write(Block::Venc, REBUILT_CHROMA, memory.chroma[rebuilt] >> ADDRESS_SHIFT);
    hw.write(Block::Venc, REFERENCE_LUMA, memory.luma[reference] >> ADDRESS_SHIFT);
    hw.write(Block::Venc, REFERENCE_CHROMA, memory.chroma[reference] >> ADDRESS_SHIFT);
    hw.write(Block::Venc, MOTION_OUT, memory.motion[reference] >> ADDRESS_SHIFT);
    hw.write(Block::Venc, MOTION_IN, memory.motion[rebuilt] >> ADDRESS_SHIFT);

    hw.write(Block::Venc, INTERRUPT_ENABLE, ALL_INTERRUPTS);
    hw.write(Block::Venc, MODE, MODE_FRAME);
    hw.write(Block::Venc, CLOCK, 1);
    hw.write(Block::Venc, TRIGGER, START_FRAME);
}

/// Reads a frame's stream bytes and clock cycles, and stops the clock.
pub fn finish<H: Hardware>(hw: &mut H) -> (u32, u32) {
    let counts = (hw.read(Block::Venc, BYTE_COUNT), hw.read(Block::Venc, CYCLES));
    hw.write(Block::Venc, CLOCK, 0);
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::fake::Fake;
    use crate::hw::last_write;
    use alloc::vec::Vec;

    const HD: Shape = Shape { width: 1280, height: 720 };
    const STOCK: Settings = Settings { shape: HD, level: 41, group: 4, frames_per_second: 60 };
    const PLACES: Memory = Memory {
        source: 0x903C_A000,
        stream: 0x902C_A000,
        stream_bytes: 0x10_0000,
        luma: [0x9000_7000, 0x9016_0000],
        chroma: [0x900E_D000, 0x9024_6000],
        motion: [0x902B_9000, 0x902C_1000],
    };

    fn writes(settings: &Settings, memory: &Memory, index: u32, quantiser: u32) -> Vec<(usize, u32)> {
        let mut hw = Fake::default();
        start(&mut hw, settings, memory, index, quantiser);
        assert!(hw.writes.iter().all(|write| write.0 == Block::Venc));
        hw.writes.iter().map(|&(_, offset, value)| (offset, value)).collect()
    }

    #[test]
    fn key_frame_words() {
        #[rustfmt::skip]
        let expected = [
            (0xA8, 0), (0xA8, 0), (0xA8, 1),
            (0x04, 0x7801_0004), (0x08, 0x626E_7BAE), (0x0C, 0x924E_0000), (0x24, 0x0E10_2D50), (0x28, 0x0A02_8000), (0x30, 0xCA60_0000),
            (0x48, 0), (0x4C, 0), (0x50, 0x3308), (0x54, 0x800), (0x12C, 0x1F4),
            (0x10, 0x0010_B406), (0x14, 0), (0x18, 0), (0x1C, 0),
            (0x48, 0), (0x4C, 0), (0x50, 0x3308), (0x54, 0x800), (0x124, 0), (0x128, 0x0001_0000),
            (0x68, 0x2000), (0x64, 0x0902_CA00), (0x6C, 0x0903_CA00), (0x70, 0x0904_AB00), (0x94, 0),
            (0x74, 0x0901_6000), (0x78, 0x0902_4600), (0x7C, 0x0900_0700), (0x80, 0x0900_ED00), (0x84, 0x0902_B900), (0x88, 0x0902_C100),
            (0xA4, 0x3F), (0x00, 0x0200_BFE0), (0xEC, 1), (0x58, 4),
        ];
        assert_eq!(writes(&STOCK, &PLACES, 0, 30), expected);
    }

    #[test]
    fn group_frames() {
        let frames: Vec<Vec<(usize, u32)>> = (0..6).map(|index| writes(&STOCK, &PLACES, index, 30)).collect();
        let kinds: Vec<u32> = frames.iter().map(|frame| last_write(frame, FRAME_KIND)).collect();
        assert_eq!(kinds, [0x0010_B406, 0x0010_8400, 0x0010_8500, 0x0010_8500, 0x0010_B40E, 0x0010_8400]);
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(
                (last_write(frame, FRAME_NUMBER), last_write(frame, FRAME_ORDER), last_write(frame, KEY_NUMBER)),
                (index as u32, index as u32, index as u32 / 4 * 4)
            );
            let odd = index % 2 == 1;
            let expected = if odd {
                [0x0900_0700, 0x0900_ED00, 0x0901_6000, 0x0902_4600, 0x0902_C100, 0x0902_B900]
            } else {
                [0x0901_6000, 0x0902_4600, 0x0900_0700, 0x0900_ED00, 0x0902_B900, 0x0902_C100]
            };
            assert_eq!(
                [REBUILT_LUMA, REBUILT_CHROMA, REFERENCE_LUMA, REFERENCE_CHROMA, MOTION_OUT, MOTION_IN]
                    .map(|offset| last_write(frame, offset)),
                expected,
                "frame {index}"
            );
            assert_eq!(frame.len(), frames[0].len());
        }
    }

    #[test]
    fn other_sizes_words() {
        let full_hd =
            Settings { shape: Shape { width: 1920, height: 1080 }, level: 40, group: 30, frames_per_second: 30 };
        let places = Memory { source: 0x9093_6000, ..PLACES };
        let frame = writes(&full_hd, &places, 0, 24);
        let words =
            [GROUP, TOOLS, SIZE, PITCH, PROFILE, FRAME_RATE, SOURCE_CHROMA].map(|offset| last_write(&frame, offset));
        assert_eq!(words, [0x6007_801E, 0x626E_63AE, 0x1FE0_4478, 0x0F03_C008, 0xCA20_0000, 0x001E_0000, 0x090B_3400]);

        let wide = Settings { shape: Shape { width: 864, height: 480 }, level: 31, group: 2, frames_per_second: 60 };
        let frames: Vec<Vec<(usize, u32)>> = (0..6).map(|index| writes(&wide, &PLACES, index, 0)).collect();
        assert_eq!(
            [GROUP, SIZE, PITCH].map(|offset| last_write(&frames[0], offset)),
            [0x0000_8002, 0x0654_1E36, 0x06C1_B000]
        );
        let kinds: Vec<u32> = frames.iter().map(|frame| last_write(frame, FRAME_KIND)).collect();
        assert_eq!(
            kinds,
            [0x0010_B406, 0x0010_8400, 0x0010_B40E, 0x0010_8400, 0x0010_B406, 0x0010_8400],
            "key frames count 0, 1, 0, 1"
        );
        assert_eq!(frames.iter().map(|frame| last_write(frame, KEY_NUMBER)).collect::<Vec<_>>(), [0, 0, 2, 2, 4, 4]);

        let long = Settings { group: 60, ..STOCK };
        assert_eq!(
            last_write(&writes(&long, &PLACES, 0, 51), GROUP),
            0xCC00_003C,
            "a group longer than 30 frames is named once"
        );
        assert_eq!(last_write(&writes(&long, &PLACES, 0, 99), GROUP), 0xCC00_003C, "no quantiser past the coarsest");
        assert_eq!(last_write(&writes(&long, &PLACES, 61, 22), TOOLS), 0x626E_5BAE);
    }

    #[test]
    fn work_buffer_sizes() {
        assert_eq!(HD.work(), (942_080, 471_040, 28_800));
        assert_eq!(Shape { width: 1920, height: 1080 }.work(), (2_088_960, 1_044_480, 65_280));
        assert_eq!(Shape { width: 864, height: 480 }.work(), (414_720, 207_360, 13_440));
        assert_eq!(Shape { width: 640, height: 480 }.work(), (307_200, 153_600, 9_600));
    }

    #[test]
    fn source_size() {
        assert_eq!((HD.stride(), HD.lines(), HD.source_bytes()), (1280, 720, 1_382_400));
        let full_hd = Shape { width: 1920, height: 1080 };
        assert_eq!((full_hd.stride(), full_hd.lines(), full_hd.source_bytes()), (1920, 1088, 3_133_440));
        assert_eq!(Shape { width: 1282, height: 2 }.stride(), 1296);
    }

    #[test]
    fn settings_and_addresses_valid() {
        assert!(STOCK.is_valid() && PLACES.is_aligned());
        assert!(!Settings { group: 0, ..STOCK }.is_valid());
        assert!(!Settings { group: 1024, ..STOCK }.is_valid());
        assert!(Settings { group: 1023, ..STOCK }.is_valid());
        assert!(!Settings { shape: Shape { width: 1281, height: 720 }, ..STOCK }.is_valid());
        assert!(!Settings { shape: Shape { width: 0, height: 720 }, ..STOCK }.is_valid());
        let sized = |width, height| Settings { shape: Shape { width, height }, ..STOCK }.is_valid();
        assert_eq!(
            (sized(4080, 4080), sized(4082, 720), sized(1280, 4082)),
            (true, false, false),
            "a side of 255 macroblocks fills its byte of the size word, one of 256 would spill"
        );
        assert!(!Memory { stream: PLACES.stream + 8, ..PLACES }.is_aligned());
        assert!(!Memory { motion: [PLACES.motion[0], PLACES.motion[1] + 1], ..PLACES }.is_aligned());
        assert!(!Memory { stream_bytes: 1000, ..PLACES }.is_aligned());
        assert!(STOCK.is_key_frame(0) && STOCK.is_key_frame(8) && !STOCK.is_key_frame(9));
    }

    #[test]
    fn frame_end() {
        let mut hw =
            Fake::with(&[(Block::Venc, BYTE_COUNT, 40_276), (Block::Venc, CYCLES, 2_826_853), (Block::Venc, CLOCK, 1)]);
        assert_eq!(finish(&mut hw), (40_276, 2_826_853));
        assert_eq!(hw.writes, [(Block::Venc, CLOCK, 0)]);
        assert_eq!(
            [4, 8, 0, 0x10].map(Ending::from_status),
            [Ending::Done, Ending::Full, Ending::Other(0), Ending::Other(0x10)]
        );
        assert!(Ending::Done.fits(40_276, HD.stream_bytes()) && Ending::Done.fits(1, 1));
        assert!(
            !Ending::Done.fits(0, 1) && !Ending::Done.fits(2, 1) && !Ending::Full.fits(1, 2),
            "a frame of nothing, one past its buffer, and one the block found too large"
        );
        assert_eq!(HD.stream_bytes(), HD.source_bytes(), "a frame's stream gets the room of its picture");
        let mut stuck = Fake::default();
        reset(&mut stuck);
        assert_eq!(stuck.writes, [(Block::Venc, RESET, 0), (Block::Venc, RESET, 0), (Block::Venc, RESET, 1)]);
    }

    #[test]
    fn error_texts() {
        assert_eq!(to_late_text(), "the encoder did not finish a frame in 100 ms");
        assert_eq!(Ending::Full.to_unfit_text(9), "the encoder ended it as Full with 9 bytes");
        assert!(
            PLACES
                .to_unaligned_text()
                .starts_with("the encoder cannot be given these buffers: Memory { source: 903ca000")
        );
    }
}
