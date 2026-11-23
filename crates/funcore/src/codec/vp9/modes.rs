use super::header::{COMPOUND_REFERENCE, Compressed, Header, REFERENCE_MODE_SELECT};
use super::probs::{Counts, MvProbs, Probs};
use super::tables::{
    ALTREF_FRAME, B_HEIGHT_LOG2, B_WIDTH_LOG2, BLOCK_8X8, COUNTER_TO_CONTEXT, DC_PRED, GOLDEN_FRAME,
    IDX_N_COLUMN_TO_SUBBLOCK, INTER_MODE_TREE, INTERP_FILTER_TREE, INTRA_FRAME, INTRA_MODE_TREE, KF_UV_MODE_PROBS,
    KF_Y_MODE_PROBS, LAST_FRAME, MAX_TX_SIZE, MI_SIZE, MODE_2_COUNTER, MV_CLASS_TREE, MV_FR_TREE, MV_JOINT_TREE,
    MV_REF_BLOCKS, NEARESTMV, NEARMV, NEWMV, NONE, NUM_8X8_HIGH, NUM_8X8_WIDE, SEG_LVL_REF_FRAME, SEG_LVL_SKIP,
    SEGMENT_TREE, SIZE_GROUP, SWITCHABLE, TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_MODE_SELECT, TX_MODE_TO_BIGGEST,
    UV_SIZE, ZEROMV,
};
use crate::codec::Flaw;
use crate::codec::boolean::BoolDecoder;
use core::ops::Range;

// The context of an interpolation filter a neighbour does not give: an intra block's, or no block (VP9 9.3.2).
const SWITCHABLE_FILTERS: u8 = 3;
// The joints whose row or column of a vector difference is coded (VP9 7.4.13).
const MV_JOINT_HZVNZ: u8 = 2;
const MV_JOINT_HNZVZ: u8 = 1;
const MV_JOINT_HNZVNZ: u8 = 3;
// A vector's classes, the first of two integer values, and the size of each class above it (VP9 3, 6.4.20).
const CLASS0_SIZE: i32 = 2;
const MV_CLASS_0: u8 = 0;
// A vector at least this many pixels long is coded without its eighths (VP9 3, 6.4.20).
const COMPANDED_MVREF_THRESH: i32 = 8;
// How far past a block's edges a candidate vector may point, in eighths of a pixel (VP9 3, 6.4.17).
const MV_BORDER: i32 = 128;
// A decoded vector's components lie above -(1 << 14) and below (1 << 14) less 1 (VP9 7.4.13).
const MV_LIMIT: i32 = 1 << 14;
// Eighths of a pixel across an 8x8 block, the unit of a block's distance to the frame's edges (VP9 6.4.17).
const MI_EIGHTHS: i32 = 64;
// Bits below the integer part of a vector component: three of eighths (VP9 6.4.20).
const FRACTION_BITS: i32 = 3;

// A motion vector in eighths of a pixel, row first (VP9 6.4.19).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Mv {
    pub(super) row: i16,
    pub(super) col: i16,
}

// What the next frame takes of a block's motion: its references and its vectors (VP9 6.1.2, PrevMvs).
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Motion {
    pub(super) refs: [i8; 2],
    pub(super) mvs: [Mv; 2],
}

// A block's mode info, kept in each 8x8 place it covers; a block of 8x8 or more has its four subblocks alike (VP9 6.4).
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Info {
    pub(super) size: u8,
    pub(super) segment: u8,
    pub(super) is_seg_predicted: bool,
    pub(super) is_skip: bool,
    pub(super) tx_size: u8,
    pub(super) is_inter: bool,
    pub(super) mode: u8,
    pub(super) sub_modes: [u8; 4],
    pub(super) uv_mode: u8,
    pub(super) refs: [i8; 2],
    pub(super) filter: u8,
    pub(super) mvs: [[Mv; 2]; 4],
}

impl Info {
    pub(super) const fn is_compound(&self) -> bool {
        self.refs[1] > INTRA_FRAME
    }

    // The transform size of a plane's blocks: 4x4 for chroma under 8x8, else no larger than its block (VP9 6.4.22).
    pub(super) fn plane_tx_size(&self, plane: usize) -> u8 {
        if plane == 0 {
            self.tx_size
        } else if self.size < BLOCK_8X8 {
            TX_4X4
        } else {
            self.tx_size.min(MAX_TX_SIZE[usize::from(UV_SIZE[usize::from(self.size)])])
        }
    }
}

// What reading a block's mode info takes of its frame and tile.
pub(super) struct Scope<'a> {
    pub(super) header: &'a Header,
    pub(super) compressed: &'a Compressed,
    pub(super) probs: &'a Probs,
    pub(super) previous: Option<&'a [Motion]>,
    pub(super) previous_segments: &'a [u8],
    pub(super) mi_rows: usize,
    pub(super) mi_cols: usize,
    pub(super) tile: (usize, usize),
}

// A block's place in 8x8 units and its size.
#[derive(Clone, Copy)]
pub(super) struct Place {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) size: u8,
}

impl Place {
    // The block's 8x8 places inside a frame of that many rows and columns of them, a row at a time, as places of the frame's grid.
    pub(super) fn rows(self, mi_rows: usize, mi_cols: usize) -> impl Iterator<Item = Range<usize>> {
        let size = usize::from(self.size);
        let across = usize::from(NUM_8X8_WIDE[size]).min(mi_cols - self.col);
        let down = usize::from(NUM_8X8_HIGH[size]).min(mi_rows - self.row);
        (0..down).map(move |line| {
            let start = (self.row + line) * mi_cols + self.col;
            start..start + across
        })
    }

    // The block's first sample in a plane, across and down: chroma is of half the size.
    pub(super) const fn origin(self, plane: usize) -> (usize, usize) {
        let sub = (plane > 0) as usize;
        ((self.col * MI_SIZE) >> sub, (self.row * MI_SIZE) >> sub)
    }
}

// Distances of a block to the frame's edges in eighths of a pixel, negative past them (VP9 6.4.17).
#[derive(Clone, Copy)]
pub(super) struct Edges {
    pub(super) left: i32,
    pub(super) right: i32,
    pub(super) top: i32,
    pub(super) bottom: i32,
}

impl Edges {
    pub(super) fn of(place: Place, mi_rows: usize, mi_cols: usize) -> Edges {
        let (width, height) = (NUM_8X8_WIDE[usize::from(place.size)], NUM_8X8_HIGH[usize::from(place.size)]);
        Edges {
            left: -(place.col as i32) * MI_EIGHTHS,
            right: (mi_cols as i32 - i32::from(width) - place.col as i32) * MI_EIGHTHS,
            top: -(place.row as i32) * MI_EIGHTHS,
            bottom: (mi_rows as i32 - i32::from(height) - place.row as i32) * MI_EIGHTHS,
        }
    }
}

// clamp as libvpx has it, the low bound tested first: where the bounds cross, a value under the low one takes it and any other the high one.
pub(super) const fn clamp(value: i32, low: i32, high: i32) -> i32 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

// The block of the place at the offset, where it lies in the frame's rows and the tile's columns (VP9 6.4.18).
fn neighbor<'g>(scope: &Scope<'_>, grid: &'g [Info], place: Place, (row, col): (i8, i8)) -> Option<&'g Info> {
    let row = place.row.checked_add_signed(isize::from(row)).filter(|&row| row < scope.mi_rows)?;
    let col =
        place.col.checked_add_signed(isize::from(col)).filter(|&col| col >= scope.tile.0 && col < scope.tile.1)?;
    grid.get(row * scope.mi_cols + col)
}

// Reads a block's mode info; the segment map of the frame takes its segment (VP9 6.4.1 to 6.4.23).
pub(super) fn read(
    scope: &Scope<'_>,
    grid: &[Info],
    segments: &mut [u8],
    decoder: &mut BoolDecoder<'_>,
    counts: &mut Counts,
    place: Place,
) -> Result<Info, Flaw> {
    let (above, left) = (neighbor(scope, grid, place, (-1, 0)), neighbor(scope, grid, place, (0, -1)));
    let mut reader = Reader { scope, grid, decoder, counts, place, above, left };
    let mut info = Info { size: place.size, ..Info::default() };
    if scope.header.is_intra() {
        info.segment = reader.intra_segment(segments);
        info.is_skip = reader.skip(info.segment);
        info.tx_size = reader.tx_size(true);
        reader.intra_frame_modes(&mut info);
    } else {
        reader.inter_segment(segments, &mut info);
        info.is_skip = reader.skip(info.segment);
        info.is_inter = reader.is_inter(info.segment);
        info.tx_size = reader.tx_size(!info.is_skip || !info.is_inter);
        if info.is_inter {
            reader.inter_modes(&mut info)?;
        } else {
            reader.intra_modes(&mut info);
        }
    }
    Ok(info)
}

struct Reader<'s, 'g, 'd, 'b> {
    scope: &'s Scope<'s>,
    grid: &'g [Info],
    decoder: &'d mut BoolDecoder<'b>,
    counts: &'d mut Counts,
    place: Place,
    above: Option<&'g Info>,
    left: Option<&'g Info>,
}

impl Reader<'_, '_, '_, '_> {
    // The block's places in the segment maps.
    fn cells(&self) -> impl Iterator<Item = usize> {
        self.place.rows(self.scope.mi_rows, self.scope.mi_cols).flatten()
    }

    fn fill_segments(&self, segments: &mut [u8], segment: Option<u8>) {
        for at in self.cells() {
            let value = segment.unwrap_or_else(|| self.scope.previous_segments.get(at).copied().unwrap_or(0));
            if let Some(slot) = segments.get_mut(at) {
                *slot = value;
            }
        }
    }

    // intra_segment_id: with no new map the block keeps the last one's ids, and its own is nought (VP9 6.4.7).
    fn intra_segment(&mut self, segments: &mut [u8]) -> u8 {
        let segmentation = &self.scope.header.segmentation;
        if !segmentation.is_enabled {
            return 0;
        }
        if !segmentation.update_map {
            self.fill_segments(segments, None);
            return 0;
        }
        let segment = self.decoder.tree(&SEGMENT_TREE, &segmentation.tree_probs);
        self.fill_segments(segments, Some(segment));
        segment
    }

    // inter_segment_id: the predicted id is the least of the last map's over the block (VP9 6.4.12).
    fn inter_segment(&mut self, segments: &mut [u8], info: &mut Info) {
        let segmentation = &self.scope.header.segmentation;
        if !segmentation.is_enabled {
            return;
        }
        let last = |at: usize| self.scope.previous_segments.get(at).copied().unwrap_or(0);
        let predicted = self.cells().map(last).min().unwrap_or(0);
        if !segmentation.update_map {
            self.fill_segments(segments, None);
            info.segment = predicted;
            return;
        }
        info.segment = if segmentation.temporal_update {
            let context = usize::from(self.above.is_some_and(|above| above.is_seg_predicted))
                + usize::from(self.left.is_some_and(|left| left.is_seg_predicted));
            info.is_seg_predicted = self.decoder.read(segmentation.pred_probs[context]);
            if info.is_seg_predicted { predicted } else { self.decoder.tree(&SEGMENT_TREE, &segmentation.tree_probs) }
        } else {
            self.decoder.tree(&SEGMENT_TREE, &segmentation.tree_probs)
        };
        self.fill_segments(segments, Some(info.segment));
    }

    fn skip(&mut self, segment: u8) -> bool {
        if self.scope.header.segmentation.feature(segment, SEG_LVL_SKIP).is_some() {
            return true;
        }
        let context = usize::from(self.above.is_some_and(|above| above.is_skip))
            + usize::from(self.left.is_some_and(|left| left.is_skip));
        let skip = self.decoder.read(self.scope.probs.skip[context]);
        self.counts.skip[context][usize::from(skip)] += 1;
        skip
    }

    // tx_size: chosen where the frame lets each block choose, else the largest the frame and block allow (VP9 6.4.10).
    fn tx_size(&mut self, allow_select: bool) -> u8 {
        let most = MAX_TX_SIZE[usize::from(self.place.size)];
        let tx_mode = self.scope.compressed.tx_mode;
        if !(allow_select && tx_mode == TX_MODE_SELECT && self.place.size >= BLOCK_8X8) {
            return most.min(TX_MODE_TO_BIGGEST[usize::from(tx_mode)]);
        }
        let side = |neighbor: Option<&Info>| neighbor.map(|info| if info.is_skip { most } else { info.tx_size });
        let (above, left) = (side(self.above), side(self.left));
        let (above, left) = match (above, left) {
            (Some(above), Some(left)) => (above, left),
            (Some(above), None) => (above, above),
            (None, Some(left)) => (left, left),
            (None, None) => (most, most),
        };
        let context = usize::from(above + left > most);
        let probs = &self.scope.probs;
        let probs: &[u8] = match most {
            TX_8X8 => &probs.tx8[context],
            TX_16X16 => &probs.tx16[context],
            _ => &probs.tx32[context],
        };
        let mut size = u8::from(self.decoder.read(probs[0]));
        if size != TX_4X4 && most >= TX_16X16 {
            size += u8::from(self.decoder.read(probs[1]));
            if size != TX_8X8 && most >= TX_32X32 {
                size += u8::from(self.decoder.read(probs[2]));
            }
        }
        let counts = &mut self.counts;
        match most {
            TX_8X8 => counts.tx8[context][usize::from(size)] += 1,
            TX_16X16 => counts.tx16[context][usize::from(size)] += 1,
            _ => counts.tx32[context][usize::from(size)] += 1,
        }
        size
    }

    // The intra modes of an intra frame, by the modes above and to the left of each subblock (VP9 6.4.6, 9.3.2).
    fn intra_frame_modes(&mut self, info: &mut Info) {
        info.refs = [INTRA_FRAME, NONE];
        let mode_of = |neighbor: Option<&Info>, subblock: usize| {
            neighbor.filter(|info| !info.is_inter).map_or(DC_PRED, |info| info.sub_modes[subblock])
        };
        let (across, down) = self.subblocks();
        for idy in (0..2).step_by(down) {
            for idx in (0..2).step_by(across) {
                let subblock = idy * 2 + idx;
                let above = if idy > 0 { info.sub_modes[idx] } else { mode_of(self.above, 2 + idx) };
                let left = if idx > 0 { info.sub_modes[idy * 2] } else { mode_of(self.left, 1 + idy * 2) };
                let mode = self.decoder.tree(&INTRA_MODE_TREE, &KF_Y_MODE_PROBS[usize::from(above)][usize::from(left)]);
                Self::spread_mode(info, subblock, mode, (across, down));
            }
        }
        info.mode = info.sub_modes[3];
        info.uv_mode = self.decoder.tree(&INTRA_MODE_TREE, &KF_UV_MODE_PROBS[usize::from(info.mode)]);
    }

    // The subblocks a block's modes and vectors are read for, across and down: two for each 4 pixels (VP9 6.4.6).
    fn subblocks(&self) -> (usize, usize) {
        if self.place.size >= BLOCK_8X8 {
            return (2, 2);
        }
        let size = usize::from(self.place.size);
        (1 << B_WIDTH_LOG2[size], 1 << B_HEIGHT_LOG2[size])
    }

    fn spread_mode(info: &mut Info, subblock: usize, mode: u8, (across, down): (usize, usize)) {
        info.sub_modes[subblock] = mode;
        if across == 2 {
            info.sub_modes[subblock + 1] = mode;
        }
        if down == 2 {
            info.sub_modes[subblock + 2] = mode;
            if across == 2 {
                info.sub_modes[subblock + 3] = mode;
            }
        }
    }

    // The intra modes of an intra block in an inter frame, by the block's size group (VP9 6.4.11).
    fn intra_modes(&mut self, info: &mut Info) {
        info.refs = [INTRA_FRAME, NONE];
        info.filter = SWITCHABLE_FILTERS;
        let group =
            if self.place.size >= BLOCK_8X8 { usize::from(SIZE_GROUP[usize::from(self.place.size)]) } else { 0 };
        let (across, down) = self.subblocks();
        for idy in (0..2).step_by(down) {
            for idx in (0..2).step_by(across) {
                let mode = self.decoder.tree(&INTRA_MODE_TREE, &self.scope.probs.y_mode[group]);
                self.counts.y_mode[group][usize::from(mode)] += 1;
                Self::spread_mode(info, idy * 2 + idx, mode, (across, down));
            }
        }
        info.mode = info.sub_modes[3];
        info.uv_mode = self.decoder.tree(&INTRA_MODE_TREE, &self.scope.probs.uv_mode[usize::from(info.mode)]);
        self.counts.uv_mode[usize::from(info.mode)][usize::from(info.uv_mode)] += 1;
    }

    fn is_inter(&mut self, segment: u8) -> bool {
        if let Some(reference) = self.scope.header.segmentation.feature(segment, SEG_LVL_REF_FRAME) {
            return reference != i32::from(INTRA_FRAME);
        }
        let context = match (self.above, self.left) {
            (Some(above), Some(left)) => match (!above.is_inter, !left.is_inter) {
                (true, true) => 3,
                (above, left) => usize::from(above || left),
            },
            (Some(edge), None) | (None, Some(edge)) => 2 * usize::from(!edge.is_inter),
            (None, None) => 0,
        };
        let is_inter = self.decoder.read(self.scope.probs.is_inter[context]);
        self.counts.is_inter[context][usize::from(is_inter)] += 1;
        is_inter
    }

    // read_ref_frames (VP9 6.4.15).
    fn refs(&mut self, segment: u8) -> [i8; 2] {
        if let Some(reference) = self.scope.header.segmentation.feature(segment, SEG_LVL_REF_FRAME) {
            return [reference as i8, NONE];
        }
        let compressed = self.scope.compressed;
        let is_compound = if compressed.reference_mode == REFERENCE_MODE_SELECT {
            let context = self.comp_mode_context();
            let is_compound = self.decoder.read(self.scope.probs.comp_mode[context]);
            self.counts.comp_mode[context][usize::from(is_compound)] += 1;
            is_compound
        } else {
            compressed.reference_mode == COMPOUND_REFERENCE
        };
        if is_compound {
            let fixed_index = usize::from(self.scope.header.sign_bias[compressed.fixed as usize]);
            let context = self.comp_ref_context();
            let bit = self.decoder.read(self.scope.probs.comp_ref[context]);
            self.counts.comp_ref[context][usize::from(bit)] += 1;
            let mut refs = [NONE; 2];
            refs[fixed_index] = compressed.fixed;
            refs[1 - fixed_index] = compressed.variable[usize::from(bit)];
            return refs;
        }
        let context = self.single_ref_p1_context();
        let first = self.decoder.read(self.scope.probs.single_ref[context][0]);
        self.counts.single_ref[context][0][usize::from(first)] += 1;
        if !first {
            return [LAST_FRAME, NONE];
        }
        let context = self.single_ref_p2_context();
        let second = self.decoder.read(self.scope.probs.single_ref[context][1]);
        self.counts.single_ref[context][1][usize::from(second)] += 1;
        [if second { ALTREF_FRAME } else { GOLDEN_FRAME }, NONE]
    }

    // vp9_get_reference_mode_context (VP9 9.3.2, comp_mode).
    fn comp_mode_context(&self) -> usize {
        let fixed = self.scope.compressed.fixed;
        match (self.above, self.left) {
            (Some(above), Some(left)) => match (above.is_compound(), left.is_compound()) {
                (false, false) => usize::from((above.refs[0] == fixed) ^ (left.refs[0] == fixed)),
                (false, true) => 2 + usize::from(above.refs[0] == fixed || !above.is_inter),
                (true, false) => 2 + usize::from(left.refs[0] == fixed || !left.is_inter),
                (true, true) => 4,
            },
            (Some(edge), None) | (None, Some(edge)) => {
                if edge.is_compound() {
                    3
                } else {
                    usize::from(edge.refs[0] == fixed)
                }
            }
            (None, None) => 1,
        }
    }

    // vp9_get_pred_context_comp_ref_p (VP9 9.3.2, comp_ref).
    fn comp_ref_context(&self) -> usize {
        let compressed = self.scope.compressed;
        let (fixed, variable) = (compressed.fixed, compressed.variable);
        let variable_index = 1 - usize::from(self.scope.header.sign_bias[fixed as usize]);
        let variable_of = |info: &Info| if info.is_compound() { info.refs[variable_index] } else { info.refs[0] };
        match (self.above, self.left) {
            (Some(above), Some(left)) => match (!above.is_inter, !left.is_inter) {
                (true, true) => 2,
                (true, false) | (false, true) => {
                    let edge = if above.is_inter { above } else { left };
                    1 + 2 * usize::from(variable_of(edge) != variable[1])
                }
                (false, false) => {
                    let (above_single, left_single) = (!above.is_compound(), !left.is_compound());
                    let (above_ref, left_ref) = (variable_of(above), variable_of(left));
                    if above_ref == left_ref && variable[1] == above_ref {
                        0
                    } else if above_single && left_single {
                        if (above_ref == fixed && left_ref == variable[0])
                            || (left_ref == fixed && above_ref == variable[0])
                        {
                            4
                        } else if above_ref == left_ref {
                            3
                        } else {
                            1
                        }
                    } else if above_single || left_single {
                        let compound_ref = if left_single { above_ref } else { left_ref };
                        let single_ref = if above_single { above_ref } else { left_ref };
                        if compound_ref == variable[1] && single_ref != variable[1] {
                            1
                        } else if single_ref == variable[1] && compound_ref != variable[1] {
                            2
                        } else {
                            4
                        }
                    } else if above_ref == left_ref {
                        4
                    } else {
                        2
                    }
                }
            },
            (Some(edge), None) | (None, Some(edge)) => {
                if !edge.is_inter {
                    2
                } else if edge.is_compound() {
                    4 * usize::from(edge.refs[variable_index] != variable[1])
                } else {
                    3 * usize::from(edge.refs[0] != variable[1])
                }
            }
            (None, None) => 2,
        }
    }

    // vp9_get_pred_context_single_ref_p1 (VP9 9.3.2, single_ref_p1).
    fn single_ref_p1_context(&self) -> usize {
        let has_last = |info: &Info| info.refs[0] == LAST_FRAME || info.refs[1] == LAST_FRAME;
        let edge_context = |edge: &Info| {
            if edge.is_compound() {
                1 + usize::from(has_last(edge))
            } else {
                4 * usize::from(edge.refs[0] == LAST_FRAME)
            }
        };
        match (self.above, self.left) {
            (Some(above), Some(left)) => match (!above.is_inter, !left.is_inter) {
                (true, true) => 2,
                (true, false) => edge_context(left),
                (false, true) => edge_context(above),
                (false, false) => match (above.is_compound(), left.is_compound()) {
                    (true, true) => 1 + usize::from(has_last(above) || has_last(left)),
                    (true, false) | (false, true) => {
                        let (single, compound) = if above.is_compound() { (left, above) } else { (above, left) };
                        if single.refs[0] == LAST_FRAME {
                            3 + usize::from(has_last(compound))
                        } else {
                            usize::from(has_last(compound))
                        }
                    }
                    (false, false) => {
                        2 * usize::from(above.refs[0] == LAST_FRAME) + 2 * usize::from(left.refs[0] == LAST_FRAME)
                    }
                },
            },
            (Some(edge), None) | (None, Some(edge)) => {
                if edge.is_inter {
                    edge_context(edge)
                } else {
                    2
                }
            }
            (None, None) => 2,
        }
    }

    // vp9_get_pred_context_single_ref_p2 (VP9 9.3.2, single_ref_p2).
    fn single_ref_p2_context(&self) -> usize {
        let has_golden = |info: &Info| info.refs[0] == GOLDEN_FRAME || info.refs[1] == GOLDEN_FRAME;
        match (self.above, self.left) {
            (Some(above), Some(left)) => match (!above.is_inter, !left.is_inter) {
                (true, true) => 2,
                (true, false) | (false, true) => {
                    let edge = if above.is_inter { above } else { left };
                    if edge.is_compound() {
                        1 + 2 * usize::from(has_golden(edge))
                    } else if edge.refs[0] == LAST_FRAME {
                        3
                    } else {
                        4 * usize::from(edge.refs[0] == GOLDEN_FRAME)
                    }
                }
                (false, false) => match (above.is_compound(), left.is_compound()) {
                    (true, true) => {
                        if above.refs == left.refs {
                            3 * usize::from(has_golden(above) || has_golden(left))
                        } else {
                            2
                        }
                    }
                    (true, false) | (false, true) => {
                        let (single, compound) = if above.is_compound() { (left, above) } else { (above, left) };
                        if single.refs[0] == GOLDEN_FRAME {
                            3 + usize::from(has_golden(compound))
                        } else if single.refs[0] == ALTREF_FRAME {
                            usize::from(has_golden(compound))
                        } else {
                            1 + 2 * usize::from(has_golden(compound))
                        }
                    }
                    (false, false) => {
                        let (above, left) = (above.refs[0], left.refs[0]);
                        if above == LAST_FRAME && left == LAST_FRAME {
                            3
                        } else if above == LAST_FRAME || left == LAST_FRAME {
                            let edge = if above == LAST_FRAME { left } else { above };
                            4 * usize::from(edge == GOLDEN_FRAME)
                        } else {
                            2 * usize::from(above == GOLDEN_FRAME) + 2 * usize::from(left == GOLDEN_FRAME)
                        }
                    }
                },
            },
            (Some(edge), None) | (None, Some(edge)) => {
                if !edge.is_inter || (edge.refs[0] == LAST_FRAME && !edge.is_compound()) {
                    2
                } else if edge.is_compound() {
                    3 * usize::from(has_golden(edge))
                } else {
                    4 * usize::from(edge.refs[0] == GOLDEN_FRAME)
                }
            }
            (None, None) => 2,
        }
    }

    // get_pred_context_switchable_interp (VP9 9.3.2, interp_filter).
    fn filter_context(&self) -> usize {
        let left = self.left.map_or(SWITCHABLE_FILTERS, |left| left.filter);
        let above = self.above.map_or(SWITCHABLE_FILTERS, |above| above.filter);
        usize::from(if left == above {
            left
        } else if left == SWITCHABLE_FILTERS {
            above
        } else if above == SWITCHABLE_FILTERS {
            left
        } else {
            SWITCHABLE_FILTERS
        })
    }

    // The context of the inter mode, by the modes of the first two candidates (VP9 6.4.16).
    fn mode_context(&self) -> usize {
        let counter: u8 = MV_REF_BLOCKS[usize::from(self.place.size)]
            .iter()
            .take(2)
            .filter_map(|&offset| neighbor(self.scope, self.grid, self.place, (offset[0], offset[1])))
            .map(|candidate| MODE_2_COUNTER[usize::from(candidate.mode)])
            .sum();
        usize::from(COUNTER_TO_CONTEXT[usize::from(counter)])
    }

    fn inter_mode(&mut self, context: usize) -> u8 {
        let offset = self.decoder.tree(&INTER_MODE_TREE, &self.scope.probs.inter_mode[context]);
        self.counts.inter_mode[context][usize::from(offset)] += 1;
        NEARESTMV + offset
    }

    // read_inter_block_mode_info (VP9 6.4.14 to 6.4.16, 6.4.18).
    fn inter_modes(&mut self, info: &mut Info) -> Result<(), Flaw> {
        let header = self.scope.header;
        info.refs = self.refs(info.segment);
        let is_compound = info.is_compound();
        let references = if is_compound { 2 } else { 1 };
        let context = self.mode_context();
        if header.segmentation.feature(info.segment, SEG_LVL_SKIP).is_some() {
            info.mode = ZEROMV;
            if self.place.size < BLOCK_8X8 {
                return Err(Flaw::Invalid("skip segment of a block under 8x8"));
            }
        } else if self.place.size >= BLOCK_8X8 {
            info.mode = self.inter_mode(context);
        }
        info.filter = if header.filter == SWITCHABLE {
            let context = self.filter_context();
            let filter = self.decoder.tree(&INTERP_FILTER_TREE, &self.scope.probs.interp_filter[context]);
            self.counts.interp_filter[context][usize::from(filter)] += 1;
            filter
        } else {
            header.filter
        };
        let edges = Edges::of(self.place, self.scope.mi_rows, self.scope.mi_cols);
        if self.place.size >= BLOCK_8X8 {
            let mut best = [Mv::default(); 2];
            if info.mode != ZEROMV {
                for (reference, best) in best.iter_mut().enumerate().take(references) {
                    let (count, list) = self.find_mv_refs(info.mode, info.refs[reference], None, edges);
                    *best = lower_precision(list[count - 1], header.allow_hp);
                }
            }
            let mvs = self.assign(info.mode, best, best, references)?;
            info.mvs = [mvs; 4];
            return Ok(());
        }
        let (across, down) = self.subblocks();
        let mut best_new = [Mv::default(); 2];
        let mut has_new = false;
        let mut mode = ZEROMV;
        for idy in (0..2).step_by(down) {
            for idx in (0..2).step_by(across) {
                let subblock = idy * 2 + idx;
                mode = self.inter_mode(context);
                let mut best_sub = [Mv::default(); 2];
                if mode == NEARESTMV || mode == NEARMV {
                    for (reference, best) in best_sub.iter_mut().enumerate().take(references) {
                        *best = self.sub8x8_mv(info, mode, subblock, reference, edges);
                    }
                } else if mode == NEWMV && !has_new {
                    for (reference, best) in best_new.iter_mut().enumerate().take(references) {
                        let (_, list) = self.find_mv_refs(NEWMV, info.refs[reference], None, edges);
                        *best = lower_precision(list[0], header.allow_hp);
                    }
                    has_new = true;
                }
                let mvs = self.assign(mode, best_new, best_sub, references)?;
                info.mvs[subblock] = mvs;
                if down == 2 {
                    info.mvs[subblock + 2] = mvs;
                }
                if across == 2 {
                    info.mvs[subblock + 1] = mvs;
                }
            }
        }
        info.mode = mode;
        Ok(())
    }

    // assign_mv: a new vector is the best one plus a coded difference, and lies in the bound of 7.4.13 (VP9 6.4.19).
    fn assign(&mut self, mode: u8, best: [Mv; 2], nearest: [Mv; 2], references: usize) -> Result<[Mv; 2], Flaw> {
        match mode {
            NEWMV => {
                let mut mvs = [Mv::default(); 2];
                for (mv, best) in mvs.iter_mut().zip(best).take(references) {
                    *mv = self.read_mv(best);
                    let is_valid =
                        |component: i16| -MV_LIMIT < i32::from(component) && i32::from(component) < MV_LIMIT - 1;
                    if !(is_valid(mv.row) && is_valid(mv.col)) {
                        return Err(Flaw::Invalid("motion vector"));
                    }
                }
                Ok(mvs)
            }
            NEARESTMV | NEARMV => Ok(nearest),
            _ => Ok([Mv::default(); 2]),
        }
    }

    // read_mv: the difference, counted as libvpx counts it, its eighths kept whatever the precision (VP9 6.4.19).
    fn read_mv(&mut self, best: Mv) -> Mv {
        let probs = &self.scope.probs;
        let joint = self.decoder.tree(&MV_JOINT_TREE, &probs.mv_joint);
        self.counts.mv_joint[usize::from(joint)] += 1;
        let use_hp = self.scope.header.allow_hp && use_hp(best);
        let mut difference = [0i32; 2];
        if joint == MV_JOINT_HZVNZ || joint == MV_JOINT_HNZVNZ {
            difference[0] = self.read_component(0, use_hp);
        }
        if joint == MV_JOINT_HNZVZ || joint == MV_JOINT_HNZVNZ {
            difference[1] = self.read_component(1, use_hp);
        }
        Mv { row: (i32::from(best.row) + difference[0]) as i16, col: (i32::from(best.col) + difference[1]) as i16 }
    }

    fn read_component(&mut self, component: usize, use_hp: bool) -> i32 {
        let probs: &MvProbs = &self.scope.probs.mv[component];
        let counts = &mut self.counts.mv[component];
        let is_negative = self.decoder.read(probs.sign);
        let class = self.decoder.tree(&MV_CLASS_TREE, &probs.classes);
        counts.sign[usize::from(is_negative)] += 1;
        counts.classes[usize::from(class)] += 1;
        let (integer, magnitude) = if class == MV_CLASS_0 {
            let integer = usize::from(self.decoder.read(probs.class0_bit));
            counts.class0_bit[integer] += 1;
            (integer, 0)
        } else {
            let mut integer = 0usize;
            for bit in 0..usize::from(class) {
                let value = self.decoder.read(probs.bits[bit]);
                counts.bits[bit][usize::from(value)] += 1;
                integer |= usize::from(value) << bit;
            }
            (integer, CLASS0_SIZE << (i32::from(class) + 2))
        };
        let fraction = if class == MV_CLASS_0 {
            let fraction = self.decoder.tree(&MV_FR_TREE, &probs.class0_fr[integer]);
            counts.class0_fr[integer][usize::from(fraction)] += 1;
            fraction
        } else {
            let fraction = self.decoder.tree(&MV_FR_TREE, &probs.fr);
            counts.fr[usize::from(fraction)] += 1;
            fraction
        };
        let high =
            if use_hp { self.decoder.read(if class == MV_CLASS_0 { probs.class0_hp } else { probs.hp }) } else { true };
        if class == MV_CLASS_0 {
            counts.class0_hp[usize::from(high)] += 1;
        } else {
            counts.hp[usize::from(high)] += 1;
        }
        let value = magnitude + ((integer as i32) << FRACTION_BITS | i32::from(fraction) << 1 | i32::from(high)) + 1;
        if is_negative { -value } else { value }
    }

    // append_sub8x8_mvs_for_idx: the nearest or near vector of a subblock (VP9 6.4.18).
    fn sub8x8_mv(&self, info: &Info, mode: u8, subblock: usize, reference: usize, edges: Edges) -> Mv {
        let reference_frame = info.refs[reference];
        let earlier = |index: usize| info.mvs[index][reference];
        let first_other = |list: [Mv; 2], than: Mv| list.into_iter().find(|&mv| mv != than).unwrap_or_default();
        match subblock {
            0 => {
                let (count, list) = self.find_mv_refs(mode, reference_frame, Some(0), edges);
                list[count - 1]
            }
            1 | 2 => {
                if mode == NEARESTMV {
                    earlier(0)
                } else {
                    let (_, list) = self.find_mv_refs(mode, reference_frame, Some(subblock), edges);
                    first_other(list, earlier(0))
                }
            }
            _ => {
                if mode == NEARESTMV {
                    earlier(2)
                } else if earlier(2) != earlier(1) {
                    earlier(1)
                } else if earlier(2) != earlier(0) {
                    earlier(0)
                } else {
                    let (_, list) = self.find_mv_refs(mode, reference_frame, Some(3), edges);
                    first_other(list, earlier(2))
                }
            }
        }
    }

    // dec_find_mv_refs: the candidate vectors of a reference, clamped near the block (VP9 6.4.17).
    fn find_mv_refs(&self, mode: u8, reference: i8, block: Option<usize>, edges: Edges) -> (usize, [Mv; 2]) {
        let mut list = List { mvs: [Mv::default(); 2], count: 0, is_early: mode != NEARMV };
        let positions = &MV_REF_BLOCKS[usize::from(self.place.size)];
        let sign_bias = &self.scope.header.sign_bias;
        let candidate = |offset: [i8; 2]| neighbor(self.scope, self.grid, self.place, (offset[0], offset[1]));
        let flip = |mv: Mv, from: i8| {
            if sign_bias[from as usize] != sign_bias[reference as usize] {
                Mv { row: mv.row.wrapping_neg(), col: mv.col.wrapping_neg() }
            } else {
                mv
            }
        };
        let previous =
            self.scope.previous.and_then(|motion| motion.get(self.place.row * self.scope.mi_cols + self.place.col));
        let is_done = 'search: {
            let mut is_different_found = false;
            let mut start = 0;
            if let Some(block) = block {
                for &offset in &positions[..2] {
                    if let Some(candidate) = candidate(offset) {
                        is_different_found = true;
                        let subblock = usize::from(IDX_N_COLUMN_TO_SUBBLOCK[block][usize::from(offset[1] == 0)]);
                        for which in 0..2 {
                            if candidate.refs[which] == reference {
                                if list.add(candidate.mvs[subblock][which]) {
                                    break 'search true;
                                }
                                break;
                            }
                        }
                    }
                }
                start = 2;
            }
            for &offset in &positions[start..] {
                if let Some(candidate) = candidate(offset) {
                    is_different_found = true;
                    for which in 0..2 {
                        if candidate.refs[which] == reference {
                            if list.add(candidate.mvs[3][which]) {
                                break 'search true;
                            }
                            break;
                        }
                    }
                }
            }
            if let Some(previous) = previous {
                for which in 0..2 {
                    if previous.refs[which] == reference {
                        if list.add(previous.mvs[which]) {
                            break 'search true;
                        }
                        break;
                    }
                }
            }
            if is_different_found {
                for &offset in positions {
                    let Some(candidate) = candidate(offset).filter(|candidate| candidate.is_inter) else {
                        continue;
                    };
                    let mvs = candidate.mvs[3];
                    if candidate.refs[0] != reference && list.add(flip(mvs[0], candidate.refs[0])) {
                        break 'search true;
                    }
                    if candidate.is_compound()
                        && candidate.refs[1] != reference
                        && mvs[1] != mvs[0]
                        && list.add(flip(mvs[1], candidate.refs[1]))
                    {
                        break 'search true;
                    }
                }
            }
            if let Some(previous) = previous {
                if previous.refs[0] != reference
                    && previous.refs[0] > INTRA_FRAME
                    && list.add(flip(previous.mvs[0], previous.refs[0]))
                {
                    break 'search true;
                }
                if previous.refs[1] > INTRA_FRAME
                    && previous.refs[1] != reference
                    && previous.mvs[1] != previous.mvs[0]
                    && list.add(flip(previous.mvs[1], previous.refs[1]))
                {
                    break 'search true;
                }
            }
            false
        };
        if !is_done {
            list.count = if mode == NEARMV { 2 } else { 1 };
        }
        for mv in list.mvs.iter_mut().take(list.count) {
            mv.col = clamp(i32::from(mv.col), edges.left - MV_BORDER, edges.right + MV_BORDER) as i16;
            mv.row = clamp(i32::from(mv.row), edges.top - MV_BORDER, edges.bottom + MV_BORDER) as i16;
        }
        (list.count, list.mvs)
    }
}

// The candidate vectors found so far; the search ends at the second, or at the first where only it is wanted.
struct List {
    mvs: [Mv; 2],
    count: usize,
    is_early: bool,
}

impl List {
    // ADD_MV_REF_LIST_EB: whether the search is done.
    fn add(&mut self, mv: Mv) -> bool {
        if self.count == 0 {
            self.mvs[0] = mv;
            self.count = 1;
            return self.is_early;
        }
        if mv != self.mvs[0] {
            self.mvs[1] = mv;
            self.count = 2;
            return true;
        }
        false
    }
}

// use_mv_hp: whether a vector near enough to nought keeps its eighths (VP9 6.4.20).
const fn use_hp(mv: Mv) -> bool {
    (mv.row as i32).abs() >> FRACTION_BITS < COMPANDED_MVREF_THRESH
        && (mv.col as i32).abs() >> FRACTION_BITS < COMPANDED_MVREF_THRESH
}

// lower_mv_precision: an odd component moves a step toward nought where eighths are not kept (VP9 6.4.20).
fn lower_precision(mut mv: Mv, allow_hp: bool) -> Mv {
    if !(allow_hp && use_hp(mv)) {
        for component in [&mut mv.row, &mut mv.col] {
            if *component & 1 == 1 {
                *component += if *component > 0 { -1 } else { 1 };
            }
        }
    }
    mv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_lowered_toward_nought() {
        let mv = Mv { row: 7, col: -9 };
        assert_eq!(lower_precision(mv, true), mv, "a short vector keeps its eighths where the frame allows them");
        assert_eq!(lower_precision(mv, false), Mv { row: 6, col: -8 }, "odd eighths step toward nought");
        let long = Mv { row: 65, col: 3 };
        assert_eq!(lower_precision(long, true), Mv { row: 64, col: 2 }, "a vector of 8 pixels or more loses them");
    }

    #[test]
    fn candidate_list_rules() {
        let (a, b) = (Mv { row: 1, col: 2 }, Mv { row: 3, col: 4 });
        let mut nearest = List { mvs: [Mv::default(); 2], count: 0, is_early: true };
        assert!(nearest.add(a), "the first vector ends the search for the nearest");
        let mut near = List { mvs: [Mv::default(); 2], count: 0, is_early: false };
        assert!(!near.add(a), "the near search goes on after the first");
        assert!(!near.add(a), "a repeat of the first is passed over");
        assert!(near.add(b), "a second, different vector ends it");
        assert_eq!((near.count, near.mvs), (2, [a, b]), "both kept in order");
    }

    #[test]
    fn crossed_bounds_clamp() {
        assert_eq!(clamp(5, 0, 10), 5, "inside");
        assert_eq!(clamp(-5, 0, 10), 0, "below");
        assert_eq!(clamp(-200, -128, -320), -128, "crossed bounds: below the low one gives it");
        assert_eq!(clamp(0, -128, -320), -320, "crossed bounds: above the high one gives it");
    }
}
