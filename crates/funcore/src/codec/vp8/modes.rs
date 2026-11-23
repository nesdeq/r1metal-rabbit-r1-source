use super::cost::{bool_cost, tree_cost};
use super::frame::{MACROBLOCK, SUBBLOCKS, SUBBLOCKS_ACROSS};
use super::header::{Header, State};
use super::tables::{
    ABOVE4X4, B_DC_PRED, B_HE_PRED, B_PRED, B_TM_PRED, B_VE_PRED, BMODE_PROB, BMODE_TREE, H_PRED, KF_BMODE_PROB,
    KF_UV_MODE_PROB, KF_YMODE_PROB, KF_YMODE_TREE, LEFT4X4, MB_SEGMENT_TREE, MODE_CONTEXTS, MV_16, MV_LEFT_RIGHT,
    MV_PARTITION_COUNTS, MV_PARTITIONS, MV_QUARTERS, MV_REF_TREE, MV_TOP_BOTTOM, MVP_BITS, MVP_COUNT, MVP_IS_SHORT,
    MVP_SHORT, MVP_SIGN, MVPARTITION_PROBS, MVPARTITION_TREE, NEARESTMV, NEARMV, NEW4X4, NEWMV, SMALL_MVTREE, SPLITMV,
    SUB_MV_REF_PROB, SUB_MV_REF_TREE, TM_PRED, UV_MODE_TREE, V_PRED, YMODE_TREE, ZERO4X4, ZEROMV,
};
use crate::codec::boolean::{BoolDecoder, BoolEncoder, path};
use alloc::vec::Vec;

// The frame a macroblock is predicted from (RFC 6386, 16.2).
pub(super) const INTRA_FRAME: u8 = 0;
pub(super) const LAST_FRAME: u8 = 1;
pub(super) const GOLDEN_FRAME: u8 = 2;
const ALTREF_FRAME: u8 = 3;
// A vector counts quarters of a pixel (RFC 6386, 5); a macroblock is the margin it is clamped to past each edge (16.3).
const QUARTERS: i32 = 4;
const MACROBLOCK_QUARTERS: i32 = MACROBLOCK as i32 * QUARTERS;
// The weights of the macroblock above and the one to the left, and of the one above and to the left (RFC 6386, 16.3).
const NEAR_WEIGHT: u8 = 2;
const FAR_WEIGHT: u8 = 1;
// A long component's bits: 0 to 2 first, then 9 down to 4, then 3 unless it is implied (RFC 6386, 17.1).
const LOW_BITS: usize = 3;
const LONG_BITS: usize = 10;
const IMPLIED_BIT: usize = 3;
const SHORT_LEAVES: usize = 8;
// The contexts of a split's vector mode (RFC 6386, 16.4).
const SUBMVREF_NORMAL: usize = 0;
const SUBMVREF_LEFT_ZED: usize = 1;
const SUBMVREF_ABOVE_ZED: usize = 2;
const SUBMVREF_LEFT_ABOVE_SAME: usize = 3;
const SUBMVREF_LEFT_ABOVE_ZED: usize = 4;

// A motion vector in quarters of a luma pixel.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Mv {
    pub(super) x: i16,
    pub(super) y: i16,
}

impl Mv {
    const ZERO: Mv = Mv { x: 0, y: 0 };

    const fn is_zero(self) -> bool {
        self.x == 0 && self.y == 0
    }

    // A clamped vector and a coded offset sum to less than 16 bits on the frames the decoder takes.
    fn plus(self, other: Mv) -> Mv {
        Mv { x: self.x.wrapping_add(other.x), y: self.y.wrapping_add(other.y) }
    }

    pub(super) fn minus(self, other: Mv) -> Mv {
        Mv { x: self.x.wrapping_sub(other.x), y: self.y.wrapping_sub(other.y) }
    }
}

// A macroblock's prediction record, with every subblock's vector (nought for intra) and intra mode for its neighbours.
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Macroblock {
    pub(super) y_mode: u8,
    pub(super) uv_mode: u8,
    pub(super) reference: u8,
    pub(super) segment: u8,
    pub(super) skip: bool,
    pub(super) mvs: [Mv; SUBBLOCKS],
    pub(super) modes: [u8; SUBBLOCKS],
}

// What the row below reads of a record as it codes: its reference, its mode, and the vectors of its last line of subblocks, each in four bytes, a component's two little-endian, the row's first.
pub(super) const HANDED_BYTES: usize = 2 + SUBBLOCKS_ACROSS * MV_BYTES;
const MV_BYTES: usize = 4;
const LAST_LINE: usize = SUBBLOCKS - SUBBLOCKS_ACROSS;

impl Macroblock {
    // The vector a neighbour counts by: the last subblock's (RFC 6386, 16.3).
    const fn mv(&self) -> Mv {
        self.mvs[SUBBLOCKS - 1]
    }

    // What the row below reads of the record, as `from_handed` takes it.
    pub(super) fn to_handed(self) -> [u8; HANDED_BYTES] {
        let mut bytes = [0; HANDED_BYTES];
        (bytes[0], bytes[1]) = (self.reference, self.y_mode);
        for (out, mv) in bytes[2..].as_chunks_mut::<MV_BYTES>().0.iter_mut().zip(&self.mvs[LAST_LINE..]) {
            out[..2].copy_from_slice(&mv.y.to_le_bytes());
            out[2..].copy_from_slice(&mv.x.to_le_bytes());
        }
        bytes
    }

    // A record of what the row above handed of it, the rest left as a record's default.
    pub(super) fn from_handed(bytes: &[u8; HANDED_BYTES]) -> Macroblock {
        let mut record = Macroblock { reference: bytes[0], y_mode: bytes[1], ..Macroblock::default() };
        for (mv, part) in record.mvs[LAST_LINE..].iter_mut().zip(bytes[2..].as_chunks::<MV_BYTES>().0) {
            *mv = Mv { y: i16::from_le_bytes([part[0], part[1]]), x: i16::from_le_bytes([part[2], part[3]]) };
        }
        record
    }
}

// The bounds a vector is clamped to: one macroblock past each edge of the frame, in quarters of a pixel.
#[derive(Clone, Copy)]
struct Bounds {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Bounds {
    fn new((row, column): (usize, usize), (rows, columns): (usize, usize)) -> Bounds {
        let quarters = |count: usize| count as i32 * MACROBLOCK_QUARTERS;
        Bounds {
            left: -quarters(column + 1),
            right: quarters(columns - column),
            top: -quarters(row + 1),
            bottom: quarters(rows - row),
        }
    }

    fn clamp(self, mv: Mv) -> Mv {
        let x = i32::from(mv.x).max(self.left).min(self.right);
        let y = i32::from(mv.y).max(self.top).min(self.bottom);
        Mv { x: x as i16, y: y as i16 }
    }
}

// The subblock mode a key frame's macroblock of a whole-block mode stands for in its neighbours' contexts (11.3).
pub(super) const fn subblock_mode(y_mode: u8) -> u8 {
    match y_mode {
        V_PRED => B_VE_PRED,
        H_PRED => B_HE_PRED,
        TM_PRED => B_TM_PRED,
        _ => B_DC_PRED,
    }
}

// A frame's prediction records behind a border of intra DC_PRED records above and left (RFC 6386, 11.3 and 16.3).
#[derive(Default)]
pub(super) struct Grid {
    records: Vec<Macroblock>,
    columns: usize,
    rows: usize,
}

impl Grid {
    pub(super) fn resize(&mut self, columns: usize, rows: usize) {
        self.records.clear();
        self.records.resize((columns + 1) * (rows + 1), Macroblock::default());
        (self.columns, self.rows) = (columns, rows);
    }

    const fn index(&self, row: usize, column: usize) -> usize {
        (row + 1) * (self.columns + 1) + column + 1
    }

    pub(super) fn get(&self, row: usize, column: usize) -> &Macroblock {
        &self.records[self.index(row, column)]
    }

    // Reads the prediction record of the macroblock at `row`, `column` from the first partition (RFC 6386, 19.3).
    pub(super) fn read(
        &mut self,
        decoder: &mut BoolDecoder<'_>,
        header: &Header,
        state: &State,
        (row, column): (usize, usize),
    ) {
        let (stride, here) = (self.columns + 1, self.index(row, column));
        let bounds = Bounds::new((row, column), (self.rows, self.columns));
        let (before, rest) = self.records.split_at_mut(here);
        let (Some(this), Some(left), Some(above), Some(above_left)) =
            (rest.first_mut(), before.get(here - 1), before.get(here - stride), before.get(here - stride - 1))
        else {
            return;
        };
        let segmentation = &state.segmentation;
        if segmentation.update_map {
            this.segment = decoder.tree(&MB_SEGMENT_TREE, &segmentation.tree_probs);
        } else if header.is_key {
            this.segment = 0;
        }
        this.skip = header.skip_prob.is_some_and(|probability| decoder.read(probability));
        if header.is_key {
            read_key_modes(decoder, this, left, above);
        } else if decoder.read(header.prob_intra) {
            let neighbours = Neighbours { above, left, above_left };
            read_inter_modes(decoder, header, &state.probabilities.mv, this, neighbours, bounds);
        } else {
            this.y_mode = decoder.tree(&YMODE_TREE, &state.probabilities.y_mode);
            if this.y_mode == B_PRED {
                for mode in &mut this.modes {
                    *mode = decoder.tree(&BMODE_TREE, &BMODE_PROB);
                }
            }
            this.uv_mode = decoder.tree(&UV_MODE_TREE, &state.probabilities.uv_mode);
            (this.reference, this.mvs) = (INTRA_FRAME, [Mv::ZERO; SUBBLOCKS]);
        }
    }

    pub(super) fn set(&mut self, (row, column): (usize, usize), record: Macroblock) {
        let here = self.index(row, column);
        self.records[here] = record;
    }

    // Each row's records behind the border record left of them, for rows set at once.
    pub(super) fn rows_mut(&mut self) -> core::slice::ChunksExactMut<'_, Macroblock> {
        let stride = self.columns + 1;
        self.records[stride..].chunks_exact_mut(stride)
    }

    // The records left of, above, and above and left of the one at `here`.
    fn neighbours(&self, here: usize) -> Neighbours<'_> {
        let stride = self.columns + 1;
        let record = |index: usize| &self.records[index];
        Neighbours { above: record(here - stride), left: record(here - 1), above_left: record(here - stride - 1) }
    }

    // The records about the macroblock at `row`, `column`.
    pub(super) fn neighbours_of(&self, (row, column): (usize, usize)) -> Neighbours<'_> {
        self.neighbours(self.index(row, column))
    }

    // The records left of and above the macroblock at `row`, `column`.
    pub(super) fn beside(&self, (row, column): (usize, usize)) -> (&Macroblock, &Macroblock) {
        let neighbours = self.neighbours(self.index(row, column));
        (neighbours.left, neighbours.above)
    }

    // What an inter mode of the macroblock at `row`, `column` from `reference` is coded against.
    pub(super) fn context(&self, header: &Header, (row, column): (usize, usize), reference: u8) -> Context {
        self.neighbours(self.index(row, column)).context(header, ((row, column), (self.rows, self.columns)), reference)
    }

    // Writes the prediction record of the macroblock at `row`, `column` into the first partition, as `read` reads it.
    pub(super) fn write(
        &self,
        encoder: &mut BoolEncoder,
        header: &Header,
        state: &State,
        (row, column): (usize, usize),
    ) {
        let here = self.index(row, column);
        let (this, neighbours) = (&self.records[here], self.neighbours(here));
        let segmentation = &state.segmentation;
        if segmentation.update_map {
            encoder.tree(&MB_SEGMENT_TREE, &segmentation.tree_probs, this.segment);
        }
        if let Some(probability) = header.skip_prob {
            encoder.write(probability, this.skip);
        }
        if header.is_key {
            write_key_modes(encoder, this, neighbours.left, neighbours.above);
            return;
        }
        encoder.write(header.prob_intra, this.reference != INTRA_FRAME);
        if this.reference != INTRA_FRAME {
            let bounds = Bounds::new((row, column), (self.rows, self.columns));
            write_inter_modes(encoder, header, &state.probabilities.mv, this, &neighbours, bounds);
            return;
        }
        encoder.tree(&YMODE_TREE, &state.probabilities.y_mode, this.y_mode);
        if this.y_mode == B_PRED {
            for mode in this.modes {
                encoder.tree(&BMODE_TREE, &BMODE_PROB, mode);
            }
        }
        encoder.tree(&UV_MODE_TREE, &state.probabilities.uv_mode, this.uv_mode);
    }
}

// A key frame's modes, each subblock's in the context of the modes above it and to its left (RFC 6386, 11).
fn read_key_modes(decoder: &mut BoolDecoder<'_>, this: &mut Macroblock, left: &Macroblock, above: &Macroblock) {
    this.y_mode = decoder.tree(&KF_YMODE_TREE, &KF_YMODE_PROB);
    if this.y_mode == B_PRED {
        for subblock in 0..SUBBLOCKS {
            let [above_mode, left_mode] = key_contexts(this, left, above, subblock);
            this.modes[subblock] = decoder.tree(&BMODE_TREE, &KF_BMODE_PROB[above_mode][left_mode]);
        }
    } else {
        this.modes = [subblock_mode(this.y_mode); SUBBLOCKS];
    }
    this.uv_mode = decoder.tree(&UV_MODE_TREE, &KF_UV_MODE_PROB);
    (this.reference, this.mvs) = (INTRA_FRAME, [Mv::ZERO; SUBBLOCKS]);
}

// The records above, left of, and above and left of a macroblock.
pub(super) struct Neighbours<'a> {
    pub(super) above: &'a Macroblock,
    pub(super) left: &'a Macroblock,
    pub(super) above_left: &'a Macroblock,
}

impl Neighbours<'_> {
    // What an inter mode from `reference` of the macroblock at a place, its row and column, in a frame of a size, its rows and columns, is coded against.
    pub(super) fn context(
        &self,
        header: &Header,
        (place, size): ((usize, usize), (usize, usize)),
        reference: u8,
    ) -> Context {
        Context::new(self, Bounds::new(place, size), reference, header)
    }

    // Visits the differences of the vectors `this`, the record they neighbour, codes, as `Grid::write` writes them; `position` is its place and the frame's size, as `context` takes them.
    pub(super) fn differences(
        &self,
        header: &Header,
        this: &Macroblock,
        position: ((usize, usize), (usize, usize)),
        mut visit: impl FnMut(Mv),
    ) {
        if this.reference == INTRA_FRAME {
            return;
        }
        let context = self.context(header, position, this.reference);
        match this.y_mode {
            NEWMV => visit(this.mv().minus(context.best)),
            SPLITMV => {
                let (partitioning, labels) = split(this, self);
                for (first, label) in firsts(partitioning).zip(labels) {
                    if label == NEW4X4 {
                        visit(this.mvs[first].minus(context.best));
                    }
                }
            }
            _ => {}
        }
    }
}

// The neighbours' best, nearest and near vectors and the census of their weights (RFC 6386, 16.3, find_near_mvs).
fn find_near_mvs(neighbours: &Neighbours<'_>, reference: u8, sign_bias: [bool; 4]) -> ([Mv; 3], [usize; 4]) {
    let mut near = [Mv::ZERO; 4];
    let mut counts = [0usize; 4];
    let mut index = 0;
    for (neighbour, weight) in
        [(neighbours.above, NEAR_WEIGHT), (neighbours.left, NEAR_WEIGHT), (neighbours.above_left, FAR_WEIGHT)]
    {
        if neighbour.reference == INTRA_FRAME {
            continue;
        }
        let mv = neighbour.mv();
        if mv.is_zero() {
            counts[0] += usize::from(weight);
            continue;
        }
        let biased = sign_bias[usize::from(neighbour.reference)] != sign_bias[usize::from(reference)];
        let mv = if biased { Mv { x: mv.x.wrapping_neg(), y: mv.y.wrapping_neg() } } else { mv };
        if mv != near[index] {
            index += 1;
            near[index] = mv;
        }
        counts[index] += usize::from(weight);
    }
    // Three distinct vectors: the one above and to the left merges with the nearest where they are the same.
    if counts[3] > 0 && near[index] == near[1] {
        counts[1] += 1;
    }
    counts[3] = [neighbours.above, neighbours.left, neighbours.above_left]
        .iter()
        .zip([NEAR_WEIGHT, NEAR_WEIGHT, FAR_WEIGHT])
        .filter(|(neighbour, _)| neighbour.y_mode == SPLITMV)
        .map(|(_, weight)| usize::from(weight))
        .sum();
    if counts[2] > counts[1] {
        counts.swap(1, 2);
        near.swap(1, 2);
    }
    if counts[1] >= counts[0] {
        near[0] = near[1];
    }
    ([near[0], near[1], near[2]], counts)
}

fn read_component(decoder: &mut BoolDecoder<'_>, probabilities: &[u8; MVP_COUNT]) -> i16 {
    let mut value = 0i16;
    if decoder.read(probabilities[MVP_IS_SHORT]) {
        for bit in (0..LOW_BITS).chain((IMPLIED_BIT + 1..LONG_BITS).rev()) {
            value += i16::from(decoder.read(probabilities[MVP_BITS + bit])) << bit;
        }
        // A long value is at least 8: below 16 its bit 3 is set and not coded (RFC 6386, 17.1).
        if value >> (IMPLIED_BIT + 1) == 0 || decoder.read(probabilities[MVP_BITS + IMPLIED_BIT]) {
            value += 1 << IMPLIED_BIT;
        }
    } else {
        value = i16::from(decoder.tree(&SMALL_MVTREE, &probabilities[MVP_SHORT..MVP_SHORT + SHORT_LEAVES - 1]));
    }
    if value != 0 && decoder.read(probabilities[MVP_SIGN]) { -value } else { value }
}

// A vector: the row's component, then the column's (RFC 6386, 17.2).
fn read_mv(decoder: &mut BoolDecoder<'_>, probabilities: &[[u8; MVP_COUNT]; 2]) -> Mv {
    let y = read_component(decoder, &probabilities[0]);
    let x = read_component(decoder, &probabilities[1]);
    Mv { x, y }
}

const fn sub_mv_context(left: Mv, above: Mv) -> usize {
    let (is_left_zero, is_above_zero) = (left.is_zero(), above.is_zero());
    let is_same = left.x == above.x && left.y == above.y;
    if is_same && is_left_zero {
        SUBMVREF_LEFT_ABOVE_ZED
    } else if is_same {
        SUBMVREF_LEFT_ABOVE_SAME
    } else if is_above_zero {
        SUBMVREF_ABOVE_ZED
    } else if is_left_zero {
        SUBMVREF_LEFT_ZED
    } else {
        SUBMVREF_NORMAL
    }
}

// SPLITMV's partitioning and each part's vector, which later parts take unclamped as left or above (RFC 6386, 16.4).
fn read_split(
    decoder: &mut BoolDecoder<'_>,
    probabilities: &[[u8; MVP_COUNT]; 2],
    this: &mut Macroblock,
    neighbours: &Neighbours<'_>,
    best: Mv,
) {
    let partitioning = decoder.tree(&MVPARTITION_TREE, &MVPARTITION_PROBS);
    let layout = &MV_PARTITIONS[usize::from(partitioning)];
    for (part, first) in firsts(partitioning).enumerate() {
        let (left, above) = sub_neighbours(this, neighbours, first);
        let mv = match decoder.tree(&SUB_MV_REF_TREE, &SUB_MV_REF_PROB[sub_mv_context(left, above)]) {
            LEFT4X4 => left,
            ABOVE4X4 => above,
            ZERO4X4 => Mv::ZERO,
            _ => read_mv(decoder, probabilities).plus(best),
        };
        for (subblock, &owner) in this.mvs.iter_mut().zip(layout) {
            if usize::from(owner) == part {
                *subblock = mv;
            }
        }
    }
}

// What an inter macroblock's mode is coded against: the clamped best, nearest and near vectors, and the probabilities of the mode tree by the neighbours' weights (RFC 6386, 16.3).
#[derive(Clone, Copy, Debug)]
pub(super) struct Context {
    pub(super) best: Mv,
    pub(super) nearest: Mv,
    pub(super) near: Mv,
    pub(super) probabilities: [u8; 4],
}

impl Context {
    fn new(neighbours: &Neighbours<'_>, bounds: Bounds, reference: u8, header: &Header) -> Context {
        let sign_bias = [false, false, header.sign_bias_golden, header.sign_bias_altref];
        let ([best, nearest, near], counts) = find_near_mvs(neighbours, reference, sign_bias);
        Context {
            best: bounds.clamp(best),
            nearest: bounds.clamp(nearest),
            near: bounds.clamp(near),
            probabilities: core::array::from_fn(|node| MODE_CONTEXTS[counts[node]][node]),
        }
    }

    // The vector of a whole-macroblock mode; SPLITMV's are its parts'.
    pub(super) fn mv(&self, mode: u8) -> Option<Mv> {
        match mode {
            NEARESTMV => Some(self.nearest),
            NEARMV => Some(self.near),
            ZEROMV => Some(Mv::ZERO),
            _ => None,
        }
    }
}

// An inter macroblock's reference frame, mode and vectors (RFC 6386, 16.2 to 16.4, and the reference's decode_mvs).
fn read_inter_modes(
    decoder: &mut BoolDecoder<'_>,
    header: &Header,
    probabilities: &[[u8; MVP_COUNT]; 2],
    this: &mut Macroblock,
    neighbours: Neighbours<'_>,
    bounds: Bounds,
) {
    this.reference = if !decoder.read(header.prob_last) {
        LAST_FRAME
    } else if decoder.read(header.prob_golden) {
        ALTREF_FRAME
    } else {
        GOLDEN_FRAME
    };
    let context = Context::new(&neighbours, bounds, this.reference, header);
    this.y_mode = decoder.tree(&MV_REF_TREE, &context.probabilities);
    let mv = match this.y_mode {
        NEWMV => read_mv(decoder, probabilities).plus(context.best),
        SPLITMV => {
            read_split(decoder, probabilities, this, &neighbours, context.best);
            return;
        }
        mode => context.mv(mode).unwrap_or_default(),
    };
    this.mvs = [mv; SUBBLOCKS];
}

// The bools of a vector's component as read_component reads them, each by the index of its probability (17.1).
fn component_bools(value: i16, mut bool: impl FnMut(usize, bool)) {
    let magnitude = value.unsigned_abs();
    let is_long = usize::from(magnitude) >= SHORT_LEAVES;
    bool(MVP_IS_SHORT, is_long);
    if is_long {
        for bit in (0..LOW_BITS).chain((IMPLIED_BIT + 1..LONG_BITS).rev()) {
            bool(MVP_BITS + bit, magnitude >> bit & 1 == 1);
        }
        if magnitude >> (IMPLIED_BIT + 1) != 0 {
            bool(MVP_BITS + IMPLIED_BIT, magnitude >> IMPLIED_BIT & 1 == 1);
        }
    } else {
        let (steps, depth) = path(&SMALL_MVTREE, magnitude as u8);
        for &(node, bit) in &steps[..depth] {
            bool(MVP_SHORT + node / 2, bit);
        }
    }
    if magnitude != 0 {
        bool(MVP_SIGN, value < 0);
    }
}

// The bools of a vector's components, the row's first, each by its component and the index of its probability.
fn mv_bools(difference: Mv, mut bool: impl FnMut(usize, usize, bool)) {
    for (component, value) in [difference.y, difference.x].into_iter().enumerate() {
        component_bools(value, |index, bit| bool(component, index, bit));
    }
}

// The largest magnitude a component codes: ten bits (RFC 6386, 17.1).
const MOST_MAGNITUDE: usize = (1 << LONG_BITS) - 1;

// What each value of a component from -MOST_MAGNITUDE to MOST_MAGNITUDE costs, in 256ths of a bit, by component.
pub(super) type MvCosts = [Vec<u32>; 2];

pub(super) fn mv_costs(probabilities: &[[u8; MVP_COUNT]; 2]) -> MvCosts {
    core::array::from_fn(|component| {
        let values = -(MOST_MAGNITUDE as i16)..=MOST_MAGNITUDE as i16;
        let cost = |value: i16| {
            let mut sum = 0;
            component_bools(value, |index, bit| sum += bool_cost(probabilities[component][index], bit));
            sum
        };
        values.map(cost).collect()
    })
}

// What coding a vector's difference costs, in 256ths of a bit; no component is past MOST_MAGNITUDE.
#[inline]
pub(super) fn mv_cost(costs: &MvCosts, difference: Mv) -> u32 {
    let place = |value: i16| (i32::from(value) + MOST_MAGNITUDE as i32) as usize;
    costs[0][place(difference.y)] + costs[1][place(difference.x)]
}

fn write_mv(encoder: &mut BoolEncoder, probabilities: &[[u8; MVP_COUNT]; 2], difference: Mv) {
    mv_bools(difference, |component, index, bit| encoder.write(probabilities[component][index], bit));
}

// How often each probability of the two components decided each branch.
pub(super) type MvCounts = [[[u32; 2]; MVP_COUNT]; 2];

pub(super) fn count_mv(counts: &mut MvCounts, difference: Mv) {
    mv_bools(difference, |component, index, bit| counts[component][index][usize::from(bit)] += 1);
}

// The coarsest partitioning whose parts each hold one of the macroblock's vectors, and each part's label: the first of LEFT4X4, ABOVE4X4 and ZERO4X4 that names its vector, NEW4X4 otherwise.
fn split(this: &Macroblock, neighbours: &Neighbours<'_>) -> (u8, [u8; SUBBLOCKS]) {
    let partitioning = [MV_TOP_BOTTOM, MV_LEFT_RIGHT, MV_QUARTERS]
        .into_iter()
        .find(|&partitioning| {
            let layout = &MV_PARTITIONS[usize::from(partitioning)];
            (0..SUBBLOCKS).all(|subblock| {
                let first = layout.iter().position(|&owner| owner == layout[subblock]).unwrap_or(0);
                this.mvs[subblock] == this.mvs[first]
            })
        })
        .unwrap_or(MV_16);
    let mut labels = [NEW4X4; SUBBLOCKS];
    for (first, label) in firsts(partitioning).zip(&mut labels) {
        let (left, above) = sub_neighbours(this, neighbours, first);
        let mv = this.mvs[first];
        *label = [(LEFT4X4, left), (ABOVE4X4, above), (ZERO4X4, Mv::ZERO)]
            .into_iter()
            .find(|&(_, candidate)| candidate == mv)
            .map_or(NEW4X4, |(label, _)| label);
    }
    (partitioning, labels)
}

// The first subblock of each part of a partitioning, in the order of the parts.
fn firsts(partitioning: u8) -> impl Iterator<Item = usize> {
    let layout = &MV_PARTITIONS[usize::from(partitioning)];
    (0..MV_PARTITION_COUNTS[usize::from(partitioning)])
        .map(|part| layout.iter().position(|&owner| usize::from(owner) == part).unwrap_or(0))
}

// The vectors left of and above a subblock: its neighbours' in the macroblock or in the macroblocks beside it.
fn sub_neighbours(this: &Macroblock, neighbours: &Neighbours<'_>, first: usize) -> (Mv, Mv) {
    let left = if first.is_multiple_of(SUBBLOCKS_ACROSS) {
        neighbours.left.mvs[first + SUBBLOCKS_ACROSS - 1]
    } else {
        this.mvs[first - 1]
    };
    let above = if first < SUBBLOCKS_ACROSS {
        neighbours.above.mvs[first + SUBBLOCKS - SUBBLOCKS_ACROSS]
    } else {
        this.mvs[first - SUBBLOCKS_ACROSS]
    };
    (left, above)
}

fn write_inter_modes(
    encoder: &mut BoolEncoder,
    header: &Header,
    probabilities: &[[u8; MVP_COUNT]; 2],
    this: &Macroblock,
    neighbours: &Neighbours<'_>,
    bounds: Bounds,
) {
    encoder.write(header.prob_last, this.reference != LAST_FRAME);
    if this.reference != LAST_FRAME {
        encoder.write(header.prob_golden, this.reference == ALTREF_FRAME);
    }
    let context = Context::new(neighbours, bounds, this.reference, header);
    encoder.tree(&MV_REF_TREE, &context.probabilities, this.y_mode);
    match this.y_mode {
        NEWMV => write_mv(encoder, probabilities, this.mv().minus(context.best)),
        SPLITMV => {
            let (partitioning, labels) = split(this, neighbours);
            encoder.tree(&MVPARTITION_TREE, &MVPARTITION_PROBS, partitioning);
            for (first, label) in firsts(partitioning).zip(labels) {
                let (left, above) = sub_neighbours(this, neighbours, first);
                encoder.tree(&SUB_MV_REF_TREE, &SUB_MV_REF_PROB[sub_mv_context(left, above)], label);
                if label == NEW4X4 {
                    write_mv(encoder, probabilities, this.mvs[first].minus(context.best));
                }
            }
        }
        _ => {}
    }
}

// What the parts of SPLITMV cost against `best` as `split` labels them, in 256ths of a bit: the partitioning, the labels and the new vectors.
pub(super) fn split_cost(
    this: &Macroblock,
    (left, above): (&Macroblock, &Macroblock),
    best: Mv,
    costs: &MvCosts,
) -> u32 {
    let neighbours = Neighbours { above, left, above_left: above };
    let (partitioning, labels) = split(this, &neighbours);
    let mut cost = tree_cost(&MVPARTITION_TREE, &MVPARTITION_PROBS, partitioning);
    for (first, label) in firsts(partitioning).zip(labels) {
        let (left, above) = sub_neighbours(this, &neighbours, first);
        cost += tree_cost(&SUB_MV_REF_TREE, &SUB_MV_REF_PROB[sub_mv_context(left, above)], label);
        if label == NEW4X4 {
            cost += mv_cost(costs, this.mvs[first].minus(best));
        }
    }
    cost
}

// A key frame's modes, as read_key_modes reads them.
fn write_key_modes(encoder: &mut BoolEncoder, this: &Macroblock, left: &Macroblock, above: &Macroblock) {
    encoder.tree(&KF_YMODE_TREE, &KF_YMODE_PROB, this.y_mode);
    if this.y_mode == B_PRED {
        for subblock in 0..SUBBLOCKS {
            let [above_mode, left_mode] = key_contexts(this, left, above, subblock);
            encoder.tree(&BMODE_TREE, &KF_BMODE_PROB[above_mode][left_mode], this.modes[subblock]);
        }
    }
    encoder.tree(&UV_MODE_TREE, &KF_UV_MODE_PROB, this.uv_mode);
}

// The modes above a key frame's subblock and to its left, which its mode is coded in the context of (RFC 6386, 11.3).
pub(super) fn key_contexts(this: &Macroblock, left: &Macroblock, above: &Macroblock, subblock: usize) -> [usize; 2] {
    let above_mode = if subblock < SUBBLOCKS_ACROSS {
        above.modes[subblock + SUBBLOCKS - SUBBLOCKS_ACROSS]
    } else {
        this.modes[subblock - SUBBLOCKS_ACROSS]
    };
    let left_mode = if subblock.is_multiple_of(SUBBLOCKS_ACROSS) {
        left.modes[subblock + SUBBLOCKS_ACROSS - 1]
    } else {
        this.modes[subblock - 1]
    };
    [usize::from(above_mode), usize::from(left_mode)]
}

#[cfg(test)]
mod tests {
    use super::super::tables::{B_HU_PRED, DC_PRED, DEFAULT_MV_CONTEXT, NUM_INTRA_BMODES, YMODE_PROB};
    use super::*;

    fn inter(reference: u8, mv: Mv, y_mode: u8) -> Macroblock {
        Macroblock { reference, y_mode, mvs: [mv; SUBBLOCKS], ..Macroblock::default() }
    }

    #[test]
    fn near_mvs_census() {
        let (a, b) = (Mv { x: 8, y: -4 }, Mv { x: 2, y: 2 });
        let intra = Macroblock::default();
        let neighbours =
            Neighbours { above: &inter(LAST_FRAME, a, NEWMV), left: &inter(LAST_FRAME, a, NEARMV), above_left: &intra };
        let ([best, nearest, near], counts) = find_near_mvs(&neighbours, LAST_FRAME, [false; 4]);
        assert_eq!((best, nearest, near, counts), (a, a, Mv::ZERO, [0, 4, 0, 0]), "above and left agree: weight 4");
        let neighbours = Neighbours {
            above: &inter(LAST_FRAME, Mv::ZERO, ZEROMV),
            left: &inter(LAST_FRAME, b, SPLITMV),
            above_left: &inter(LAST_FRAME, a, NEWMV),
        };
        let ([best, nearest, near], counts) = find_near_mvs(&neighbours, LAST_FRAME, [false; 4]);
        assert_eq!(counts, [2, 2, 1, 2], "a zero above, b to the left, a above left; the left one split");
        assert_eq!((best, nearest, near), (b, b, a), "the nearest weighs as much as zero, so it is best");
        let neighbours = Neighbours {
            above: &inter(GOLDEN_FRAME, a, NEWMV),
            left: &intra,
            above_left: &inter(LAST_FRAME, a, NEWMV),
        };
        let ([_, nearest, near], counts) = find_near_mvs(&neighbours, LAST_FRAME, [false, false, true, false]);
        assert_eq!(
            nearest,
            Mv { x: -8, y: 4 },
            "a golden neighbour's vector is turned round for a sign bias that differs"
        );
        assert_eq!(
            (near, counts[..3].to_vec()),
            (a, alloc::vec![0, 2, 1]),
            "and so differs from the same vector unturned"
        );
    }

    #[test]
    fn handed_records_read_back() {
        let mvs = core::array::from_fn(|subblock| Mv { x: subblock as i16 * 37 - 300, y: 511 - subblock as i16 * 64 });
        let record = Macroblock { reference: GOLDEN_FRAME, y_mode: SPLITMV, uv_mode: 2, mvs, ..Macroblock::default() };
        let read = Macroblock::from_handed(&record.to_handed());
        assert_eq!((read.reference, read.y_mode), (GOLDEN_FRAME, SPLITMV), "the reference and the mode");
        assert_eq!(read.mvs[LAST_LINE..], mvs[LAST_LINE..], "the last line's vectors, which the row below reads");
        assert_eq!((read.uv_mode, read.mvs[0]), (0, Mv::ZERO), "nothing the row below does not read");
    }

    #[test]
    fn clamped_to_one_macroblock_past_edges() {
        let bounds = Bounds::new((0, 1), (2, 3));
        assert_eq!(bounds.clamp(Mv { x: -500, y: 500 }), Mv { x: -128, y: 128 }, "two macroblocks left, two down");
        assert_eq!(bounds.clamp(Mv { x: 300, y: -300 }), Mv { x: 128, y: -64 }, "two right, one up");
        assert_eq!(bounds.clamp(Mv { x: 5, y: -7 }), Mv { x: 5, y: -7 }, "a vector inside the bounds stays");
    }

    #[test]
    fn components_short_and_long() {
        let probabilities = &DEFAULT_MV_CONTEXT[0];
        let mut encoder = BoolEncoder::new();
        // 5 is short: "101" down the tree, then a set sign.
        encoder.write(probabilities[MVP_IS_SHORT], false);
        encoder.write(probabilities[MVP_SHORT], true);
        encoder.write(probabilities[MVP_SHORT + 4], false);
        encoder.write(probabilities[MVP_SHORT + 5], true);
        encoder.write(probabilities[MVP_SIGN], true);
        // 12 is long: bits 0 to 2 are 0, 0, 1; bits 9 to 4 clear; bit 3 is implied.
        encoder.write(probabilities[MVP_IS_SHORT], true);
        for (bit, value) in [(0, false), (1, false), (2, true)] {
            encoder.write(probabilities[MVP_BITS + bit], value);
        }
        for bit in (4..10).rev() {
            encoder.write(probabilities[MVP_BITS + bit], false);
        }
        encoder.write(probabilities[MVP_SIGN], false);
        // 1000 is long with bit 3 coded: 1000 = 0b11_1110_1000.
        encoder.write(probabilities[MVP_IS_SHORT], true);
        for bit in (0..3).chain((4..10).rev()) {
            encoder.write(probabilities[MVP_BITS + bit], 1000 >> bit & 1 == 1);
        }
        encoder.write(probabilities[MVP_BITS + 3], true);
        encoder.write(probabilities[MVP_SIGN], true);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        assert_eq!(read_component(&mut decoder, probabilities), -5, "a short value with its sign");
        assert_eq!(read_component(&mut decoder, probabilities), 12, "bit 3 implied below 16");
        assert_eq!(read_component(&mut decoder, probabilities), -1000, "a long value with bit 3 coded");
        assert!(!decoder.is_past_end(), "the three components fill no more than was written");
    }

    // Records of a frame of `rows` by `columns` macroblocks, each mode and vector as a decoder would find them.
    fn records(header: &Header, (rows, columns): (usize, usize), seed: u32) -> Grid {
        let mut state = seed;
        let mut next = |bound: u32| {
            state = crate::base::math::xorshift32(state);
            (state % bound) as usize
        };
        let mut grid = Grid::default();
        grid.resize(columns, rows);
        let small = |value: usize| value as i16 - 511;
        for row in 0..rows {
            for column in 0..columns {
                let mut this = Macroblock { skip: next(2) == 0, ..Macroblock::default() };
                let kind = if header.is_key { 0 } else { next(8) };
                if kind < 2 {
                    this.y_mode = next(5) as u8;
                    this.uv_mode = next(4) as u8;
                    this.modes = if this.y_mode == B_PRED {
                        core::array::from_fn(|_| next(NUM_INTRA_BMODES as u32) as u8)
                    } else {
                        [subblock_mode(this.y_mode); SUBBLOCKS]
                    };
                    grid.set((row, column), this);
                    continue;
                }
                this.reference = [LAST_FRAME, GOLDEN_FRAME, ALTREF_FRAME][next(3)];
                let context = grid.context(header, (row, column), this.reference);
                this.y_mode = [NEARESTMV, NEARMV, ZEROMV, NEWMV, SPLITMV][next(5)];
                let random = Mv { x: small(next(1023)), y: small(next(1023)) };
                this.mvs = [context.mv(this.y_mode).unwrap_or(random); SUBBLOCKS];
                if this.y_mode == SPLITMV {
                    let layout = &MV_PARTITIONS[next(4)];
                    let neighbours = grid.neighbours(grid.index(row, column));
                    for part in 0..=*layout.iter().max().unwrap_or(&0) {
                        let first = layout.iter().position(|&owner| owner == part).unwrap_or(0);
                        let (left, above) = sub_neighbours(&this, &neighbours, first);
                        let fresh = Mv { x: small(next(1023)), y: small(next(1023)) };
                        let mv = [left, above, Mv::ZERO, fresh][next(4)];
                        for (subblock, &owner) in layout.iter().enumerate() {
                            if owner == part {
                                this.mvs[subblock] = mv;
                            }
                        }
                    }
                }
                grid.set((row, column), this);
            }
        }
        grid
    }

    #[test]
    fn written_modes_read_back() {
        let inter =
            Header { prob_intra: 60, prob_last: 150, prob_golden: 100, skip_prob: Some(200), ..Header::default() };
        let key = Header { is_key: true, skip_prob: None, ..inter };
        for (header, seed) in [(inter, 1), (key, 2), (inter, 3)] {
            let (rows, columns) = (9, 11);
            let written = records(&header, (rows, columns), seed);
            let state = State::default();
            let mut encoder = BoolEncoder::new();
            for row in 0..rows {
                for column in 0..columns {
                    written.write(&mut encoder, &header, &state, (row, column));
                }
            }
            let bytes = encoder.finish();
            let mut decoder = BoolDecoder::new(&bytes);
            let mut read = Grid::default();
            read.resize(columns, rows);
            for row in 0..rows {
                for column in 0..columns {
                    read.read(&mut decoder, &header, &state, (row, column));
                    let (wrote, got) = (written.get(row, column), read.get(row, column));
                    let place = (row, column, header.is_key);
                    assert_eq!(got.reference, wrote.reference, "{place:?}: the reference frame");
                    assert_eq!(got.y_mode, wrote.y_mode, "{place:?}: the mode");
                    assert_eq!(got.mvs, wrote.mvs, "{place:?}: every subblock's vector");
                    assert_eq!(got.skip, wrote.skip && header.skip_prob.is_some(), "{place:?}: the skip flag");
                    if wrote.reference == INTRA_FRAME {
                        assert_eq!(got.uv_mode, wrote.uv_mode, "{place:?}: the chroma mode");
                    }
                    if wrote.y_mode == B_PRED || header.is_key {
                        assert_eq!(got.modes, wrote.modes, "{place:?}: the subblock modes");
                    }
                }
            }
            assert!(!decoder.is_past_end(), "{} macroblocks within the bytes written", rows * columns);
        }
    }

    #[test]
    fn tree_paths() {
        let tree = &BMODE_TREE;
        let (steps, depth) = path(tree, B_HU_PRED);
        assert_eq!(depth, steps.len(), "the deepest leaf fills the path's room");
        let expected = [(0, true), (2, true), (4, true), (6, true), (12, true), (14, true), (16, true)];
        assert_eq!(steps[..depth], expected, "B_HU_PRED's nodes and branches from the root");
        let (steps, depth) = path(&MB_SEGMENT_TREE, 0);
        assert_eq!(steps[..depth], [(0, false), (2, false)], "a leaf of value nought below the root");
        for value in 0..10u8 {
            let mut encoder = BoolEncoder::new();
            encoder.tree(tree, &BMODE_PROB, value);
            let bytes = encoder.finish();
            let read = BoolDecoder::new(&bytes).tree(tree, &BMODE_PROB);
            assert_eq!(read, value, "subblock mode {value} written and read down its path");
        }
    }

    #[test]
    fn signed_values_and_trees() {
        let mut encoder = BoolEncoder::new();
        for (flag, magnitude, sign) in [(1, 0b0101, 1), (0, 0, 0), (1, 0b0011, 0)] {
            encoder.literal(flag, 1);
            if flag == 1 {
                encoder.literal(magnitude, 4);
                encoder.literal(sign, 1);
            }
        }
        // DC_PRED is "0" in the luma tree of an interframe and B_PRED is "111".
        encoder.write(112, false);
        encoder.write(112, true);
        encoder.write(86, true);
        encoder.write(37, true);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        assert_eq!(decoder.delta(4), -5, "a flag, the magnitude 0101 and a set sign");
        assert_eq!(decoder.delta(4), 0, "a clear flag and nothing after it");
        assert_eq!(decoder.delta(4), 3, "the magnitude 0011 and a clear sign");
        assert_eq!(decoder.tree(&YMODE_TREE, &YMODE_PROB), DC_PRED, "a leaf at the first node");
        assert_eq!(decoder.tree(&YMODE_TREE, &YMODE_PROB), B_PRED, "a leaf three nodes down");
    }

    #[test]
    fn split_contexts() {
        let (zero, one, two) = (Mv::ZERO, Mv { x: 1, y: 0 }, Mv { x: 0, y: 2 });
        let contexts =
            [(zero, zero), (one, one), (one, zero), (zero, two), (one, two)].map(|(l, a)| sub_mv_context(l, a));
        assert_eq!(
            contexts,
            [SUBMVREF_LEFT_ABOVE_ZED, SUBMVREF_LEFT_ABOVE_SAME, SUBMVREF_ABOVE_ZED, SUBMVREF_LEFT_ZED, SUBMVREF_NORMAL],
            "both nought, the same, above nought, left nought, and neither"
        );
    }
}
