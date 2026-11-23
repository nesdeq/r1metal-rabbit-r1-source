use super::cost::{bool_cost, leaf_costs, path_cost, weigh};
use super::frame::{CHROMA_BLOCK, CHROMA_SUBBLOCKS, MACROBLOCK, SUBBLOCK, SUBBLOCKS, SUBBLOCKS_ACROSS, side};
use super::header::{Header, Probabilities};
use super::inter::Filter;
use super::intra;
use super::modes::{
    Context, GOLDEN_FRAME, Grid, INTRA_FRAME, LAST_FRAME, Macroblock, Mv, MvCosts, key_contexts, mv_cost, split_cost,
    subblock_mode,
};
use super::quantize::{Coder, Steps};
use super::reconstruct::{self, ChromaTarget, Lines, Pixels, Target};
use super::search::{Block, Search};
use super::tables::{
    B_PRED, BMODE_TREE, DC_PRED, H_PRED, KF_BMODE_PROB, KF_UV_MODE_PROB, KF_YMODE_PROB, KF_YMODE_TREE, MV_PARTITIONS,
    MV_QUARTERS, MV_REF_TREE, NEARESTMV, NEARMV, NEWMV, NUM_INTRA_BMODES, SPLITMV, TM_PRED, UV_MODE_TREE, V_PRED,
    YMODE_TREE, ZEROMV,
};
use super::tokens::{Levels, Residue};
use crate::base::simd;
use crate::codec::boolean::{Path, path};
use crate::codec::picture::{Frame, Geometry};
use core::ops::Range;

const WHOLE_MODES: [u8; 4] = [DC_PRED, V_PRED, H_PRED, TM_PRED];
const Y_MODES: usize = B_PRED as usize + 1;
const UV_MODES: usize = WHOLE_MODES.len();
const INTER_MODES: usize = (SPLITMV - NEARESTMV + 1) as usize;
const LUMA_PIXELS: usize = MACROBLOCK * MACROBLOCK;
const CHROMA_PIXELS: usize = CHROMA_BLOCK * CHROMA_BLOCK;
// A quarter of a macroblock, the part of SPLITMV the encoder searches.
const QUARTER: usize = MACROBLOCK / 2;
// The cost of a whole-macroblock inter prediction, in steps of the AC quantizer, past which SPLITMV is searched where UNEVEN_QUARTER holds too, so that its four searches go only where a whole vector predicts poorly.
const SPLIT_FROM_STEPS: u32 = 16;
// The same past which intra prediction is tried, so that its modes are reckoned only where a whole vector predicts poorly.
const INTRA_FROM_STEPS: u32 = 8;
// SPLITMV is searched where, besides, the quarter its whole vector predicts worst differs by more than half again the quarters' mean, as where a part moves otherwise.
const UNEVEN_QUARTER: (u32, u32) = (3, 2);
// A key frame's subblock modes' costs by the modes above and to the left (RFC 6386, 11.5).
const KEY_SUBBLOCK_COSTS: [[[u32; NUM_INTRA_BMODES]; NUM_INTRA_BMODES]; NUM_INTRA_BMODES] = {
    let mut costs = [[[0; NUM_INTRA_BMODES]; NUM_INTRA_BMODES]; NUM_INTRA_BMODES];
    let mut above = 0;
    while above < NUM_INTRA_BMODES {
        let mut left = 0;
        while left < NUM_INTRA_BMODES {
            costs[above][left] = leaf_costs(&BMODE_TREE, &KF_BMODE_PROB[above][left]);
            left += 1;
        }
        above += 1;
    }
    costs
};

// What each intra mode costs in a frame, in 256ths of a bit: the luma modes and the chroma modes.
#[derive(Clone, Copy, Default)]
pub(super) struct ModeCosts {
    y: [u32; Y_MODES],
    uv: [u32; UV_MODES],
}

impl ModeCosts {
    pub(super) fn new(is_key: bool, probabilities: &Probabilities) -> ModeCosts {
        if is_key {
            ModeCosts { y: leaf_costs(&KF_YMODE_TREE, &KF_YMODE_PROB), uv: leaf_costs(&UV_MODE_TREE, &KF_UV_MODE_PROB) }
        } else {
            ModeCosts {
                y: leaf_costs(&YMODE_TREE, &probabilities.y_mode),
                uv: leaf_costs(&UV_MODE_TREE, &probabilities.uv_mode),
            }
        }
    }
}

// What the band's decisions read: the source padded to whole macroblocks, the references, and the frame's settings.
#[derive(Clone, Copy)]
pub(super) struct Inputs<'a> {
    pub(super) source: &'a Frame,
    pub(super) geometries: &'a [Geometry; 3],
    pub(super) last: &'a Frame,
    pub(super) golden: Option<&'a Frame>,
    pub(super) halves: [&'a [u8]; 3],
    pub(super) header: &'a Header,
    pub(super) modes: &'a ModeCosts,
    pub(super) mv_costs: &'a MvCosts,
    pub(super) steps: &'a Steps,
    // What a bit is worth in absolute differences, in 256ths, and the AC quantizer's step.
    pub(super) lambda: u32,
    pub(super) step: u32,
    // The probabilities the last frame coded its references with: intra, last, golden.
    pub(super) references: [u8; 3],
    pub(super) vectors: &'a [Mv],
    // The last frame's records.
    pub(super) previous: &'a Grid,
    pub(super) size: (usize, usize),
}

impl Inputs<'_> {
    const fn weigh(&self, cost: u32) -> u32 {
        weigh(self.lambda, cost)
    }
}

// The SATD of a block of `SIZE` of the source at `at` against a prediction at `place` of `plane`: the sum of the absolute Walsh-Hadamard coefficients of each 4x4 difference, halved, close to what its residue costs.
fn satd<const SIZE: usize>(
    (source, at, stride): (&[u8], usize, usize),
    (plane, place, plane_stride): (&[u8], usize, usize),
) -> u32 {
    (simd::satd((&source[at..], stride), (&plane[place..], plane_stride), (SIZE, SIZE)) + 1) >> 1
}

// An intra choice: the record and what its luma costs.
struct Intra {
    record: Macroblock,
    cost: u32,
}

// What a band decides of a macroblock: the record coded, an inter one or a key frame's intra one, and for an inter one what it costs and the intra modes that may cost less, which the coding checks against the edges it reconstructs.
#[derive(Clone, Copy, Default)]
pub(super) struct Choice {
    pub(super) record: Macroblock,
    pub(super) cost: u32,
    pub(super) intra: Option<Macroblock>,
}

// The cheapest whole-block luma mode of the macroblock of `source` by the edges of its place in `plane`, `above` and `has` as `reconstruct::edges` takes them, and what it costs: its SATD and its bits at `lambda`.
pub(super) fn choose_whole(
    source: Pixels<'_>,
    plane: Pixels<'_>,
    (above, has): (&[u8], (bool, bool)),
    (costs, lambda): (&ModeCosts, u32),
) -> (u8, u32) {
    let mut left = [0; MACROBLOCK];
    let edges = reconstruct::edges(plane, above, has, MACROBLOCK, &mut left);
    let mut predicted = [0u8; LUMA_PIXELS];
    let (mut chosen, mut least) = (DC_PRED, u32::MAX);
    for mode in WHOLE_MODES {
        intra::predict(mode, MACROBLOCK, &edges, &mut predicted, 0, MACROBLOCK);
        let cost = satd::<MACROBLOCK>(source, (&predicted, 0, MACROBLOCK)) + weigh(lambda, costs.y[usize::from(mode)]);
        if cost < least {
            (chosen, least) = (mode, cost);
        }
    }
    (chosen, least)
}

// The intra modes of the macroblock at `row`, `column` by its source and the source's edges as the decoder would find them: on a key frame the luma's, the subblock modes and the chroma's, in an inter frame the luma's alone; `beside` are the records left of and above it.
fn choose_intra(
    inputs: &Inputs<'_>,
    lines: &Lines,
    (row, column): (usize, usize),
    (left_record, above_record): (&Macroblock, &Macroblock),
) -> Intra {
    let (source, geometry) = (&inputs.source.planes[0][..], inputs.geometries[0]);
    let at = geometry.at(column * MACROBLOCK, row * MACROBLOCK);
    let luma = (source, at, geometry.stride);
    let (above, has) = (lines.above(column)[0], (column > 0, row > 0));
    let costs = inputs.modes;
    let (y_mode, cost) = choose_whole(luma, luma, (above, has), (costs, inputs.lambda));
    let record = Macroblock {
        reference: INTRA_FRAME,
        y_mode,
        modes: [subblock_mode(y_mode); SUBBLOCKS],
        ..Macroblock::default()
    };
    let mut best = Intra { record, cost };
    // Subblock modes pay on a key frame alone: tried in an inter frame they cost more than they saved (measured).
    if !inputs.header.is_key {
        return best;
    }
    let mut record = Macroblock { y_mode: B_PRED, ..best.record };
    let mut cost = inputs.weigh(costs.y[usize::from(B_PRED)]);
    for block in 0..SUBBLOCKS {
        let (pixels, left, corner) = reconstruct::subblock_edges(luma, above, has.0, block);
        let [above_mode, left_mode] = key_contexts(&record, left_record, above_record, block);
        let mode_costs = &KEY_SUBBLOCK_COSTS[above_mode][left_mode];
        let place = at + block / SUBBLOCKS_ACROSS * SUBBLOCK * geometry.stride + block % SUBBLOCKS_ACROSS * SUBBLOCK;
        let (mut chosen, mut least) = (0, u32::MAX);
        for (mode, &mode_cost) in mode_costs.iter().enumerate() {
            let predicted = intra::predict_subblock(mode as u8, &pixels, &left, corner);
            let offer =
                satd::<SUBBLOCK>((source, place, geometry.stride), (&predicted, 0, SUBBLOCK)) + inputs.weigh(mode_cost);
            if offer < least {
                (chosen, least) = (mode as u8, offer);
            }
        }
        record.modes[block] = chosen;
        cost += least;
        if cost >= best.cost {
            break;
        }
    }
    if cost < best.cost {
        best = Intra { record, cost };
    }
    best.record.uv_mode = choose_chroma(inputs, lines, (row, column));
    best
}

// The chroma mode of the macroblock at `row`, `column`, by the source's edges.
fn choose_chroma(inputs: &Inputs<'_>, lines: &Lines, (row, column): (usize, usize)) -> u8 {
    let geometry = inputs.geometries[1];
    let at = geometry.at(column * CHROMA_BLOCK, row * CHROMA_BLOCK);
    let (above, has) = (lines.above(column), (column > 0, row > 0));
    let mut predicted = [0u8; CHROMA_PIXELS];
    let mut left = [0; MACROBLOCK];
    let (mut chosen, mut least) = (DC_PRED, u32::MAX);
    for mode in WHOLE_MODES {
        let mut cost = inputs.weigh(inputs.modes.uv[usize::from(mode)]);
        for (source, &above) in inputs.source.planes[1..].iter().zip(&above[1..]) {
            let edges = reconstruct::edges((source, at, geometry.stride), above, has, CHROMA_BLOCK, &mut left);
            intra::predict(mode, CHROMA_BLOCK, &edges, &mut predicted, 0, CHROMA_BLOCK);
            cost += satd::<CHROMA_BLOCK>((source, at, geometry.stride), (&predicted, 0, CHROMA_BLOCK));
        }
        if cost < least {
            (chosen, least) = (mode, cost);
        }
    }
    chosen
}

// The paths down the inter modes' tree to NEARESTMV, and on to SPLITMV.
const INTER_MODE_PATHS: [Path; INTER_MODES] = {
    let mut paths = [path(&MV_REF_TREE, NEARESTMV); INTER_MODES];
    let mut index = 0;
    while index < INTER_MODES {
        paths[index] = path(&MV_REF_TREE, NEARESTMV + index as u8);
        index += 1;
    }
    paths
};

// What the inter modes cost in a context, in 256ths of a bit, from NEARESTMV to SPLITMV.
pub(super) fn inter_mode_costs(context: &Context) -> [u32; INTER_MODES] {
    INTER_MODE_PATHS.map(|path| path_cost(&path, &context.probabilities))
}

// The cheapest label of a whole-macroblock vector and what it costs in 256ths of a bit: ZEROMV, NEARESTMV or NEARMV where they name it, or NEWMV and the vector's difference from the best.
pub(super) fn label(context: &Context, modes: &[u32; INTER_MODES], mv: Mv, costs: &MvCosts) -> (u8, u32) {
    let mode = |mode: u8| (mode, modes[usize::from(mode - NEARESTMV)]);
    let (new, new_cost) = mode(NEWMV);
    [ZEROMV, NEARESTMV, NEARMV].into_iter().filter(|&label| context.mv(label) == Some(mv)).map(mode).fold(
        (new, new_cost + mv_cost(costs, mv.minus(context.best))),
        |cheapest, offer| {
            if offer.1 < cheapest.1 { offer } else { cheapest }
        },
    )
}

// The decisions of one band of macroblock rows, made in raster order, each from those before it in the band, and in an inter frame each macroblock predicted from a frame reconstructed into `planes`, the band's lines of the frame coded; the first row takes for the records above it the last frame's there.
pub(super) fn band(
    inputs: &Inputs<'_>,
    rows: Range<usize>,
    choices: &mut [Choice],
    levels: &mut [Levels],
    mut planes: [&mut [u8]; 3],
) {
    let (_, columns) = inputs.size;
    let mut lines = Lines::default();
    let mut grid = Grid::default();
    let mut residue = Residue::default();
    grid.resize(columns, rows.len() + 1);
    if rows.start > 0 && !inputs.header.is_key {
        for column in 0..columns {
            grid.set((0, column), *inputs.previous.get(rows.start - 1, column));
        }
    }
    // Where the band's lines begin in each plane, which a place in them is counted from.
    let origins: [usize; 3] = core::array::from_fn(|index| {
        let geometry = inputs.geometries[index];
        geometry.at(0, rows.start * side(index)) - geometry.border
    });
    lines.reset(inputs.geometries);
    for row in rows.clone() {
        if row > 0 {
            lines.keep(inputs.source, inputs.geometries, row - 1);
        }
        let local = row - rows.start + 1;
        for column in 0..columns {
            let index = (row - rows.start) * columns + column;
            let beside = grid.beside((local, column));
            let choice = if inputs.header.is_key {
                Choice { record: choose_intra(inputs, &lines, (row, column), beside).record, ..Choice::default() }
            } else {
                let context = grid.context(inputs.header, (local, column), LAST_FRAME);
                let mut coder = Coder {
                    source: inputs.source,
                    geometries: inputs.geometries,
                    position: (row, column),
                    steps: inputs.steps,
                    residue: &mut residue,
                    levels: &mut levels[index],
                };
                let place = |plane: usize| {
                    let geometry = inputs.geometries[plane];
                    (geometry.at(column * side(plane), row * side(plane)) - origins[plane], geometry.stride)
                };
                let [luma, u, v] = &mut planes;
                let ((luma_at, luma_stride), (chroma_at, chroma_stride)) = (place(0), place(1));
                let out = ((&mut **luma, luma_at, luma_stride), ([&mut **u, &mut **v], chroma_at, chroma_stride));
                let intra = || choose_intra(inputs, &lines, (row, column), beside);
                let mut choice = choose_inter(inputs, (row, column), (&context, beside), intra, &mut coder, out);
                if let Some(intra) = &mut choice.intra {
                    intra.uv_mode = choose_chroma(inputs, &lines, (row, column));
                }
                choice
            };
            choices[index] = choice;
            grid.set((local, column), choice.intra.unwrap_or(choice.record));
        }
    }
}

// The cheapest inter prediction of the macroblock at `row`, `column`, coded and reconstructed into `out`, its places in the luma and chroma planes of the frame coded; and where it costs much the intra modes of `intra` where they cost less still.
fn choose_inter(
    inputs: &Inputs<'_>,
    (row, column): (usize, usize),
    (context, beside): (&Context, (&Macroblock, &Macroblock)),
    intra: impl FnOnce() -> Intra,
    coder: &mut Coder<'_>,
    out: (Target<'_>, ChromaTarget<'_>),
) -> Choice {
    let geometry = inputs.geometries[0];
    let search = Search {
        source: &inputs.source.planes[0],
        luma: &inputs.last.planes[0],
        halves: inputs.halves,
        geometry,
        lambda: inputs.lambda,
        costs: inputs.mv_costs,
    };
    let (rows, columns) = inputs.size;
    let block = Block::new((row, column), (rows, columns), context.best);
    let temporal = inputs.vectors[row * columns + column];
    let starts = [Mv::default(), context.nearest, context.near, temporal];
    let found = search.refine::<MACROBLOCK>(&block, search.full::<MACROBLOCK>(&block, &starts));
    let [intra_probability, last_probability, golden_probability] = inputs.references;
    let reference_cost = |reference: u8| {
        bool_cost(intra_probability, true)
            + match reference {
                LAST_FRAME => bool_cost(last_probability, false),
                _ => bool_cost(last_probability, true) + bool_cost(golden_probability, false),
            }
    };
    // The vectors the search found and those the neighbours name, each at the cost of its cheapest label.
    let modes = inter_mode_costs(context);
    let label = |mv: Mv| label(context, &modes, mv, inputs.mv_costs).1;
    let mut best = (LAST_FRAME, found.0, u32::MAX);
    let candidates = [found.0, context.nearest, context.near, Mv::default()].map(|mv| block.clamp(mv));
    for (at, &mv) in candidates.iter().enumerate() {
        if candidates[..at].contains(&mv) {
            continue;
        }
        let bits = inputs.weigh(label(mv) + reference_cost(LAST_FRAME));
        let Some(limit) = best.2.checked_sub(bits) else {
            continue;
        };
        let offer = search.distortion::<MACROBLOCK>(&block, mv, limit) + bits;
        if offer < best.2 {
            best = (LAST_FRAME, mv, offer);
        }
    }
    if let Some(golden) = inputs.golden {
        let at = geometry.at(column * MACROBLOCK, row * MACROBLOCK);
        let bits = inputs.weigh(label(Mv::default()) + reference_cost(GOLDEN_FRAME));
        let limit = best.2.saturating_sub(bits);
        let (source, reference) = (&inputs.source.planes[0][at..], &golden.planes[0][at..]);
        let size = (MACROBLOCK, MACROBLOCK);
        let offer = simd::sad((source, geometry.stride), (reference, geometry.stride), size, limit) + bits;
        if offer < best.2 {
            best = (GOLDEN_FRAME, Mv::default(), offer);
        }
    }
    let (reference, mv, _) = best;
    let frame_of =
        |reference: u8| if reference == GOLDEN_FRAME { inputs.golden.unwrap_or(inputs.last) } else { inputs.last };
    let frame = frame_of(reference);
    let mut record = Macroblock { reference, y_mode: NEWMV, mvs: [mv; SUBBLOCKS], ..Macroblock::default() };
    let mut predicted = [0u8; LUMA_PIXELS];
    let filter = Filter::Sixtap;
    let source_at = geometry.at(column * MACROBLOCK, row * MACROBLOCK);
    let predict = |record: &Macroblock, frame: &Frame, predicted: &mut [u8; LUMA_PIXELS]| {
        let target = (&mut predicted[..], 0, MACROBLOCK);
        reconstruct::predict_luma(record, (&frame.planes[0], geometry), (row, column), filter, target);
        satd::<MACROBLOCK>((&inputs.source.planes[0], source_at, geometry.stride), (predicted, 0, MACROBLOCK))
    };
    let distortion = predict(&record, frame, &mut predicted);
    let mut cost = distortion + inputs.weigh(label(mv) + reference_cost(reference));
    if cost > SPLIT_FROM_STEPS * inputs.step && is_uneven(&search, &block, mv) {
        let (split, split_bits) = choose_split(inputs, &search, &block, (context, &modes), beside, mv);
        let mut split_predicted = [0u8; LUMA_PIXELS];
        let split_cost =
            predict(&split, inputs.last, &mut split_predicted) + inputs.weigh(split_bits + reference_cost(LAST_FRAME));
        if split_cost < cost {
            (record, predicted, cost) = (split, split_predicted, split_cost);
        }
    }
    // Reconstructed as the decoder will, in its place in the band's lines of the frame coded, its chroma from the frame of the record coded, which SPLITMV's search took the last frame for.
    let ((plane, at, stride), chroma) = out;
    reconstruct::put(&predicted, MACROBLOCK, (&mut *plane, at, stride));
    let references = (frame_of(record.reference), inputs.geometries);
    reconstruct::inter(&record, coder, ((plane, at, stride), chroma), references, (row, column), (filter, false));
    let intra = (cost > INTRA_FROM_STEPS * inputs.step)
        .then(intra)
        .filter(|intra| intra.cost + inputs.weigh(bool_cost(intra_probability, false)) < cost)
        .map(|intra| intra.record);
    Choice { record, cost, intra }
}

// Whether one quarter of the block differs at `mv` by more than UNEVEN_QUARTER times the quarters' mean.
fn is_uneven(search: &Search<'_>, block: &Block, mv: Mv) -> bool {
    let quarters: [u32; CHROMA_SUBBLOCKS] =
        core::array::from_fn(|quarter| search.distortion::<QUARTER>(&quarter_of(block, quarter), mv, u32::MAX));
    let (most, sum) = (quarters.iter().copied().max().unwrap_or(0), quarters.iter().sum::<u32>());
    let (times, over) = UNEVEN_QUARTER;
    most * CHROMA_SUBBLOCKS as u32 * over > sum * times
}

// A quarter of a macroblock's block, the quarters in raster order.
fn quarter_of(block: &Block, quarter: usize) -> Block {
    Block { x: block.x + quarter % 2 * QUARTER, y: block.y + quarter / 2 * QUARTER, ..*block }
}

// SPLITMV in quarters from the last frame, each quarter searched about the whole macroblock's vector: the record and what its mode and vectors cost in 256ths of a bit.
fn choose_split(
    inputs: &Inputs<'_>,
    search: &Search<'_>,
    block: &Block,
    (context, modes): (&Context, &[u32; INTER_MODES]),
    beside: (&Macroblock, &Macroblock),
    mv: Mv,
) -> (Macroblock, u32) {
    let mut record = Macroblock { reference: LAST_FRAME, y_mode: SPLITMV, ..Macroblock::default() };
    let layout = &MV_PARTITIONS[usize::from(MV_QUARTERS)];
    let mut previous = mv;
    for quarter in 0..CHROMA_SUBBLOCKS {
        let part = quarter_of(block, quarter);
        let starts = [mv, previous, context.best];
        let (found, _) = search.refine::<QUARTER>(&part, search.full::<QUARTER>(&part, &starts));
        for (subblock, &owner) in layout.iter().enumerate() {
            if usize::from(owner) == quarter {
                record.mvs[subblock] = found;
            }
        }
        previous = found;
    }
    let cost = modes[usize::from(SPLITMV - NEARESTMV)] + split_cost(&record, beside, context.best, inputs.mv_costs);
    (record, cost)
}

#[cfg(test)]
mod tests {
    use super::super::modes::mv_costs;
    use super::super::tables::{DEFAULT_MV_CONTEXT, MODE_CONTEXTS};
    use super::*;
    use crate::codec::picture::extend_plane;
    use alloc::vec;

    #[test]
    fn satd_of_patterns() {
        let (source, flat) = ([103u8; LUMA_PIXELS], [100u8; LUMA_PIXELS]);
        let satd16 = satd::<MACROBLOCK>((&source, 0, MACROBLOCK), (&flat, 0, MACROBLOCK));
        assert_eq!(satd16, 16 * 24, "a flat difference of 3 is each subblock's DC alone: 16 times 3, halved");
        let board: [u8; 16] = core::array::from_fn(|at| if (at % 4 + at / 4) % 2 == 0 { 101 } else { 99 });
        let satd4 = satd::<SUBBLOCK>((&board, 0, SUBBLOCK), (&flat, 0, SUBBLOCK));
        assert_eq!(satd4, 8, "a checkerboard of plus and minus one is one coefficient of 16, halved");
    }

    #[test]
    fn whole_mode_by_edges() {
        let geometry = Geometry::new(2 * MACROBLOCK, 2 * MACROBLOCK, 2 * MACROBLOCK);
        let mut plane = vec![0u8; geometry.bytes()];
        // Stripes down the right half, beside a left half that grows down its lines.
        for y in 0..geometry.height {
            for x in 0..geometry.width {
                plane[geometry.at(x, y)] = if x < MACROBLOCK {
                    (y * 7) as u8
                } else if x % 4 < 2 {
                    40
                } else {
                    200
                };
            }
        }
        extend_plane(&mut plane, geometry);
        let mut lines = Lines::default();
        let chroma = Geometry::new(MACROBLOCK, MACROBLOCK, MACROBLOCK);
        let geometries = [geometry, chroma, chroma];
        lines.reset(&geometries);
        let mut frame = Frame::default();
        frame.allocate(&geometries);
        frame.planes[0].copy_from_slice(&plane);
        lines.keep(&frame, &geometries, 0);
        let costs = ModeCosts::new(true, &Probabilities::default());
        let block = (&plane[..], geometry.at(MACROBLOCK, MACROBLOCK), geometry.stride);
        let (mode, cost) = choose_whole(block, block, (lines.above(1)[0], (true, true)), (&costs, 1));
        assert_eq!((mode, cost), (V_PRED, 0), "stripes under a line of the same stripes: V_PRED, the picture itself");
    }

    #[test]
    fn labels_by_cost() {
        let context = Context {
            best: Mv { x: 4, y: 0 },
            nearest: Mv { x: 4, y: 0 },
            near: Mv { x: 8, y: 8 },
            probabilities: core::array::from_fn(|node| MODE_CONTEXTS[2][node]),
        };
        let (modes, costs) = (inter_mode_costs(&context), mv_costs(&DEFAULT_MV_CONTEXT));
        let named =
            [Mv { x: 4, y: 0 }, Mv { x: 8, y: 8 }, Mv::default()].map(|mv| label(&context, &modes, mv, &costs).0);
        assert_eq!(named, [NEARESTMV, NEARMV, ZEROMV], "a vector the neighbours name takes their label");
        let (mode, cost) = label(&context, &modes, Mv { x: 12, y: -4 }, &costs);
        let bits = modes[usize::from(NEWMV - NEARESTMV)] + mv_cost(&costs, Mv { x: 8, y: -4 });
        assert_eq!(
            (mode, cost),
            (NEWMV, bits),
            "another is NEWMV, at its mode's cost and its difference from the best"
        );
    }
}
