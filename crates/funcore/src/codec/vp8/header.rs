use super::filter::Kind;
use super::inter::Filter;
use super::tables::{
    BLOCK_TYPES, COEFF_BANDS, COEFF_UPDATE_PROBS, CoeffProbs, DEFAULT_COEFF_PROBS, DEFAULT_MV_CONTEXT, ENTROPY_NODES,
    MV_UPDATE_PROBS, MVP_COUNT, PREV_COEFF_CONTEXTS, UV_MODE_PROB, YMODE_PROB,
};
use crate::base::bytes::le16;
use crate::codec::Flaw;
use crate::codec::boolean::{BoolDecoder, BoolEncoder};
use alloc::vec::Vec;

// The frame tag's three bytes, and a key frame's start code and size after it (RFC 6386, 9.1).
pub(super) const TAG_BYTES: usize = 3;
pub(super) const KEY_BYTES: usize = 10;
const START_CODE: [u8; 3] = [0x9D, 0x01, 0x2A];
const START_CODE_AT: usize = TAG_BYTES;
const SIZE_AT: usize = TAG_BYTES + START_CODE.len();
const SIZE_BITS: u32 = 14;
// The frame tag's fields, with the bit of the versions beyond 3 (RFC 6386, 19.1, and the reference's frame header).
const INTERFRAME_BIT: u32 = 0;
const VERSION_SHIFT: u32 = 1;
const VERSION_MASK: u32 = 3;
const EXPERIMENTAL_BIT: u32 = 3;
const SHOW_BIT: u32 = 4;
const FIRST_SIZE_SHIFT: u32 = 5;
// Version 3 moves chroma by whole pixels alone (RFC 6386, 18.1).
const FULL_PIXEL_VERSION: u8 = 3;
// The sizes of the token partitions after the first, three bytes each (RFC 6386, 9.5).
const PARTITION_SIZE_BYTES: usize = 3;
pub(super) const MOST_PARTITIONS: usize = 8;
// The bits of the header's fields (RFC 6386, 19.2).
const QUANTIZER_BITS: u32 = 7;
const QUANTIZER_DELTA_BITS: u32 = 4;
const SEGMENT_LEVEL_BITS: u32 = 6;
const LEVEL_BITS: u32 = 6;
const SHARPNESS_BITS: u32 = 3;
const LF_DELTA_BITS: u32 = 6;
const PARTITIONS_BITS: u32 = 2;
const COPY_BITS: u32 = 2;
const PROBABILITY_BITS: u32 = 8;
const MV_PROBABILITY_BITS: u32 = 7;
pub(super) const SEGMENTS: usize = 4;
const SEGMENT_TREE_PROBS: usize = 3;
// A segment tree probability not sent is 255 (RFC 6386, 9.3).
const UNSENT_PROBABILITY: u8 = 255;
// The reference frames and the prediction modes a loop filter delta is kept for (RFC 6386, 9.4).
pub(super) const LF_DELTAS: usize = 4;
// What a buffer copy flag names: no buffer, the last frame, or the other of golden and altref (RFC 6386, 9.7).
pub(super) const COPY_NONE: u8 = 0;
pub(super) const COPY_LAST: u8 = 1;
pub(super) const COPY_OTHER: u8 = 2;

// The frame tag (RFC 6386, 9.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Tag {
    pub(super) is_key: bool,
    pub(super) version: u8,
    pub(super) is_shown: bool,
    pub(super) first_size: usize,
}

pub(super) fn read_tag(frame: &[u8]) -> Result<Tag, Flaw> {
    let &[first, second, third, ..] = frame else {
        return Err(Flaw::Short);
    };
    let raw = u32::from(first) | u32::from(second) << 8 | u32::from(third) << 16;
    if raw >> EXPERIMENTAL_BIT & 1 == 1 {
        return Err(Flaw::Unsupported("a version beyond 3"));
    }
    Ok(Tag {
        is_key: raw >> INTERFRAME_BIT & 1 == 0,
        version: (raw >> VERSION_SHIFT & VERSION_MASK) as u8,
        is_shown: raw >> SHOW_BIT & 1 == 1,
        first_size: (raw >> FIRST_SIZE_SHIFT) as usize,
    })
}

// Whether a frame's first bytes tell a key frame: a clear frame type bit and the start code.
#[cfg(test)]
pub(super) fn is_key(frame: &[u8]) -> bool {
    frame.first().is_some_and(|&byte| u32::from(byte) >> INTERFRAME_BIT & 1 == 0)
        && frame.get(START_CODE_AT..SIZE_AT) == Some(&START_CODE[..])
}

// A key frame's width and height in pixels; the two scaling bits of each are for display alone (RFC 6386, 9.1).
pub(super) fn read_size(frame: &[u8]) -> Result<(usize, usize), Flaw> {
    if frame.len() < KEY_BYTES {
        return Err(Flaw::Short);
    }
    if frame[START_CODE_AT..SIZE_AT] != START_CODE {
        return Err(Flaw::Invalid("start code"));
    }
    let size = |at: usize| le16(frame, at).map_or(0, |word| usize::from(word & ((1 << SIZE_BITS) - 1)));
    let (width, height) = (size(SIZE_AT), size(SIZE_AT + 2));
    if width == 0 || height == 0 {
        return Err(Flaw::Invalid("picture size"));
    }
    Ok((width, height))
}

// The probabilities a key frame resets and an interframe may update for good or for itself alone (RFC 6386, 9.11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Probabilities {
    pub(super) coeff: CoeffProbs,
    pub(super) mv: [[u8; MVP_COUNT]; 2],
    pub(super) y_mode: [u8; 4],
    pub(super) uv_mode: [u8; 3],
}

impl Default for Probabilities {
    fn default() -> Probabilities {
        Probabilities { coeff: DEFAULT_COEFF_PROBS, mv: DEFAULT_MV_CONTEXT, y_mode: YMODE_PROB, uv_mode: UV_MODE_PROB }
    }
}

// Segmentation as the frames so far left it (RFC 6386, 9.3).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Segmentation {
    pub(super) is_enabled: bool,
    pub(super) update_map: bool,
    pub(super) is_absolute: bool,
    pub(super) quantizer: [i32; SEGMENTS],
    pub(super) level: [i32; SEGMENTS],
    pub(super) tree_probs: [u8; SEGMENT_TREE_PROBS],
}

// The loop filter's deltas by reference frame and by mode, as the frames so far left them (RFC 6386, 9.4).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Deltas {
    pub(super) is_enabled: bool,
    pub(super) reference: [i32; LF_DELTAS],
    pub(super) mode: [i32; LF_DELTAS],
}

// What the frame headers keep from one frame to the next.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct State {
    pub(super) probabilities: Probabilities,
    pub(super) saved: Option<Probabilities>,
    pub(super) segmentation: Segmentation,
    pub(super) deltas: Deltas,
}

// The dequantisation indices: the base and the deltas of the five other factors (RFC 6386, 9.6).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Quantizers {
    pub(super) index: i32,
    pub(super) y_dc: i32,
    pub(super) y2_dc: i32,
    pub(super) y2_ac: i32,
    pub(super) uv_dc: i32,
    pub(super) uv_ac: i32,
}

// A frame's header, past the tag (RFC 6386, 9 and 19.2).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Header {
    pub(super) is_key: bool,
    pub(super) filter: Filter,
    pub(super) is_full_pixel: bool,
    pub(super) kind: Kind,
    pub(super) level: i32,
    pub(super) sharpness: u8,
    pub(super) partitions: usize,
    pub(super) quantizers: Quantizers,
    pub(super) refresh_golden: bool,
    pub(super) refresh_altref: bool,
    pub(super) copy_to_golden: u8,
    pub(super) copy_to_altref: u8,
    pub(super) sign_bias_golden: bool,
    pub(super) sign_bias_altref: bool,
    pub(super) refresh_entropy: bool,
    pub(super) refresh_last: bool,
    pub(super) skip_prob: Option<u8>,
    pub(super) prob_intra: u8,
    pub(super) prob_last: u8,
    pub(super) prob_golden: u8,
}

fn read_segmentation(decoder: &mut BoolDecoder<'_>, segmentation: &mut Segmentation) {
    segmentation.is_enabled = decoder.flag();
    if !segmentation.is_enabled {
        segmentation.update_map = false;
        return;
    }
    segmentation.update_map = decoder.flag();
    let update_data = decoder.flag();
    if update_data {
        segmentation.is_absolute = decoder.flag();
        for quantizer in &mut segmentation.quantizer {
            *quantizer = decoder.delta(QUANTIZER_BITS);
        }
        for level in &mut segmentation.level {
            *level = decoder.delta(SEGMENT_LEVEL_BITS);
        }
    }
    if segmentation.update_map {
        for probability in &mut segmentation.tree_probs {
            *probability = if decoder.flag() { decoder.literal(PROBABILITY_BITS) as u8 } else { UNSENT_PROBABILITY };
        }
    }
}

// A delta not sent keeps its value, as the reference library and the RFC's "updated" have it (RFC 6386, 9.4).
fn read_deltas(decoder: &mut BoolDecoder<'_>, deltas: &mut Deltas) {
    deltas.is_enabled = decoder.flag();
    if deltas.is_enabled && decoder.flag() {
        for delta in deltas.reference.iter_mut().chain(&mut deltas.mode) {
            if decoder.flag() {
                *delta = decoder.signed(LF_DELTA_BITS);
            }
        }
    }
}

fn read_quantizers(decoder: &mut BoolDecoder<'_>) -> Quantizers {
    let index = decoder.literal(QUANTIZER_BITS) as i32;
    let mut delta = || decoder.delta(QUANTIZER_DELTA_BITS);
    Quantizers { index, y_dc: delta(), y2_dc: delta(), y2_ac: delta(), uv_dc: delta(), uv_ac: delta() }
}

fn read_copy(decoder: &mut BoolDecoder<'_>) -> Result<u8, Flaw> {
    match decoder.literal(COPY_BITS) as u8 {
        copy @ (COPY_NONE | COPY_LAST | COPY_OTHER) => Ok(copy),
        _ => Err(Flaw::Invalid("buffer copy")),
    }
}

fn read_coefficient_updates(decoder: &mut BoolDecoder<'_>, coeff: &mut CoeffProbs) {
    for kind in 0..BLOCK_TYPES {
        for band in 0..COEFF_BANDS {
            for context in 0..PREV_COEFF_CONTEXTS {
                for node in 0..ENTROPY_NODES {
                    if decoder.read(COEFF_UPDATE_PROBS[kind][band][context][node]) {
                        coeff[kind][band][context][node] = decoder.literal(PROBABILITY_BITS) as u8;
                    }
                }
            }
        }
    }
}

// A vector's probability as its seven bits tell it: twice their value, and 1 for nought.
pub(super) const fn mv_probability(half: u8) -> u8 {
    if half == 0 { 1 } else { half << 1 }
}

// The frame header, which updates what frames keep; probabilities kept for this frame alone are saved first (19.2).
pub(super) fn read(decoder: &mut BoolDecoder<'_>, tag: Tag, state: &mut State) -> Result<Header, Flaw> {
    let is_key = tag.is_key;
    if is_key {
        if decoder.flag() {
            return Err(Flaw::Unsupported("the reserved colour space"));
        }
        // The clamping type only promises that clamping changes nothing; pixels are clamped anyway (RFC 6386, 9.2).
        decoder.flag();
        state.segmentation = Segmentation::default();
        state.deltas = Deltas::default();
    }
    read_segmentation(decoder, &mut state.segmentation);
    let kind = if decoder.flag() { Kind::Simple } else { Kind::Normal };
    let level = decoder.literal(LEVEL_BITS) as i32;
    let sharpness = decoder.literal(SHARPNESS_BITS) as u8;
    read_deltas(decoder, &mut state.deltas);
    let partitions = 1 << decoder.literal(PARTITIONS_BITS);
    let quantizers = read_quantizers(decoder);
    let (mut refresh_golden, mut refresh_altref, mut copy_to_golden, mut copy_to_altref) = (true, true, 0, 0);
    let (mut sign_bias_golden, mut sign_bias_altref) = (false, false);
    if !is_key {
        refresh_golden = decoder.flag();
        refresh_altref = decoder.flag();
        if !refresh_golden {
            copy_to_golden = read_copy(decoder)?;
        }
        if !refresh_altref {
            copy_to_altref = read_copy(decoder)?;
        }
        sign_bias_golden = decoder.flag();
        sign_bias_altref = decoder.flag();
    }
    let refresh_entropy = decoder.flag();
    let refresh_last = is_key || decoder.flag();
    if is_key {
        state.probabilities = Probabilities::default();
    }
    state.saved = (!refresh_entropy).then_some(state.probabilities);
    read_coefficient_updates(decoder, &mut state.probabilities.coeff);
    let skip_prob = decoder.flag().then(|| decoder.literal(PROBABILITY_BITS) as u8);
    let (mut prob_intra, mut prob_last, mut prob_golden) = (0, 0, 0);
    if !is_key {
        prob_intra = decoder.literal(PROBABILITY_BITS) as u8;
        prob_last = decoder.literal(PROBABILITY_BITS) as u8;
        prob_golden = decoder.literal(PROBABILITY_BITS) as u8;
        let probabilities = &mut state.probabilities;
        if decoder.flag() {
            probabilities
                .y_mode
                .iter_mut()
                .for_each(|probability| *probability = decoder.literal(PROBABILITY_BITS) as u8);
        }
        if decoder.flag() {
            probabilities
                .uv_mode
                .iter_mut()
                .for_each(|probability| *probability = decoder.literal(PROBABILITY_BITS) as u8);
        }
        for (component, updates) in probabilities.mv.iter_mut().zip(&MV_UPDATE_PROBS) {
            for (probability, &update) in component.iter_mut().zip(updates) {
                if decoder.read(update) {
                    *probability = mv_probability(decoder.literal(MV_PROBABILITY_BITS) as u8);
                }
            }
        }
    }
    let filter = if tag.version == 0 { Filter::Sixtap } else { Filter::Bilinear };
    Ok(Header {
        is_key,
        filter,
        is_full_pixel: tag.version == FULL_PIXEL_VERSION,
        kind,
        level,
        sharpness,
        partitions,
        quantizers,
        refresh_golden,
        refresh_altref,
        copy_to_golden,
        copy_to_altref,
        sign_bias_golden,
        sign_bias_altref,
        refresh_entropy,
        refresh_last,
        skip_prob,
        prob_intra,
        prob_last,
        prob_golden,
    })
}

// The frame tag and, for a key frame, the start code and the size, as read_tag and read_size read them.
pub(super) fn write_tag(tag: Tag, (width, height): (usize, usize), out: &mut Vec<u8>) {
    let raw = u32::from(!tag.is_key) << INTERFRAME_BIT
        | u32::from(tag.version) << VERSION_SHIFT
        | u32::from(tag.is_shown) << SHOW_BIT
        | (tag.first_size as u32) << FIRST_SIZE_SHIFT;
    out.extend_from_slice(&raw.to_le_bytes()[..TAG_BYTES]);
    if tag.is_key {
        out.extend_from_slice(&START_CODE);
        for size in [width, height] {
            out.extend_from_slice(&(size as u16).to_le_bytes());
        }
    }
}

fn write_deltas(encoder: &mut BoolEncoder, kept: &mut Deltas, wanted: &Deltas) {
    encoder.flag(wanted.is_enabled);
    kept.is_enabled = wanted.is_enabled;
    if !wanted.is_enabled {
        return;
    }
    let is_update = kept.reference != wanted.reference || kept.mode != wanted.mode;
    encoder.flag(is_update);
    if is_update {
        let pairs = kept.reference.iter_mut().chain(&mut kept.mode).zip(wanted.reference.iter().chain(&wanted.mode));
        for (delta, &value) in pairs {
            encoder.flag(*delta != value);
            if *delta != value {
                encoder.signed(value, LF_DELTA_BITS);
                *delta = value;
            }
        }
    }
}

fn write_quantizers(encoder: &mut BoolEncoder, quantizers: &Quantizers) {
    encoder.literal(quantizers.index as u32, QUANTIZER_BITS);
    for delta in [quantizers.y_dc, quantizers.y2_dc, quantizers.y2_ac, quantizers.uv_dc, quantizers.uv_ac] {
        encoder.delta(delta, QUANTIZER_DELTA_BITS);
    }
}

fn write_coefficient_updates(encoder: &mut BoolEncoder, kept: &mut CoeffProbs, wanted: &CoeffProbs) {
    for kind in 0..BLOCK_TYPES {
        for band in 0..COEFF_BANDS {
            for context in 0..PREV_COEFF_CONTEXTS {
                for node in 0..ENTROPY_NODES {
                    let (probability, value) =
                        (&mut kept[kind][band][context][node], wanted[kind][band][context][node]);
                    encoder.write(COEFF_UPDATE_PROBS[kind][band][context][node], *probability != value);
                    if *probability != value {
                        encoder.literal(u32::from(value), PROBABILITY_BITS);
                        *probability = value;
                    }
                }
            }
        }
    }
}

// Writes the frame header `read` reads, with the probabilities the frame is coded with and the filter's deltas; the state changes as the decoder's will, and segmentation stays off.
pub(super) fn write(
    encoder: &mut BoolEncoder,
    header: &Header,
    state: &mut State,
    probabilities: &Probabilities,
    deltas: &Deltas,
) {
    if header.is_key {
        // The one colour space, and pixels clamped (RFC 6386, 9.2).
        encoder.flag(false);
        encoder.flag(false);
        state.segmentation = Segmentation::default();
        state.deltas = Deltas::default();
    }
    encoder.flag(false);
    state.segmentation.is_enabled = false;
    state.segmentation.update_map = false;
    encoder.flag(header.kind == Kind::Simple);
    encoder.literal(header.level as u32, LEVEL_BITS);
    encoder.literal(u32::from(header.sharpness), SHARPNESS_BITS);
    write_deltas(encoder, &mut state.deltas, deltas);
    encoder.literal(header.partitions.trailing_zeros(), PARTITIONS_BITS);
    write_quantizers(encoder, &header.quantizers);
    if !header.is_key {
        encoder.flag(header.refresh_golden);
        encoder.flag(header.refresh_altref);
        if !header.refresh_golden {
            encoder.literal(u32::from(header.copy_to_golden), COPY_BITS);
        }
        if !header.refresh_altref {
            encoder.literal(u32::from(header.copy_to_altref), COPY_BITS);
        }
        encoder.flag(header.sign_bias_golden);
        encoder.flag(header.sign_bias_altref);
    }
    encoder.flag(header.refresh_entropy);
    if !header.is_key {
        encoder.flag(header.refresh_last);
    }
    if header.is_key {
        state.probabilities = Probabilities::default();
    }
    state.saved = (!header.refresh_entropy).then_some(state.probabilities);
    write_coefficient_updates(encoder, &mut state.probabilities.coeff, &probabilities.coeff);
    encoder.flag(header.skip_prob.is_some());
    if let Some(probability) = header.skip_prob {
        encoder.literal(u32::from(probability), PROBABILITY_BITS);
    }
    if header.is_key {
        return;
    }
    for probability in [header.prob_intra, header.prob_last, header.prob_golden] {
        encoder.literal(u32::from(probability), PROBABILITY_BITS);
    }
    let kept = &mut state.probabilities;
    encoder.flag(kept.y_mode != probabilities.y_mode);
    if kept.y_mode != probabilities.y_mode {
        probabilities.y_mode.iter().for_each(|&value| encoder.literal(u32::from(value), PROBABILITY_BITS));
        kept.y_mode = probabilities.y_mode;
    }
    encoder.flag(kept.uv_mode != probabilities.uv_mode);
    if kept.uv_mode != probabilities.uv_mode {
        probabilities.uv_mode.iter().for_each(|&value| encoder.literal(u32::from(value), PROBABILITY_BITS));
        kept.uv_mode = probabilities.uv_mode;
    }
    for ((component, wanted), updates) in kept.mv.iter_mut().zip(&probabilities.mv).zip(&MV_UPDATE_PROBS) {
        for ((probability, &value), &update) in component.iter_mut().zip(wanted).zip(updates) {
            encoder.write(update, *probability != value);
            if *probability != value {
                let half = value >> 1;
                encoder.literal(u32::from(half), MV_PROBABILITY_BITS);
                *probability = mv_probability(half);
            }
        }
    }
}

// Writes the token partitions as `split_partitions` reads them: the size of each but the last, then their bytes.
pub(super) fn write_partitions(partitions: &[Vec<u8>], out: &mut Vec<u8>) {
    for partition in partitions.split_last().map_or(&[][..], |(_, sized)| sized) {
        out.extend_from_slice(&(partition.len() as u32).to_le_bytes()[..PARTITION_SIZE_BYTES]);
    }
    partitions.iter().for_each(|partition| out.extend_from_slice(partition));
}

// The token partitions behind the first: their sizes, three bytes each but the last's, and their bytes (RFC 6386, 9.5).
pub(super) fn split_partitions(data: &[u8], count: usize) -> Result<[&[u8]; MOST_PARTITIONS], Flaw> {
    let count = count.clamp(1, MOST_PARTITIONS);
    let (sizes, mut rest) = data.split_at_checked(PARTITION_SIZE_BYTES * (count - 1)).ok_or(Flaw::Short)?;
    let mut partitions: [&[u8]; MOST_PARTITIONS] = [&[]; MOST_PARTITIONS];
    for (index, partition) in partitions.iter_mut().take(count).enumerate() {
        let size = match sizes.get(index * PARTITION_SIZE_BYTES..(index + 1) * PARTITION_SIZE_BYTES) {
            Some(&[low, middle, high]) => usize::from(low) | usize::from(middle) << 8 | usize::from(high) << 16,
            _ => rest.len(),
        };
        let (bytes, after) = rest.split_at_checked(size).ok_or(Flaw::Short)?;
        *partition = bytes;
        rest = after;
    }
    Ok(partitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_fields() {
        // A key frame of version 0, shown, whose first partition is 0x123 bytes: 0x123 << 5 | 1 << 4.
        let raw = 0x123u32 << 5 | 1 << 4;
        let tag = read_tag(&raw.to_le_bytes()[..3]).unwrap();
        assert_eq!(tag, Tag { is_key: true, version: 0, is_shown: true, first_size: 0x123 }, "a key frame shown");
        let inter = read_tag(&(1u32 | 2 << 1).to_le_bytes()[..3]).unwrap();
        assert_eq!(
            (inter.is_key, inter.version, inter.is_shown, inter.first_size),
            (false, 2, false, 0),
            "an interframe of version 2 not shown"
        );
        assert_eq!(read_tag(&[0x08, 0, 0]), Err(Flaw::Unsupported("a version beyond 3")), "versions 4 to 7");
        assert_eq!(read_tag(&[0, 0]), Err(Flaw::Short), "a tag of two bytes");
    }

    #[test]
    fn key_frame_size() {
        let frame = [0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A, 0xB0, 0x40, 0x90, 0xC0];
        assert_eq!(read_size(&frame), Ok((176, 144)), "the two scaling bits above each fourteen are passed over");
        assert!(is_key(&frame), "a clear first bit and the start code");
        let mut wrong = frame;
        wrong[4] = 0x02;
        assert_eq!(read_size(&wrong), Err(Flaw::Invalid("start code")), "a start code with a byte changed");
        assert!(!is_key(&wrong), "is no key frame");
        assert_eq!(read_size(&frame[..9]), Err(Flaw::Short), "a height cut short");
        let empty = [0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A, 0x00, 0xC0, 0x90, 0x00];
        assert_eq!(read_size(&empty), Err(Flaw::Invalid("picture size")), "no pixels across");
        assert!(!is_key(&[0x11, 0x02, 0x00, 0x9D, 0x01, 0x2A]), "an interframe");
        assert!(!is_key(&[0x10, 0x02]), "too short to tell");
    }

    #[test]
    fn partition_sizes() {
        let data = [2, 0, 0, 1, 0, 0, 0xA, 0xB, 0xC, 0xD, 0xE];
        let partitions = split_partitions(&data, 3).unwrap();
        assert_eq!(&partitions[..3], [&[0xA, 0xB][..], &[0xC], &[0xD, 0xE]], "the last takes what is left");
        assert!(partitions[3..].iter().all(|partition| partition.is_empty()), "the partitions not sent are empty");
        assert_eq!(split_partitions(&data[..4], 3), Err(Flaw::Short), "sizes cut short");
        assert_eq!(split_partitions(&[9, 0, 0, 1, 2], 2), Err(Flaw::Short), "a size past the frame");
        assert_eq!(split_partitions(&[7, 7], 1).unwrap()[0], [7, 7], "one partition has no size of its own");
        let mut written = Vec::new();
        write_partitions(&[alloc::vec![0xA, 0xB], alloc::vec![0xC], alloc::vec![0xD, 0xE]], &mut written);
        assert_eq!(written, data, "three partitions written as they were read");
        written.clear();
        write_partitions(&[alloc::vec![7, 7]], &mut written);
        assert_eq!(written, [7, 7], "one partition written without a size");
    }

    #[test]
    fn written_headers_read_back() {
        let deltas = Deltas { is_enabled: true, reference: [2, 0, -2, -2], mode: [4, -2, 2, 4] };
        let key = Header {
            is_key: true,
            level: 33,
            sharpness: 2,
            partitions: 4,
            quantizers: Quantizers { index: 77, y2_dc: -3, uv_ac: 5, ..Quantizers::default() },
            refresh_golden: true,
            refresh_altref: true,
            refresh_last: true,
            skip_prob: Some(201),
            ..Header::default()
        };
        let inter = Header {
            is_key: false,
            kind: Kind::Simple,
            level: 0,
            partitions: 8,
            quantizers: Quantizers { index: 3, ..Quantizers::default() },
            refresh_golden: false,
            copy_to_golden: COPY_LAST,
            refresh_altref: false,
            copy_to_altref: COPY_OTHER,
            sign_bias_golden: true,
            refresh_entropy: true,
            refresh_last: false,
            skip_prob: None,
            prob_intra: 17,
            prob_last: 200,
            prob_golden: 99,
            ..Header::default()
        };
        let mut wanted = Probabilities::default();
        wanted.coeff[1][2][0][3] = 7;
        wanted.coeff[3][7][2][10] = 250;
        wanted.y_mode = [1, 2, 3, 4];
        wanted.mv[1][5] = 1;
        wanted.mv[0][18] = 64;
        let (mut written, mut read) = (State::default(), State::default());
        for header in [key, inter] {
            let mut encoder = BoolEncoder::new();
            write(&mut encoder, &header, &mut written, &wanted, &deltas);
            let bytes = encoder.finish();
            let tag = Tag { is_key: header.is_key, version: 0, is_shown: true, first_size: bytes.len() };
            let got = super::read(&mut BoolDecoder::new(&bytes), tag, &mut read).unwrap();
            assert_eq!(
                got,
                header,
                "every field of the {} frame's header",
                if header.is_key { "key" } else { "inter" }
            );
            assert_eq!(read, written, "the decoder's state follows the encoder's");
        }
        assert_eq!(read.deltas, deltas, "the deltas sent on the key frame and kept");
        let kept = &read.probabilities;
        assert_eq!((kept.mv[1][5], kept.mv[0][18], kept.y_mode), (1, 64, [1, 2, 3, 4]), "the updates kept");
        let mut tagged = Vec::new();
        let tag = Tag { is_key: true, version: 0, is_shown: true, first_size: 0x1234 };
        write_tag(tag, (176, 144), &mut tagged);
        assert_eq!((read_tag(&tagged), read_size(&tagged)), (Ok(tag), Ok((176, 144))), "the tag and size read back");
    }

    #[test]
    fn deltas_kept_unless_sent() {
        let mut encoder = BoolEncoder::new();
        // Enabled, updated: the first reference delta sent as -5, the others and the modes not sent.
        encoder.literal(1, 1);
        encoder.literal(1, 1);
        encoder.literal(1, 1);
        encoder.literal(5, 6);
        encoder.literal(1, 1);
        for _ in 1..8 {
            encoder.literal(0, 1);
        }
        let bytes = encoder.finish();
        let mut deltas = Deltas { is_enabled: false, reference: [0, 2, 2, 2], mode: [4, -2, 2, 4] };
        read_deltas(&mut BoolDecoder::new(&bytes), &mut deltas);
        let expected = Deltas { is_enabled: true, reference: [-5, 2, 2, 2], mode: [4, -2, 2, 4] };
        assert_eq!(deltas, expected, "the delta sent replaced, the rest kept");
    }

    #[test]
    fn segmentation_values() {
        let mut encoder = BoolEncoder::new();
        // Enabled, a map and data: absolute values, a quantizer of 100 for segment 2, a level of -9 for segment 0.
        encoder.literal(0b111, 3);
        encoder.literal(1, 1);
        for segment in 0..4 {
            encoder.literal(u32::from(segment == 2), 1);
            if segment == 2 {
                encoder.literal(100, 7);
                encoder.literal(0, 1);
            }
        }
        for segment in 0..4 {
            encoder.literal(u32::from(segment == 0), 1);
            if segment == 0 {
                encoder.literal(9, 6);
                encoder.literal(1, 1);
            }
        }
        // The first tree probability is sent as 10, the others are not.
        encoder.literal(1, 1);
        encoder.literal(10, 8);
        encoder.literal(0, 2);
        let bytes = encoder.finish();
        let mut segmentation = Segmentation { quantizer: [1, 2, 3, 4], ..Segmentation::default() };
        read_segmentation(&mut BoolDecoder::new(&bytes), &mut segmentation);
        assert_eq!(
            segmentation,
            Segmentation {
                is_enabled: true,
                update_map: true,
                is_absolute: true,
                quantizer: [0, 0, 100, 0],
                level: [-9, 0, 0, 0],
                tree_probs: [10, 255, 255],
            },
            "values not sent in an update are nought"
        );
    }
}
