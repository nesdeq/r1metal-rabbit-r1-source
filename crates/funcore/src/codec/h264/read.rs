//! Parameter sets and slice heads as a decoder reads them.

use super::{
    CHROMA_420, CROP_UNIT, IMPORTANCE_SHIFT, MACROBLOCK, PICTURE_PARAMETERS, SEQUENCE_PARAMETERS, SMALLEST_COUNT_BITS,
};
use crate::base::bits::BitReader;
use crate::codec::Flaw;
use alloc::collections::BTreeMap;

const SLICE: u8 = 1;
const KEY_SLICE: u8 = 5;
const UNIT_TYPE: u8 = 0x1F;
const HIGH_PROFILES: [u32; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
const SEQUENCES: u32 = 32;
pub(crate) const PICTURES: u32 = 256;
const LONGEST_COUNT_BITS: u32 = 16;
const MOST_REFERENCES: u32 = 32;
pub(super) const MOST_FRAMES: u32 = 16;
const EXTENDED_ASPECT: u32 = 255;
const SLICE_KINDS: u32 = 5;
const KIND_B: u32 = 1;
const KIND_I: u32 = 2;
const LONG_CHANGE: u32 = 2;
const CHANGES_END: u32 = 3;
const DEBLOCKING_OFF: u32 = 1;

const fn is_inter(kind: u32) -> bool {
    kind % SLICE_KINDS != KIND_I
}

#[derive(Clone)]
pub(crate) struct Bits<'a> {
    reader: BitReader<'a>,
    offset: usize,
}

impl<'a> Bits<'a> {
    pub(crate) const fn new(content: &'a [u8]) -> Self {
        Bits { reader: BitReader::with_escapes(content), offset: 0 }
    }

    pub(super) fn bits(&mut self, count: u32) -> Result<u32, Flaw> {
        let value = self.reader.take(count).ok_or(Flaw::Short)?;
        self.offset += count as usize;
        Ok(value)
    }

    pub(super) fn bit(&mut self) -> Result<u32, Flaw> {
        self.bits(1)
    }

    fn flag(&mut self) -> Result<bool, Flaw> {
        Ok(self.bit()? == 1)
    }

    pub(crate) fn skip(&mut self, count: usize) -> Result<(), Flaw> {
        self.reader.skip(count).ok_or(Flaw::Short)?;
        self.offset += count;
        Ok(())
    }

    pub(super) fn unsigned(&mut self) -> Result<u32, Flaw> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros == u32::BITS {
                return Err(Flaw::Invalid("a number beyond 32 bits"));
            }
        }
        Ok((1 << zeros) - 1 + self.bits(zeros)?)
    }

    fn signed(&mut self) -> Result<i32, Flaw> {
        let code = self.unsigned()?;
        let size = code.div_ceil(2) as i32;
        Ok(if code % 2 == 1 { size } else { -size })
    }

    fn bounded(&mut self, limit: u32, name: &'static str) -> Result<u32, Flaw> {
        let value = self.unsigned()?;
        if value > limit { Err(Flaw::Invalid(name)) } else { Ok(value) }
    }

    fn has_more(&self) -> bool {
        let bytes = self.reader.bytes;
        let Some(last) = bytes.iter().rposition(|&byte| byte != 0) else {
            return false;
        };
        self.reader.position() < 8 * last + 7 - bytes[last].trailing_zeros() as usize
    }

    pub(crate) fn peek(&self) -> (u32, u32) {
        let mut copy = self.clone();
        (0..u32::BITS).fold((0, 0), |(value, count), _| match copy.bit() {
            Ok(bit) => (value << 1 | bit, count + 1),
            Err(_) => (value << 1, count),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Header {
    pub(crate) importance: u8,
    pub(super) kind: u8,
}

impl Header {
    pub(crate) fn parse(unit: &[u8]) -> Option<Header> {
        unit.first().map(|&byte| Header { importance: byte >> IMPORTANCE_SHIFT & 3, kind: byte & UNIT_TYPE })
    }

    pub(crate) const fn is_slice(self) -> bool {
        self.kind == SLICE || self.kind == KEY_SLICE
    }

    pub(crate) const fn is_key_frame(self) -> bool {
        self.kind == KEY_SLICE
    }
}

/// pic_order_cnt_type: the low bits in each slice, or the frame number alone.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Order {
    Lsb {
        bits: u32,
    },
    #[default]
    Frame,
}

impl Order {
    pub(crate) const fn kind(self) -> u32 {
        match self {
            Order::Lsb { .. } => 0,
            Order::Frame => 2,
        }
    }
}

/// A sequence parameter set of 4:2:0 frames of 8 bits.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Sequence {
    pub id: u32,
    pub profile: u32,
    /// Ten times the level.
    pub level: u32,
    /// constraint_set3_flag: level 11 of the lower profiles is level 1b.
    pub third_constraint: bool,
    /// Bits of a frame number.
    pub number_bits: u32,
    pub order: Order,
    /// Frames a picture may refer to.
    pub references: u32,
    /// Macroblocks across a frame.
    pub columns: u32,
    /// Macroblocks down a frame.
    pub rows: u32,
    pub direct_8x8: bool,
    /// Pixels cropped at the left, right, top and bottom.
    pub crop: [u32; 4],
    /// Samples use the whole range of a byte.
    pub is_full_range: bool,
    /// matrix_coefficients, where the stream names them.
    pub matrix: Option<u32>,
    /// max_num_reorder_frames, where the stream names it.
    pub reorder: Option<u32>,
    /// max_dec_frame_buffering, where the stream names it.
    pub buffering: Option<u32>,
}

fn skip_hrd(bits: &mut Bits<'_>) -> Result<(), Flaw> {
    let count = bits.bounded(31, "buffer model count")? + 1;
    bits.bits(8)?;
    for _ in 0..count {
        bits.unsigned()?;
        bits.unsigned()?;
        bits.bit()?;
    }
    bits.bits(20).map(|_| ())
}

impl Sequence {
    pub(crate) fn read(unit: &[u8]) -> Result<Sequence, Flaw> {
        let mut bits = Bits::new(unit.get(1..).ok_or(Flaw::Short)?);
        let profile = bits.bits(8)?;
        let constraints = bits.bits(8)?;
        let level = bits.bits(8)?;
        let id = bits.bounded(SEQUENCES - 1, "sequence parameter set number")?;
        if HIGH_PROFILES.contains(&profile) {
            let chroma = bits.bounded(3, "chroma format")?;
            if chroma != CHROMA_420 {
                return Err(Flaw::Unsupported("a chroma format other than 4:2:0"));
            }
            if bits.unsigned()? != 0 || bits.unsigned()? != 0 {
                return Err(Flaw::Unsupported("samples of more than 8 bits"));
            }
            if bits.flag()? {
                return Err(Flaw::Unsupported("lossless macroblocks"));
            }
            if bits.flag()? {
                return Err(Flaw::Unsupported("scaling lists"));
            }
        }
        let number_bits =
            bits.bounded(LONGEST_COUNT_BITS - SMALLEST_COUNT_BITS, "frame number bits")? + SMALLEST_COUNT_BITS;
        let order = match bits.bounded(2, "picture order kind")? {
            0 => Order::Lsb {
                bits: bits.bounded(LONGEST_COUNT_BITS - SMALLEST_COUNT_BITS, "picture order bits")?
                    + SMALLEST_COUNT_BITS,
            },
            1 => return Err(Flaw::Unsupported("picture order counts in cycles")),
            _ => Order::Frame,
        };
        let references = bits.bounded(MOST_FRAMES, "reference frames")?;
        // gaps_in_frame_num_allowed_flag: a gap is filled whatever this says.
        bits.flag()?;
        let columns = bits.unsigned()?.checked_add(1).ok_or(Flaw::Invalid("width"))?;
        let rows = bits.unsigned()?.checked_add(1).ok_or(Flaw::Invalid("height"))?;
        if !bits.flag()? {
            return Err(Flaw::Unsupported("interlaced video"));
        }
        let direct_8x8 = bits.flag()?;
        let mut crop = [0u32; 4];
        if bits.flag()? {
            for edge in &mut crop {
                *edge = bits.unsigned()?.checked_mul(CROP_UNIT).ok_or(Flaw::Invalid("cropping"))?;
            }
        }
        let (width, height) = (columns.checked_mul(MACROBLOCK), rows.checked_mul(MACROBLOCK));
        let fits = |length: Option<u32>, near: u32, far: u32| {
            length.is_some_and(|length| near.checked_add(far).is_some_and(|off| off < length))
        };
        if !fits(width, crop[0], crop[1]) || !fits(height, crop[2], crop[3]) {
            return Err(Flaw::Invalid("picture size"));
        }
        let mut sequence = Sequence {
            id,
            profile,
            level,
            third_constraint: constraints >> 4 & 1 == 1,
            number_bits,
            order,
            references,
            columns,
            rows,
            direct_8x8,
            crop,
            ..Sequence::default()
        };
        if bits.flag()? {
            sequence.read_vui(&mut bits)?;
        }
        Ok(sequence)
    }

    fn read_vui(&mut self, bits: &mut Bits<'_>) -> Result<(), Flaw> {
        if bits.flag()? && bits.bits(8)? == EXTENDED_ASPECT {
            bits.bits(32)?;
        }
        if bits.flag()? {
            bits.bit()?;
        }
        if bits.flag()? {
            bits.bits(3)?;
            self.is_full_range = bits.flag()?;
            if bits.flag()? {
                bits.bits(16)?;
                self.matrix = Some(bits.bits(8)?);
            }
        }
        if bits.flag()? {
            bits.unsigned()?;
            bits.unsigned()?;
        }
        if bits.flag()? {
            bits.bits(32)?;
            bits.bits(32)?;
            bits.bit()?;
        }
        let has_nal_hrd = bits.flag()?;
        if has_nal_hrd {
            skip_hrd(bits)?;
        }
        let has_vcl_hrd = bits.flag()?;
        if has_vcl_hrd {
            skip_hrd(bits)?;
        }
        if has_nal_hrd || has_vcl_hrd {
            bits.bit()?;
        }
        bits.bit()?;
        if bits.flag()? {
            bits.bit()?;
            for _ in 0..4 {
                bits.unsigned()?;
            }
            self.reorder = Some(bits.bounded(MOST_FRAMES, "reordered frames")?);
            self.buffering = Some(bits.bounded(MOST_FRAMES, "buffered frames")?);
        }
        Ok(())
    }

    /// Pixels across and down a cropped picture.
    pub const fn size(&self) -> (u32, u32) {
        (self.columns * MACROBLOCK - self.crop[0] - self.crop[1], self.rows * MACROBLOCK - self.crop[2] - self.crop[3])
    }

    pub(crate) const fn numbers(&self) -> u32 {
        1 << self.number_bits
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Picture {
    pub(crate) id: u32,
    pub(crate) sequence: u32,
    pub(crate) is_cabac: bool,
    pub(crate) has_bottom_order: bool,
    pub(crate) last_reference: [u32; 2],
    pub(crate) is_weighted: bool,
    pub(crate) bipred_idc: u32,
    pub(crate) quantiser: i32,
    pub(crate) chroma_offset: i32,
    pub(crate) second_chroma_offset: i32,
    pub(crate) has_deblocking_control: bool,
    pub(crate) constrained_intra: bool,
    pub(crate) has_redundant_count: bool,
    pub(crate) transform_8x8: bool,
}

impl Picture {
    pub(crate) fn read(unit: &[u8]) -> Result<Picture, Flaw> {
        let mut bits = Bits::new(unit.get(1..).ok_or(Flaw::Short)?);
        let id = bits.bounded(PICTURES - 1, "picture parameter set number")?;
        let sequence = bits.bounded(SEQUENCES - 1, "sequence parameter set number")?;
        let (is_cabac, has_bottom_order) = (bits.flag()?, bits.flag()?);
        if bits.unsigned()? != 0 {
            return Err(Flaw::Unsupported("slice groups"));
        }
        let last_reference = [
            bits.bounded(MOST_REFERENCES - 1, "default references")?,
            bits.bounded(MOST_REFERENCES - 1, "default references")?,
        ];
        let (is_weighted, bipred_idc) = (bits.flag()?, bits.bits(2)?);
        let quantiser = bits.signed()?;
        bits.signed()?;
        let chroma_offset = bits.signed()?;
        let (has_deblocking_control, constrained_intra, has_redundant_count) =
            (bits.flag()?, bits.flag()?, bits.flag()?);
        let mut picture = Picture {
            id,
            sequence,
            is_cabac,
            has_bottom_order,
            last_reference,
            is_weighted,
            bipred_idc,
            quantiser,
            chroma_offset,
            second_chroma_offset: chroma_offset,
            has_deblocking_control,
            constrained_intra,
            has_redundant_count,
            ..Picture::default()
        };
        if bits.has_more() {
            picture.transform_8x8 = bits.flag()?;
            if bits.flag()? {
                return Err(Flaw::Unsupported("scaling lists"));
            }
            picture.second_chroma_offset = bits.signed()?;
        }
        Ok(picture)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Marking {
    #[default]
    Unused,
    Key {
        discard: bool,
    },
    Sliding,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Slice {
    pub(crate) first_mb: u32,
    pub(crate) kind: u32,
    pub(crate) picture: u32,
    pub(super) number: u32,
    pub(super) order_low: u32,
    pub(super) order_bottom: i32,
    pub(crate) last_reference: u32,
    pub(super) marking: Marking,
    pub(crate) arithmetic_table: u32,
    pub(crate) quantiser_step: i32,
    pub(crate) deblocking: u32,
    pub(crate) alpha: i32,
    pub(crate) beta: i32,
    pub(crate) has_weights: bool,
    pub(crate) changes_at: usize,
    pub(crate) rest_at: usize,
    pub(crate) end: usize,
}

impl Slice {
    pub(crate) const fn is_inter(&self) -> bool {
        is_inter(self.kind)
    }

    pub(crate) fn read(unit: &[u8], sets: &Sets) -> Result<Slice, Flaw> {
        let header = Header::parse(unit).filter(|header| header.is_slice()).ok_or(Flaw::Invalid("not a slice"))?;
        let mut bits = Bits::new(&unit[1..]);
        let first_mb = bits.unsigned()?;
        let kind = bits.bounded(2 * SLICE_KINDS - 1, "slice type")?;
        match kind % SLICE_KINDS {
            KIND_B => return Err(Flaw::Unsupported("B slices")),
            ..=KIND_I => {}
            _ => return Err(Flaw::Unsupported("switching slices")),
        }
        let picture = bits.bounded(PICTURES - 1, "picture parameter set number")?;
        let (sequence, set) = sets.get(picture)?;
        let is_inter = is_inter(kind);
        let number = bits.bits(sequence.number_bits)?;
        if header.is_key_frame() {
            bits.unsigned()?;
        }
        let (mut order_low, mut order_bottom) = (0, 0);
        if let Order::Lsb { bits: count } = sequence.order {
            order_low = bits.bits(count)?;
            if set.has_bottom_order {
                order_bottom = bits.signed()?;
            }
        }
        if set.has_redundant_count {
            bits.unsigned()?;
        }
        let mut last_reference = set.last_reference[0];
        if is_inter && bits.flag()? {
            last_reference = bits.bounded(MOST_REFERENCES - 1, "references of a slice")?;
        }
        let changes_at = bits.offset;
        if is_inter && bits.flag()? {
            loop {
                match bits.bounded(CHANGES_END, "reference list change")? {
                    CHANGES_END => break,
                    LONG_CHANGE => return Err(Flaw::Unsupported("long term references")),
                    _ => bits.unsigned()?,
                };
            }
        }
        let has_weights = set.is_weighted && is_inter;
        if has_weights {
            bits.unsigned()?;
            bits.unsigned()?;
            for _ in 0..=last_reference {
                if bits.flag()? {
                    bits.signed()?;
                    bits.signed()?;
                }
                if bits.flag()? {
                    for _ in 0..4 {
                        bits.signed()?;
                    }
                }
            }
        }
        let rest_at = bits.offset;
        let marking = match (header.importance, header.is_key_frame()) {
            (0, _) => Marking::Unused,
            (_, true) => match (bits.flag()?, bits.flag()?) {
                (_, true) => return Err(Flaw::Unsupported("long term references")),
                (discard, false) => Marking::Key { discard },
            },
            (_, false) if !bits.flag()? => Marking::Sliding,
            (_, false) => return Err(Flaw::Unsupported("memory management commands")),
        };
        let arithmetic_table = if set.is_cabac && is_inter { bits.bounded(2, "arithmetic coding table")? } else { 0 };
        let quantiser_step = bits.signed()?;
        let (mut deblocking, mut alpha, mut beta) = (0, 0, 0);
        if set.has_deblocking_control {
            deblocking = bits.bounded(2, "deblocking")?;
            if deblocking != DEBLOCKING_OFF {
                (alpha, beta) = (bits.signed()?, bits.signed()?);
            }
        }
        Ok(Slice {
            first_mb,
            kind,
            picture,
            number,
            order_low,
            order_bottom,
            last_reference,
            marking,
            arithmetic_table,
            quantiser_step,
            deblocking,
            alpha,
            beta,
            has_weights,
            changes_at,
            rest_at,
            end: bits.offset,
        })
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Sets {
    sequences: BTreeMap<u32, Sequence>,
    pictures: BTreeMap<u32, Picture>,
}

impl Sets {
    pub(crate) fn add(&mut self, unit: &[u8]) -> Result<bool, Flaw> {
        match Header::parse(unit).map(|header| header.kind) {
            Some(SEQUENCE_PARAMETERS) => {
                let sequence = Sequence::read(unit)?;
                self.sequences.insert(sequence.id, sequence);
            }
            Some(PICTURE_PARAMETERS) => {
                let picture = Picture::read(unit)?;
                self.pictures.insert(picture.id, picture);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(crate) fn get(&self, picture: u32) -> Result<(&Sequence, &Picture), Flaw> {
        let set = self.pictures.get(&picture).ok_or(Flaw::Missing("picture parameter set"))?;
        let sequence = self.sequences.get(&set.sequence).ok_or(Flaw::Missing("sequence parameter set"))?;
        Ok((sequence, set))
    }
}

#[cfg(test)]
pub(crate) mod samples {
    use super::{Sequence, Sets};
    use crate::codec::h264::REC_709;
    use crate::codec::h264::write::Stream;

    pub(crate) fn sized(columns: u32, rows: u32) -> Sequence {
        Sequence {
            profile: 100,
            level: 41,
            number_bits: 4,
            references: 1,
            columns,
            rows,
            direct_8x8: true,
            matrix: Some(REC_709),
            ..Sequence::default()
        }
    }

    pub(crate) fn sets(units: &[&[u8]]) -> Sets {
        let mut sets = Sets::default();
        for unit in units {
            assert_eq!(sets.add(unit), Ok(true));
        }
        sets
    }

    pub(crate) const OURS: Stream =
        Stream { width: 1280, height: 720, level: 41, pace: Some((7999, 480_000)), is_chroma_on_luma: true };
    pub(crate) const OUR_KEY: [u8; 10] = [0x65, 0x88, 0x80, 0x00, 0x40, 0x00, 0x02, 0x7F, 0xFA, 0x83];
    pub(crate) const OUR_NEXT: [u8; 10] = [0x41, 0x9A, 0x00, 0x02, 0x00, 0x05, 0x91, 0x1F, 0xFE, 0xAE];
    pub(crate) const X264_SEQUENCE: [u8; 27] = [
        0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBA, 0x6A, 0x02, 0x02, 0x03, 0x6E, 0x00, 0x00, 0x3E,
        0x7E, 0x00, 0x1D, 0x4C, 0x00, 0x1E, 0x30, 0x63, 0x2C,
    ];
    pub(crate) const X264_PICTURE: [u8; 6] = [0x68, 0xE9, 0x38, 0xF2, 0xC8, 0xB0];
    pub(crate) const X264_SLICES: [[u8; 12]; 6] = [
        [0x65, 0x88, 0x84, 0x00, 0x9F, 0xF7, 0xE5, 0x3E, 0x05, 0x28, 0xD2, 0x59],
        [0x41, 0x9A, 0x24, 0x6C, 0x7F, 0xD9, 0x00, 0xD6, 0x5E, 0xFA, 0x57, 0xEB],
        [0x41, 0x9E, 0x42, 0x38, 0x93, 0xFF, 0xD0, 0x73, 0x1F, 0xD6, 0x9F, 0xB5],
        [0x01, 0x9E, 0x61, 0x34, 0x44, 0x7F, 0xEA, 0x78, 0x2A, 0x34, 0x5B, 0x72],
        [0x01, 0x9E, 0x63, 0x6A, 0x4D, 0xFF, 0xD8, 0x4F, 0xCE, 0x74, 0x41, 0x36],
        [0x41, 0x9A, 0x68, 0x35, 0x08, 0x2D, 0x13, 0x29, 0x81, 0x27, 0x29, 0xEC],
    ];
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;
    use crate::codec::h264::write::Bits as Writer;
    use crate::codec::h264::write::Stream;
    use crate::codec::h264::write::to_unit;
    use alloc::vec::Vec;

    const CROPPED_SEQUENCE: [u8; 27] = [
        0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0xD8, 0x3D, 0xE6, 0xE6, 0xA0, 0x20, 0x20, 0x28, 0x00, 0x00, 0xF9,
        0xF8, 0x00, 0x75, 0x30, 0x00, 0x78, 0xC1, 0x8C, 0xB0,
    ];
    const LISTS_PICTURE: [u8; 7] = [0x68, 0xEA, 0xA3, 0xCB, 0x30, 0x02, 0xC0];
    const INTERLACED_SEQUENCE: [u8; 16] =
        [0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40, 0x50, 0x0B, 0xBF, 0x2C, 0xD4, 0x04, 0x04, 0x06, 0xDC];

    #[test]
    fn bits_skip_escapes() {
        let mut bits = Bits::new(&[0b1011_0100, 0, 0, 3, 0b0100_0000, 0, 0, 3, 3]);
        assert_eq!((bits.bits(3), bits.unsigned(), bits.unsigned()), (Ok(0b101), Ok(0), Ok(1)));
        assert_eq!(bits.offset, 7);
        assert_eq!(bits.bits(19), Ok(1), "a zero bit, sixteen from the zero bytes, and the 01 behind the escape byte");
        assert_eq!(
            (bits.bits(6), bits.bits(16), bits.bits(8)),
            (Ok(0), Ok(0), Ok(3)),
            "an escape byte and the three it guards"
        );
        assert_eq!((bits.offset, bits.bit()), (56, Err(Flaw::Short)));
        let mut signed = Bits::new(&[0b0100_1110]);
        assert_eq!([signed.signed(), signed.signed(), signed.signed()], [Ok(1), Ok(-1), Ok(0)]);
        assert_eq!(
            Bits::new(&[0, 0, 0, 1, 0xFF, 0xFF, 0xFF, 0xFE]).unsigned(),
            Ok(u32::MAX - 1),
            "31 zeros, a one and 31 ones: the largest number"
        );
        assert_eq!(Bits::new(&[0, 0, 0, 0, 0xFF]).unsigned(), Err(Flaw::Invalid("a number beyond 32 bits")));
        assert_eq!(Bits::new(&[0, 0]).unsigned(), Err(Flaw::Short));
    }

    #[test]
    fn peek_and_has_more() {
        let mut bits = Bits::new(&[0x12, 0x34, 0, 0, 3, 0x56, 0x78, 0x80, 0]);
        assert_eq!(bits.peek(), (0x1234_0000, 32));
        bits.skip(12).unwrap();
        assert_eq!((bits.peek(), bits.offset), ((0x4000_0567, 32), 12));
        bits.skip(28).unwrap();
        assert!(bits.has_more(), "the byte before the closing bit is content");
        assert_eq!(bits.peek(), (0x7880_0000, 24), "zeros stand for what the content does not have");
        bits.skip(8).unwrap();
        assert!(!bits.has_more(), "what is left is the closing bit and zeros");
        assert_eq!((bits.peek(), bits.skip(17)), ((0x8000_0000, 16), Err(Flaw::Short)));
        assert!(!Bits::new(&[0, 0]).has_more() && Bits::new(&[0x40]).has_more() && !Bits::new(&[0x80]).has_more());
    }

    #[test]
    fn own_parameter_sets_round_trip() {
        let sequence = Sequence::read(&OURS.sequence_parameters()).unwrap();
        assert_eq!((sequence.id, sequence.profile, sequence.level), (0, 100, 41));
        assert_eq!((sequence.number_bits, sequence.order, sequence.references), (16, Order::Lsb { bits: 16 }, 1));
        assert_eq!(
            (sequence.columns, sequence.rows, sequence.direct_8x8, sequence.crop, sequence.size()),
            (80, 45, true, [0; 4], (1280, 720))
        );
        assert_eq!(
            (sequence.is_full_range, sequence.matrix, sequence.reorder, sequence.buffering),
            (false, Some(1), Some(0), Some(2))
        );
        assert_eq!((sequence.numbers(), sequence.order.kind()), (65536, 0));
        let picture = Picture {
            is_cabac: true,
            last_reference: [1, 1],
            has_deblocking_control: true,
            transform_8x8: true,
            ..Picture::default()
        };
        assert_eq!(Picture::read(&OURS.picture_parameters()), Ok(picture));
        let unpaced =
            Sequence::read(&Stream { pace: None, is_chroma_on_luma: false, ..OURS }.sequence_parameters()).unwrap();
        assert_eq!((unpaced.reorder, unpaced.buffering, unpaced.matrix), (Some(0), Some(2), Some(1)));
        let full_hd = Sequence::read(&Stream { width: 1920, height: 1080, ..OURS }.sequence_parameters()).unwrap();
        assert_eq!(
            (full_hd.columns, full_hd.rows, full_hd.crop, full_hd.size()),
            (120, 68, [0, 0, 0, 8], (1920, 1080))
        );
    }

    #[test]
    fn x264_parameter_sets() {
        let sequence = Sequence::read(&X264_SEQUENCE).unwrap();
        assert_eq!(
            (sequence.profile, sequence.level, sequence.number_bits, sequence.order, sequence.references),
            (100, 40, 4, Order::Lsb { bits: 6 }, 4)
        );
        assert_eq!(
            (sequence.size(), sequence.reorder, sequence.buffering, sequence.matrix),
            ((1280, 720), Some(2), Some(4), Some(1))
        );
        let picture = Picture {
            is_cabac: true,
            last_reference: [3, 0],
            is_weighted: true,
            bipred_idc: 2,
            quantiser: -3,
            chroma_offset: -2,
            second_chroma_offset: -2,
            has_deblocking_control: true,
            transform_8x8: true,
            ..Picture::default()
        };
        assert_eq!(Picture::read(&X264_PICTURE), Ok(picture));
        let cropped = Sequence::read(&CROPPED_SEQUENCE).unwrap();
        assert_eq!((cropped.columns, cropped.rows, cropped.crop, cropped.size()), (54, 30, [0, 10, 0, 0], (854, 480)));
    }

    fn baseline(order: u32) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.put(66 << 16 | 30, 24);
        for value in [3, 2, order, 2] {
            writer.unsigned(value);
        }
        writer.flag(true);
        writer.unsigned(19);
        writer.unsigned(14);
        writer.flags(&[true, false, false, false]);
        to_unit(3, 7, &writer.end())
    }

    #[test]
    fn baseline_without_vui() {
        let sequence = Sequence::read(&baseline(2)).unwrap();
        assert_eq!(
            (sequence.id, sequence.profile, sequence.number_bits, sequence.order, sequence.references, sequence.size()),
            (3, 66, 6, Order::Frame, 2, (320, 240))
        );
        assert_eq!(
            (sequence.order.kind(), sequence.direct_8x8, sequence.reorder, sequence.matrix),
            (2, false, None, None),
            "no usability information"
        );
    }

    #[test]
    fn unsupported_streams_refused() {
        assert_eq!(Sequence::read(&INTERLACED_SEQUENCE), Err(Flaw::Unsupported("interlaced video")));
        let high_10 = [0x67, 0x6E, 0x00, 0x28, 0b1010_0100];
        assert_eq!(Sequence::read(&high_10), Err(Flaw::Unsupported("samples of more than 8 bits")));
        let high_422 = [0x67, 0x7A, 0x00, 0x28, 0b1011_0000];
        assert_eq!(Sequence::read(&high_422), Err(Flaw::Unsupported("a chroma format other than 4:2:0")));
        let mut writer = Writer::default();
        writer.put(100 << 16 | 31, 24);
        for value in [0, 1, 0, 0] {
            writer.unsigned(value);
        }
        writer.flags(&[false, true]);
        assert_eq!(Sequence::read(&to_unit(3, 7, &writer.end())), Err(Flaw::Unsupported("scaling lists")));
        assert_eq!(Picture::read(&LISTS_PICTURE), Err(Flaw::Unsupported("scaling lists")), "x264's, of default lists");
        assert_eq!(Sequence::read(&baseline(1)), Err(Flaw::Unsupported("picture order counts in cycles")));
        assert_eq!(Sequence::read(&X264_SEQUENCE[..9]), Err(Flaw::Short));
        assert_eq!(Picture::read(&[0x68, 0b1101_0100]), Err(Flaw::Unsupported("slice groups")));
        assert_eq!((Sequence::read(&[]), Picture::read(&[0x68])), (Err(Flaw::Short), Err(Flaw::Short)));
    }

    #[test]
    fn own_slice_heads() {
        let sets = sets(&[&OURS.sequence_parameters(), &OURS.picture_parameters()]);
        let key = Slice::read(&OUR_KEY, &sets).unwrap();
        assert_eq!(
            (key.first_mb, key.kind, key.picture, key.number, key.order_low, key.last_reference),
            (0, 7, 0, 0, 0, 1)
        );
        assert_eq!(
            (key.marking, key.quantiser_step, key.deblocking, key.alpha, key.beta, key.has_weights),
            (Marking::Key { discard: false }, 2, 0, 0, 0, false)
        );
        assert_eq!((key.changes_at, key.rest_at, key.end, key.is_inter()), (42, 42, 52, false));
        let next = Slice::read(&OUR_NEXT, &sets).unwrap();
        assert_eq!(
            (next.kind, next.number, next.order_low, next.last_reference, next.marking),
            (5, 1, 2, 0, Marking::Sliding)
        );
        assert_eq!(
            (next.arithmetic_table, next.quantiser_step, next.changes_at, next.rest_at, next.end),
            (0, 4, 41, 42, 54)
        );
        assert!(next.is_inter(), "our encoder writes I and P slices, and no B slices");
    }

    #[test]
    fn x264_slice_heads() {
        let sets = sets(&[&X264_SEQUENCE, &X264_PICTURE]);
        let results = X264_SLICES.map(|unit| Slice::read(&unit, &sets));
        assert!(results[2..5].iter().all(|slice| *slice == Err(Flaw::Unsupported("B slices"))));
        let slices = [0, 1, 5].map(|at| results[at].clone().unwrap());
        assert_eq!(slices.clone().map(|slice| slice.kind), [7, 5, 5]);
        assert_eq!(slices.clone().map(|slice| (slice.number, slice.order_low)), [(0, 0), (1, 8), (3, 16)]);
        assert_eq!(
            slices.clone().map(|slice| (slice.last_reference, slice.has_weights)),
            [(3, false), (0, true), (3, true)]
        );
        assert_eq!(
            slices.clone().map(|slice| (slice.changes_at, slice.rest_at, slice.end)),
            [(20, 20, 30), (19, 24, 30), (18, 62, 72)]
        );
        assert_eq!(slices.clone().map(|slice| slice.quantiser_step), [2, 0, 2]);
        let markings = [Marking::Key { discard: false }, Marking::Sliding, Marking::Sliding];
        assert_eq!(slices.map(|slice| slice.marking), markings);
    }

    #[test]
    fn marking_fields_and_refusals() {
        let sets = sets(&[&X264_SEQUENCE, &X264_PICTURE]);
        let inter_slice = |long_change: bool, commands: bool| {
            let mut head = Writer::default();
            for value in [0, 5, 0] {
                head.unsigned(value);
            }
            head.put(7, 4);
            head.put(20, 6);
            head.flag(true);
            head.unsigned(0);
            head.flag(long_change);
            if long_change {
                head.unsigned(LONG_CHANGE);
            }
            for value in [0, 0] {
                head.unsigned(value);
            }
            head.flags(&[false, false, commands]);
            head.unsigned(1);
            head.signed(-3);
            head.unsigned(2);
            head.signed(1);
            head.signed(-2);
            Slice::read(&to_unit(2, 1, &head.end()), &sets)
        };
        let slice = inter_slice(false, false).unwrap();
        assert_eq!(
            (slice.marking, slice.number, slice.order_low, slice.last_reference, slice.has_weights),
            (Marking::Sliding, 7, 20, 0, true)
        );
        assert_eq!(
            (slice.arithmetic_table, slice.quantiser_step, slice.deblocking, slice.alpha, slice.beta),
            (1, -3, 2, 1, -2)
        );
        assert_eq!((slice.changes_at, slice.rest_at), (19, 24));
        let long_term = Err(Flaw::Unsupported("long term references"));
        assert_eq!(inter_slice(true, false), long_term, "a change of the list that names a long term reference");
        assert_eq!(inter_slice(false, true), Err(Flaw::Unsupported("memory management commands")));
        let key = |is_long_term: bool| {
            let mut writer = Writer::default();
            for value in [9, 7, 0] {
                writer.unsigned(value);
            }
            writer.put(0, 4);
            writer.unsigned(3);
            writer.put(0, 6);
            writer.flags(&[true, is_long_term]);
            writer.signed(5);
            writer.unsigned(1);
            Slice::read(&to_unit(3, 5, &writer.end()), &sets)
        };
        let slice = key(false).unwrap();
        assert_eq!((slice.first_mb, slice.marking, slice.quantiser_step), (9, Marking::Key { discard: true }, 5));
        assert_eq!(
            (slice.deblocking, slice.alpha, slice.beta, slice.changes_at, slice.end),
            (1, 0, 0, 30, 42),
            "a filter that is off has no offsets"
        );
        assert_eq!(key(true), long_term, "a key picture kept as a long term reference");
    }

    #[test]
    fn slices_need_parameter_sets() {
        assert_eq!(Slice::read(&OUR_KEY, &Sets::default()), Err(Flaw::Missing("picture parameter set")));
        assert_eq!(
            Slice::read(&OUR_KEY, &sets(&[&OURS.picture_parameters()])),
            Err(Flaw::Missing("sequence parameter set"))
        );
        let ours = sets(&[&OURS.sequence_parameters(), &OURS.picture_parameters()]);
        assert_eq!(Slice::read(&[0x41, 0b1001_0010], &ours), Err(Flaw::Unsupported("switching slices")));
        assert_eq!(Slice::read(&[0x67, 0x88], &ours), Err(Flaw::Invalid("not a slice")));
        assert_eq!(Slice::read(&OUR_KEY[..5], &ours), Err(Flaw::Short));
        let mut sets = Sets::default();
        assert_eq!((sets.add(&OUR_KEY), sets.add(&[]), sets.add(&[0x67])), (Ok(false), Ok(false), Err(Flaw::Short)));
    }

    #[test]
    fn unit_headers() {
        let headers = [0x65u8, 0x41, 0x01, 0x67, 0x06].map(|byte| Header::parse(&[byte]).unwrap());
        assert_eq!(headers.map(|header| (header.importance, header.kind)), [(3, 5), (2, 1), (0, 1), (3, 7), (0, 6)]);
        assert_eq!(headers.map(Header::is_slice), [true, true, true, false, false]);
        assert_eq!(headers.map(Header::is_key_frame), [true, false, false, false, false]);
        assert_eq!(Header::parse(&[]), None);
    }
}
