use super::probs::{self, BAND_0_CONTEXTS, CONTEXTS, Probs};
use super::tables::{
    ALLOW_32X32, ALTREF_FRAME, FEATURE_BITS, FEATURE_SIGNED, GOLDEN_FRAME, LAST_FRAME, LITERAL_TO_FILTER, MAX_SEGMENTS,
    SEG_LVL_MAX, SUPERBLOCK_SIDE, SWITCHABLE, TX_MODE_SELECT, TX_MODE_TO_BIGGEST,
};
use crate::base::bits::{BYTE_BITS, BitReader};
use crate::codec::Flaw;
use crate::codec::boolean::BoolDecoder;
use alloc::boxed::Box;

// The frame marker and the key frame's sync code (VP9 6.2, 7.2, 7.2.1).
const FRAME_MARKER: u32 = 2;
const MARKER_BITS: u32 = 2;
const SYNC_CODE: [u32; 3] = [0x49, 0x83, 0x42];
// The reference slots and those a frame names (VP9 3).
pub(super) const NUM_REF_FRAMES: usize = 8;
const REFS_PER_FRAME: usize = 3;
const SLOT_BITS: u32 = 3;
// The fields of the uncompressed header (VP9 6.2 to 6.2.13).
const COLOR_SPACE_BITS: u32 = 3;
const CS_RGB: u32 = 7;
const SIZE_BITS: u32 = 16;
const RESET_BITS: u32 = 2;
const FILTER_BITS: u32 = 2;
const CONTEXT_BITS: u32 = 2;
const LEVEL_BITS: u32 = 6;
const SHARPNESS_BITS: u32 = 3;
const LF_DELTA_BITS: u32 = 6;
const QINDEX_BITS: u32 = 8;
const DELTA_Q_BITS: u32 = 4;
const PROB_BITS: u32 = 8;
const HEADER_SIZE_BITS: u32 = 16;
// The compressed header's tx_mode (VP9 6.3.1).
const TX_MODE_BITS: u32 = 2;
// A probability not sent is 255 (VP9 6.2.12).
const UNSENT_PROB: u8 = 255;
// A tile is at least 4 superblocks wide and at most 64 (VP9 3).
const MIN_TILE_WIDTH_B64: usize = 4;
const MAX_TILE_WIDTH_B64: usize = 64;
// The loop filter deltas a frame independent of the past starts from (VP9 7.2, setup_past_independence).
const DEFAULT_REF_DELTAS: [i8; 4] = [1, 0, -1, -1];
// The reference modes (VP9 7.3.6).
const SINGLE_REFERENCE: u8 = 0;
pub(super) const COMPOUND_REFERENCE: u8 = 1;
pub(super) const REFERENCE_MODE_SELECT: u8 = 2;

// The loop filter's level, sharpness and deltas (VP9 6.2.8).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct LoopFilter {
    pub(super) level: u8,
    pub(super) sharpness: u8,
    pub(super) is_delta_enabled: bool,
    pub(super) ref_deltas: [i8; 4],
    pub(super) mode_deltas: [i8; 2],
}

// Segmentation as the frames so far left it, each feature's value where it is enabled (VP9 6.2.11).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Segmentation {
    pub(super) is_enabled: bool,
    pub(super) update_map: bool,
    pub(super) temporal_update: bool,
    pub(super) tree_probs: [u8; 7],
    pub(super) pred_probs: [u8; 3],
    pub(super) is_absolute: bool,
    features: [[Option<i32>; SEG_LVL_MAX]; MAX_SEGMENTS],
}

impl Segmentation {
    // seg_feature_active (VP9 6.4.9).
    pub(super) fn feature(&self, segment: u8, feature: usize) -> Option<i32> {
        if self.is_enabled { self.features[usize::from(segment)][feature] } else { None }
    }
}

// What the frame headers keep from one frame to the next.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Kept {
    loop_filter: LoopFilter,
    segmentation: Segmentation,
}

// A frame's uncompressed header (VP9 6.2).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Header {
    pub(super) is_key: bool,
    pub(super) is_intra_only: bool,
    pub(super) is_shown: bool,
    pub(super) is_error_resilient: bool,
    pub(super) reset_context: u8,
    pub(super) refresh: u8,
    pub(super) refs: [usize; REFS_PER_FRAME],
    pub(super) sign_bias: [bool; 4],
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) allow_hp: bool,
    pub(super) filter: u8,
    pub(super) refresh_context: bool,
    pub(super) is_parallel: bool,
    pub(super) context: usize,
    pub(super) loop_filter: LoopFilter,
    pub(super) base_q: i32,
    pub(super) delta_q: [i32; 3],
    pub(super) is_lossless: bool,
    pub(super) segmentation: Segmentation,
    pub(super) tile_cols_log2: u32,
    pub(super) tile_rows_log2: u32,
    pub(super) compressed_size: usize,
    pub(super) size: usize,
}

impl Header {
    pub(super) const fn is_intra(&self) -> bool {
        self.is_key || self.is_intra_only
    }
}

// A frame's first bytes: a reference slot to show again, or a frame to decode (VP9 6.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Parsed {
    Existing(usize),
    Frame(Box<Header>),
}

fn take(reader: &mut BitReader<'_>, count: u32) -> Result<u32, Flaw> {
    reader.take(count).ok_or(Flaw::Short)
}

fn flag(reader: &mut BitReader<'_>) -> Result<bool, Flaw> {
    reader.flag().ok_or(Flaw::Short)
}

// s(n): a magnitude, then its sign (VP9 4.9.2).
fn signed(reader: &mut BitReader<'_>, count: u32) -> Result<i32, Flaw> {
    let magnitude = take(reader, count)? as i32;
    Ok(if flag(reader)? { -magnitude } else { magnitude })
}

fn read_size(reader: &mut BitReader<'_>) -> Result<(usize, usize), Flaw> {
    Ok((take(reader, SIZE_BITS)? as usize + 1, take(reader, SIZE_BITS)? as usize + 1))
}

// render_size: what the display wants, which decoding passes over (VP9 7.2.4).
fn skip_render_size(reader: &mut BitReader<'_>) -> Result<(), Flaw> {
    if flag(reader)? {
        read_size(reader)?;
    }
    Ok(())
}

fn read_sync_code(reader: &mut BitReader<'_>) -> Result<(), Flaw> {
    for byte in SYNC_CODE {
        if take(reader, u8::BITS)? != byte {
            return Err(Flaw::Invalid("sync code"));
        }
    }
    Ok(())
}

// color_config of profile 0, whose only sampling is 4:2:0: RGB would be 4:4:4 (VP9 6.2.2, 7.2.2).
fn read_color_config(reader: &mut BitReader<'_>) -> Result<(), Flaw> {
    if take(reader, COLOR_SPACE_BITS)? == CS_RGB {
        return Err(Flaw::Unsupported("RGB, which is 4:4:4"));
    }
    take(reader, 1)?;
    Ok(())
}

// The profile and its refusal by name: profile 0 alone, of 8 bits and 4:2:0 (VP9 6.2, 7.2).
fn read_profile(reader: &mut BitReader<'_>) -> Result<(), Flaw> {
    if take(reader, MARKER_BITS)? != FRAME_MARKER {
        return Err(Flaw::Invalid("frame marker"));
    }
    let low = take(reader, 1)?;
    match take(reader, 1)? << 1 | low {
        0 => Ok(()),
        1 => Err(Flaw::Unsupported("profile 1, of 4:2:2, 4:4:0 or 4:4:4")),
        2 => Err(Flaw::Unsupported("profile 2, of 10 or 12 bits")),
        _ => Err(Flaw::Unsupported("profile 3, of 10 or 12 bits and 4:2:2, 4:4:0 or 4:4:4")),
    }
}

fn read_loop_filter(reader: &mut BitReader<'_>, filter: &mut LoopFilter) -> Result<(), Flaw> {
    filter.level = take(reader, LEVEL_BITS)? as u8;
    filter.sharpness = take(reader, SHARPNESS_BITS)? as u8;
    filter.is_delta_enabled = flag(reader)?;
    if filter.is_delta_enabled && flag(reader)? {
        for delta in filter.ref_deltas.iter_mut().chain(&mut filter.mode_deltas) {
            if flag(reader)? {
                *delta = signed(reader, LF_DELTA_BITS)? as i8;
            }
        }
    }
    Ok(())
}

fn read_prob(reader: &mut BitReader<'_>) -> Result<u8, Flaw> {
    Ok(if flag(reader)? { take(reader, PROB_BITS)? as u8 } else { UNSENT_PROB })
}

fn read_segmentation(reader: &mut BitReader<'_>, segmentation: &mut Segmentation) -> Result<(), Flaw> {
    segmentation.is_enabled = flag(reader)?;
    segmentation.update_map = false;
    segmentation.temporal_update = false;
    if !segmentation.is_enabled {
        return Ok(());
    }
    segmentation.update_map = flag(reader)?;
    if segmentation.update_map {
        for probability in &mut segmentation.tree_probs {
            *probability = read_prob(reader)?;
        }
        segmentation.temporal_update = flag(reader)?;
        for probability in &mut segmentation.pred_probs {
            *probability = if segmentation.temporal_update { read_prob(reader)? } else { UNSENT_PROB };
        }
    }
    if flag(reader)? {
        segmentation.is_absolute = flag(reader)?;
        for segment in &mut segmentation.features {
            for (feature, value) in segment.iter_mut().enumerate() {
                *value = None;
                if flag(reader)? {
                    let magnitude = take(reader, FEATURE_BITS[feature])? as i32;
                    let is_negative = FEATURE_SIGNED[feature] && flag(reader)?;
                    *value = Some(if is_negative { -magnitude } else { magnitude });
                }
            }
        }
    }
    Ok(())
}

// tile_info: the base 2 logarithms of the tile columns and rows (VP9 6.2.13, 6.2.14).
fn read_tiles(reader: &mut BitReader<'_>, width: usize) -> Result<(u32, u32), Flaw> {
    let columns = width.div_ceil(SUPERBLOCK_SIDE);
    let mut least = 0;
    while MAX_TILE_WIDTH_B64 << least < columns {
        least += 1;
    }
    let mut most = 1;
    while columns >> most >= MIN_TILE_WIDTH_B64 {
        most += 1;
    }
    let mut columns_log2 = least;
    while columns_log2 < most - 1 && flag(reader)? {
        columns_log2 += 1;
    }
    let mut rows_log2 = take(reader, 1)?;
    if rows_log2 == 1 {
        rows_log2 += take(reader, 1)?;
    }
    Ok((columns_log2, rows_log2))
}

// setup_past_independence for what the header keeps: no segment features, the loop filter's first deltas (VP9 7.2).
fn forget_past(kept: &mut Kept) {
    kept.segmentation.features = [[None; SEG_LVL_MAX]; MAX_SEGMENTS];
    kept.segmentation.is_absolute = false;
    kept.loop_filter.ref_deltas = DEFAULT_REF_DELTAS;
    kept.loop_filter.mode_deltas = [0; 2];
}

// The uncompressed header (VP9 6.2), by the slots' frame sizes; an inter frame is refused while the decoder resyncs.
pub(super) fn read(
    frame: &[u8],
    kept: &mut Kept,
    (sizes, is_resyncing): (&[Option<(usize, usize)>; NUM_REF_FRAMES], bool),
    check_size: impl Fn(usize, usize) -> Result<(), Flaw>,
) -> Result<Parsed, Flaw> {
    let mut reader = BitReader::new(frame);
    read_profile(&mut reader)?;
    if flag(&mut reader)? {
        return Ok(Parsed::Existing(take(&mut reader, SLOT_BITS)? as usize));
    }
    let mut header = Header { is_key: !flag(&mut reader)?, is_shown: flag(&mut reader)?, ..Header::default() };
    header.is_error_resilient = flag(&mut reader)?;
    if header.is_key {
        read_sync_code(&mut reader)?;
        read_color_config(&mut reader)?;
        (header.width, header.height) = read_size(&mut reader)?;
        skip_render_size(&mut reader)?;
        header.refresh = u8::MAX;
    } else {
        header.is_intra_only = !header.is_shown && flag(&mut reader)?;
        header.reset_context = if header.is_error_resilient { 0 } else { take(&mut reader, RESET_BITS)? as u8 };
        if header.is_intra_only {
            read_sync_code(&mut reader)?;
            header.refresh = take(&mut reader, u8::BITS)? as u8;
            (header.width, header.height) = read_size(&mut reader)?;
            skip_render_size(&mut reader)?;
        } else if is_resyncing {
            return Err(Flaw::Missing("key frame"));
        } else {
            header.refresh = take(&mut reader, u8::BITS)? as u8;
            for index in 0..REFS_PER_FRAME {
                header.refs[index] = take(&mut reader, SLOT_BITS)? as usize;
                header.sign_bias[usize::from(LAST_FRAME as u8) + index] = flag(&mut reader)?;
                if sizes[header.refs[index]].is_none() {
                    return Err(Flaw::Missing("reference frame"));
                }
            }
            let mut found = None;
            for slot in header.refs {
                if flag(&mut reader)? {
                    found = sizes[slot];
                    break;
                }
            }
            (header.width, header.height) = match found {
                Some(size) => size,
                None => read_size(&mut reader)?,
            };
            skip_render_size(&mut reader)?;
            header.allow_hp = flag(&mut reader)?;
            header.filter = if flag(&mut reader)? {
                SWITCHABLE
            } else {
                LITERAL_TO_FILTER[take(&mut reader, FILTER_BITS)? as usize]
            };
        }
    }
    check_size(header.width, header.height)?;
    if header.is_error_resilient {
        header.is_parallel = true;
    } else {
        header.refresh_context = flag(&mut reader)?;
        header.is_parallel = flag(&mut reader)?;
    }
    header.context = take(&mut reader, CONTEXT_BITS)? as usize;
    if header.is_intra() || header.is_error_resilient {
        forget_past(kept);
        header.sign_bias = [false; 4];
    }
    read_loop_filter(&mut reader, &mut kept.loop_filter)?;
    header.loop_filter = kept.loop_filter;
    header.base_q = take(&mut reader, QINDEX_BITS)? as i32;
    for delta in &mut header.delta_q {
        *delta = if flag(&mut reader)? { signed(&mut reader, DELTA_Q_BITS)? } else { 0 };
    }
    header.is_lossless = header.base_q == 0 && header.delta_q == [0; 3];
    read_segmentation(&mut reader, &mut kept.segmentation)?;
    header.segmentation = kept.segmentation;
    (header.tile_cols_log2, header.tile_rows_log2) = read_tiles(&mut reader, header.width)?;
    header.compressed_size = take(&mut reader, HEADER_SIZE_BITS)? as usize;
    if header.compressed_size == 0 {
        return Err(Flaw::Invalid("header size"));
    }
    header.size = reader.position().div_ceil(BYTE_BITS);
    Ok(Parsed::Frame(Box::new(header)))
}

// What the compressed header sets besides probabilities (VP9 6.3).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Compressed {
    pub(super) tx_mode: u8,
    pub(super) reference_mode: u8,
    pub(super) fixed: i8,
    pub(super) variable: [i8; 2],
}

// The compressed header: the transform mode, the reference mode and the probabilities' updates (VP9 6.3).
pub(super) fn read_compressed(decoder: &mut BoolDecoder<'_>, header: &Header, probs: &mut Probs) -> Compressed {
    let mut compressed = Compressed::default();
    if !header.is_lossless {
        compressed.tx_mode = decoder.literal(TX_MODE_BITS) as u8;
        if compressed.tx_mode == ALLOW_32X32 {
            compressed.tx_mode += decoder.literal(1) as u8;
        }
    }
    if compressed.tx_mode == TX_MODE_SELECT {
        probs.tx8.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
        probs.tx16.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
        probs.tx32.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    }
    for size in &mut probs.coef[..=usize::from(TX_MODE_TO_BIGGEST[usize::from(compressed.tx_mode)])] {
        if decoder.literal(1) == 1 {
            for bands in size.iter_mut().flatten() {
                for (band, contexts) in bands.iter_mut().enumerate() {
                    let count = if band == 0 { BAND_0_CONTEXTS } else { CONTEXTS };
                    for context in contexts.iter_mut().take(count) {
                        context.iter_mut().for_each(|probability| probs::update(decoder, probability));
                    }
                }
            }
        }
    }
    probs.skip.iter_mut().for_each(|probability| probs::update(decoder, probability));
    if header.is_intra() {
        return compressed;
    }
    probs.inter_mode.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    if header.filter == SWITCHABLE {
        probs.interp_filter.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    }
    probs.is_inter.iter_mut().for_each(|probability| probs::update(decoder, probability));
    let bias = header.sign_bias;
    let last = usize::from(LAST_FRAME as u8);
    if bias[last + 1] != bias[last] || bias[last + 2] != bias[last] {
        compressed.reference_mode = if decoder.literal(1) == 0 {
            SINGLE_REFERENCE
        } else if decoder.literal(1) == 0 {
            COMPOUND_REFERENCE
        } else {
            REFERENCE_MODE_SELECT
        };
        (compressed.fixed, compressed.variable) = if bias[last] == bias[last + 1] {
            (ALTREF_FRAME, [LAST_FRAME, GOLDEN_FRAME])
        } else if bias[last] == bias[last + 2] {
            (GOLDEN_FRAME, [LAST_FRAME, ALTREF_FRAME])
        } else {
            (LAST_FRAME, [GOLDEN_FRAME, ALTREF_FRAME])
        };
    }
    if compressed.reference_mode == REFERENCE_MODE_SELECT {
        probs.comp_mode.iter_mut().for_each(|probability| probs::update(decoder, probability));
    }
    if compressed.reference_mode != COMPOUND_REFERENCE {
        probs.single_ref.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    }
    if compressed.reference_mode != SINGLE_REFERENCE {
        probs.comp_ref.iter_mut().for_each(|probability| probs::update(decoder, probability));
    }
    probs.y_mode.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    probs.partition.iter_mut().flatten().for_each(|probability| probs::update(decoder, probability));
    probs::update_mv(decoder, &mut probs.mv_joint[0]);
    probs::update_mv(decoder, &mut probs.mv_joint[1]);
    probs::update_mv(decoder, &mut probs.mv_joint[2]);
    for component in &mut probs.mv {
        probs::update_mv(decoder, &mut component.sign);
        component.classes.iter_mut().for_each(|probability| probs::update_mv(decoder, probability));
        probs::update_mv(decoder, &mut component.class0_bit);
        component.bits.iter_mut().for_each(|probability| probs::update_mv(decoder, probability));
    }
    for component in &mut probs.mv {
        component.class0_fr.iter_mut().flatten().for_each(|probability| probs::update_mv(decoder, probability));
        component.fr.iter_mut().for_each(|probability| probs::update_mv(decoder, probability));
    }
    if header.allow_hp {
        for component in &mut probs.mv {
            probs::update_mv(decoder, &mut component.class0_hp);
            probs::update_mv(decoder, &mut component.hp);
        }
    }
    compressed
}
