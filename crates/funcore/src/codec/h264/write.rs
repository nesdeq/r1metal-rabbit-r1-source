//! The hardware encoder's parameter sets, and its byte stream cut into units.

use super::{
    CHROMA_420, CROP_UNIT, IMPORTANCE_SHIFT, MACROBLOCK, PICTURE_PARAMETERS, REC_709, SEQUENCE_PARAMETERS,
    SMALLEST_COUNT_BITS,
};
use crate::base::bits::H264_ESCAPE;
use alloc::vec::Vec;

const HIGHEST_IMPORTANCE: u8 = 3;
const HIGH_PROFILE: u32 = 100;
const FRAME_NUMBER_BITS: u32 = 16;
const ORDER_COUNT_BITS: u32 = 16;
const REFERENCE_FRAMES: u32 = 1;
const DEFAULT_REFERENCES: u32 = 2;
const VIDEO_FORMAT: u32 = 5;
const CHROMA_ON_TOP_LEFT_LUMA: u32 = 2;
const VECTOR_LENGTH_BITS: u32 = 16;
const DECODER_FRAMES: u32 = 2;
const FIELDS_PER_FRAME: u32 = 2;
const START_CODE: [u8; 3] = [0, 0, 1];
const ESCAPE_LIMIT: u8 = 4;
pub(crate) const LENGTH_BYTES: usize = size_of::<u32>();

#[derive(Default)]
pub(super) struct Bits(crate::base::bits::BitWriter);

impl Bits {
    pub(super) fn put(&mut self, value: u32, count: u32) {
        self.0.put(value, count);
    }

    pub(super) fn flag(&mut self, is_set: bool) {
        self.put(u32::from(is_set), 1);
    }

    pub(super) fn flags(&mut self, flags: &[bool]) {
        flags.iter().for_each(|&is_set| self.flag(is_set));
    }

    pub(super) fn unsigned(&mut self, value: u32) {
        let code = value + 1;
        let length = u32::BITS - code.leading_zeros();
        self.put(0, length - 1);
        self.put(code, length);
    }

    pub(super) fn signed(&mut self, value: i32) {
        self.unsigned(if value > 0 { 2 * value as u32 - 1 } else { 2 * value.unsigned_abs() });
    }

    pub(super) fn end(mut self) -> Vec<u8> {
        self.put(1, 1);
        self.0.finish()
    }
}

pub(super) fn to_unit(importance: u8, kind: u8, content: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(content.len() + 2);
    bytes.push(importance << IMPORTANCE_SHIFT | kind);
    let mut zeros = 0;
    for &byte in content {
        if zeros >= 2 && byte < ESCAPE_LIMIT {
            bytes.push(H264_ESCAPE);
            zeros = 0;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        bytes.push(byte);
    }
    bytes
}

/// A stream of the hardware encoder, as its parameter sets describe it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stream {
    pub width: u32,
    pub height: u32,
    /// Ten times the level.
    pub level: u32,
    /// Ticks per frame and per second, for frames at a steady pace.
    pub pace: Option<(u32, u32)>,
    /// Chroma samples lie on the top left luma sample of their four.
    pub is_chroma_on_luma: bool,
}

impl Stream {
    /// The sequence parameter set, without a start code.
    pub fn sequence_parameters(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        // profile_idc, no constraint_set flags, level_idc, seq_parameter_set_id 0 (H.264 7.3.2.1.1).
        bits.put(HIGH_PROFILE, 8);
        bits.put(0, 8);
        bits.put(self.level, 8);
        bits.unsigned(0);
        // chroma_format_idc, 8 bit luma and chroma, no transform bypass, no seq_scaling_matrix.
        bits.unsigned(CHROMA_420);
        bits.unsigned(0);
        bits.unsigned(0);
        bits.flags(&[false, false]);
        // log2_max_frame_num, pic_order_cnt_type 0 with its log2_max_pic_order_cnt_lsb, max_num_ref_frames, no gaps in frame_num.
        bits.unsigned(FRAME_NUMBER_BITS - SMALLEST_COUNT_BITS);
        bits.unsigned(0);
        bits.unsigned(ORDER_COUNT_BITS - SMALLEST_COUNT_BITS);
        bits.unsigned(REFERENCE_FRAMES);
        bits.flag(false);
        // The width and height in macroblocks, frame_mbs_only, direct_8x8_inference.
        bits.unsigned(self.width.div_ceil(MACROBLOCK) - 1);
        bits.unsigned(self.height.div_ceil(MACROBLOCK) - 1);
        bits.flags(&[true, true]);
        // frame_cropping_flag, and the crop of the left, right, top and bottom edges.
        let crop = |length: u32| (length.next_multiple_of(MACROBLOCK) - length) / CROP_UNIT;
        let (right, bottom) = (crop(self.width), crop(self.height));
        bits.flag(right + bottom > 0);
        if right + bottom > 0 {
            for edge in [0, right, 0, bottom] {
                bits.unsigned(edge);
            }
        }
        // vui_parameters_present_flag; no aspect_ratio_info, no overscan_info, a video_signal_type (E.1.1).
        bits.flags(&[true, false, false, true]);
        // video_format, limited range, and a colour_description: primaries, transfer and matrix.
        bits.put(VIDEO_FORMAT, 3);
        bits.flags(&[false, true]);
        for _ in 0..3 {
            bits.put(REC_709, 8);
        }
        // chroma_loc_info_present_flag, and chroma_sample_loc_type of the top and the bottom field.
        bits.flag(self.is_chroma_on_luma);
        if self.is_chroma_on_luma {
            bits.unsigned(CHROMA_ON_TOP_LEFT_LUMA);
            bits.unsigned(CHROMA_ON_TOP_LEFT_LUMA);
        }
        // timing_info_present_flag: num_units_in_tick, time_scale, no fixed_frame_rate.
        bits.flag(self.pace.is_some());
        if let Some((ticks_per_frame, ticks_per_second)) = self.pace {
            bits.put(ticks_per_frame, u32::BITS);
            bits.put(FIELDS_PER_FRAME * ticks_per_second, u32::BITS);
            bits.flag(false);
        }
        // No NAL or VCL HRD parameters, no pic_struct.
        bits.flags(&[false, false, false]);
        // bitstream_restriction_flag: vectors over the edges, no bound on bytes or bits, the vectors' lengths, no reordering, the frames a decoder holds.
        bits.flags(&[true, true]);
        bits.unsigned(0);
        bits.unsigned(0);
        bits.unsigned(VECTOR_LENGTH_BITS);
        bits.unsigned(VECTOR_LENGTH_BITS);
        bits.unsigned(0);
        bits.unsigned(DECODER_FRAMES);
        to_unit(HIGHEST_IMPORTANCE, SEQUENCE_PARAMETERS, &bits.end())
    }

    /// The picture parameter set, without a start code.
    pub fn picture_parameters(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        // pic_parameter_set_id and seq_parameter_set_id 0, CABAC, no bottom field order, one slice group (H.264 7.3.2.2).
        bits.unsigned(0);
        bits.unsigned(0);
        bits.flags(&[true, false]);
        bits.unsigned(0);
        // The default lengths of both reference lists, no weighted prediction.
        bits.unsigned(DEFAULT_REFERENCES - 1);
        bits.unsigned(DEFAULT_REFERENCES - 1);
        bits.flag(false);
        bits.put(0, 2);
        // pic_init_qp and pic_init_qs at 26, no chroma_qp_index_offset.
        bits.signed(0);
        bits.signed(0);
        bits.signed(0);
        // deblocking_filter_control, no constrained_intra_pred, no redundant_pic_cnt, the 8x8 transform, no pic_scaling_matrix, no second chroma offset.
        bits.flags(&[true, false, false, true, false]);
        bits.signed(0);
        to_unit(HIGHEST_IMPORTANCE, PICTURE_PARAMETERS, &bits.end())
    }
}

fn start_code(bytes: &[u8]) -> Option<usize> {
    let mut at = 0;
    while let Some(&[first, second, third]) = bytes.get(at..at + START_CODE.len()) {
        if third > 1 {
            at += 3;
        } else if second != 0 {
            at += 2;
        } else if third == 1 && first == 0 {
            return Some(at);
        } else {
            at += 1;
        }
    }
    None
}

fn units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = stream;
    core::iter::from_fn(move || {
        let body = &rest[start_code(rest)? + START_CODE.len()..];
        let next = start_code(body).unwrap_or(body.len());
        let end = body[..next].iter().rposition(|&byte| byte != 0).map_or(0, |last| last + 1);
        rest = &body[next..];
        Some(&body[..end])
    })
}

pub(crate) fn pack(stream: &[u8], sample: &mut Vec<u8>) -> usize {
    let mut count = 0;
    for unit in units(stream).filter(|unit| !unit.is_empty()) {
        sample.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        sample.extend_from_slice(unit);
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::h264::read::Bits as Reader;
    use alloc::vec;

    const STOCK_SEQUENCE: [u8; 19] = [
        0x67, 0x64, 0x00, 0x29, 0xAC, 0x1B, 0x1A, 0x80, 0x50, 0x05, 0xBA, 0x6A, 0x02, 0x02, 0x02, 0x0F, 0x08, 0x84,
        0x6E,
    ];
    const STOCK_PICTURE: [u8; 4] = [0x68, 0xEA, 0x43, 0xCB];
    const HD: Stream = Stream { width: 1280, height: 720, level: 41, pace: None, is_chroma_on_luma: false };

    fn reader(unit: &[u8], at: usize) -> Reader<'_> {
        let mut reader = Reader::new(&unit[1..]);
        reader.skip(at).unwrap();
        reader
    }

    #[test]
    fn exp_golomb_bits() {
        let mut bits = Bits::default();
        bits.put(0b101, 3);
        bits.unsigned(0);
        bits.unsigned(1);
        bits.unsigned(6);
        bits.signed(1);
        bits.signed(-1);
        bits.signed(4);
        assert_eq!(
            bits.end(),
            [0b1011_0100, 0b0111_0100, 0b1100_0100, 0b0100_0000],
            "101 1 010 00111 010 011 0001000 and the closing 1"
        );
        let mut whole = Bits::default();
        whole.put(0xAB, 8);
        assert_eq!(whole.end(), [0xAB, 0x80], "a full byte gets its closing bit in a new one");
    }

    #[test]
    fn stock_parameter_sets() {
        assert_eq!(HD.sequence_parameters(), STOCK_SEQUENCE);
        assert_eq!(HD.picture_parameters(), STOCK_PICTURE);
    }

    #[test]
    fn pace_and_chroma_fields() {
        let stream = Stream { pace: Some((7999, 480_000)), is_chroma_on_luma: true, ..HD };
        let unit = stream.sequence_parameters();
        assert_eq!(unit[..12], STOCK_SEQUENCE[..12], "the same up to the colour description");
        let mut reader = reader(&unit, 8 * 14 - 1);
        assert_eq!(reader.bit(), Ok(1), "chroma place present");
        assert_eq!((reader.unsigned(), reader.unsigned()), (Ok(2), Ok(2)));
        assert_eq!(reader.bit(), Ok(1), "timing present");
        assert_eq!(
            (reader.bits(32), reader.bits(32), reader.bit()),
            (Ok(7999), Ok(960_000), Ok(0)),
            "60.0075 frames a second, not promised to be steady"
        );
        assert_eq!(
            reader.bits(5),
            Ok(0b00011),
            "no buffer models, no picture structure, restrictions, vectors over the edges"
        );
        let rest: [u32; 6] = core::array::from_fn(|_| reader.unsigned().unwrap());
        assert_eq!(rest, [0, 0, 16, 16, 0, 2]);
        assert_eq!(stream.picture_parameters(), STOCK_PICTURE, "the picture parameters do not change");
    }

    #[test]
    fn escape_after_two_zeros() {
        assert_eq!(to_unit(3, 7, &[0, 0, 1, 0, 0, 0, 0, 3, 0, 0, 4]), [0x67, 0, 0, 3, 1, 0, 0, 3, 0, 0, 3, 3, 0, 0, 4]);
        assert_eq!(to_unit(2, 1, &[5, 0, 0]), [0x41, 5, 0, 0]);
        let paced = Stream { pace: Some((1, 30)), ..HD }.sequence_parameters();
        assert!(
            paced.windows(3).all(|window| !(window[0] == 0 && window[1] == 0 && window[2] < H264_ESCAPE)),
            "{paced:x?}"
        );
        assert!(
            paced.windows(3).any(|window| window == [0, 0, H264_ESCAPE]),
            "a tick count with zero bytes needs escapes"
        );
        let mut reader = reader(&paced, 8 * 14);
        assert_eq!((reader.bit(), reader.bits(32), reader.bits(32)), (Ok(1), Ok(1), Ok(60)));
    }

    #[test]
    fn cut_at_start_codes() {
        let stream = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 0x88, 0, 0, 3, 1, 0, 0];
        let cut: Vec<&[u8]> = units(&stream).collect();
        assert_eq!(
            cut,
            [&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 0x88, 0, 0, 3, 1]],
            "zeros before a start code and at the end belong to no unit"
        );
        assert_eq!(units(&[1, 2, 3]).count(), 0, "no start code, no unit");
        // Bytes of which most are zeros and ones, so that codes and near codes lie at every offset.
        let mixed: Vec<u8> =
            (0..4096u32).map(|at| [0, 0, 1, 0, 2, 0, 0, 0, 3, 1, 0, 7][(at * 7 + at / 5) as usize % 12]).collect();
        assert!(mixed.windows(START_CODE.len()).filter(|window| *window == START_CODE).count() > 100);
        for from in 0..mixed.len() {
            let rest = &mixed[from..];
            let naive = rest.windows(START_CODE.len()).position(|window| window == START_CODE);
            assert_eq!(start_code(rest), naive, "from {from}");
        }
        assert_eq!((start_code(&[0, 0]), start_code(&[]), start_code(&[0, 0, 0, 0, 1])), (None, None, Some(2)));
    }

    #[test]
    fn units_behind_lengths() {
        let mut sample = vec![0xEE];
        assert_eq!(pack(&[0, 0, 0, 1, 0x41, 0x9A, 0, 0, 1, 0x06, 5, 0x80, 0], &mut sample), 2);
        assert_eq!(sample, [0xEE, 0, 0, 0, 2, 0x41, 0x9A, 0, 0, 0, 3, 0x06, 5, 0x80]);
        assert_eq!(pack(&[0, 0, 1, 0, 0, 1], &mut sample), 0, "empty units are left out");
    }
}
