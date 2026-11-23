use super::choose::{self, Choice, Inputs, ModeCosts};
use super::cost::{ONE_BIT, bool_cost, counts_cost, weigh};
use super::decode::{BORDER, dequantization, filter_level};
use super::filter::{self, Kind, Strength};
use super::frame::{CHROMA_BLOCK, MACROBLOCK, SUBBLOCKS, side};
use super::header::{
    self, COPY_NONE, Deltas, Header, LF_DELTAS, MOST_PARTITIONS, Probabilities, Quantizers, State, Tag,
};
use super::inter::Filter;
use super::intra::{ABOVE_EDGE, ABOVE_RIGHT};
use super::modes::{
    GOLDEN_FRAME, Grid, HANDED_BYTES, INTRA_FRAME, LAST_FRAME, Macroblock, Mv, MvCosts, MvCounts, Neighbours, count_mv,
    mv_costs, subblock_mode,
};
use super::quantize::{Coder, Steps};
use super::reconstruct::{self, line_bytes};
use super::search;
use super::tables::{
    AC_QLOOKUP, B_PRED, COEFF_UPDATE_PROBS, MV_UPDATE_PROBS, QINDEX_RANGE, SPLITMV, UV_MODE_TREE, YMODE_TREE,
};
use super::tokens::{self, CONTEXTS, Counts, Levels, Residue, Sink, Writer};
use crate::base::crew::{Handoff, Part, Row, Rows, Wave, Work};
use crate::codec::Flaw;
use crate::codec::boolean::{BoolEncoder, EVEN, path, probability_of};
use crate::codec::picture::{Frame, Geometry, Picture, check_size, extend_plane};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;
use core::time::Duration;

// The bytes of a frame's tokens for each token partition they are cut into, each written by itself (RFC 6386, 9.5): each partition adds its coder's end and, but the last, its size's three bytes, a small share of this.
const BYTES_PER_PARTITION: u64 = 2048;
// The rows of macroblocks a band of decisions takes; a band is decided by itself, its first row from the last frame's records above it, which costs 0.03 dB at bands of 8 rows against one band and 0.09 dB at 4 (measured).
const BAND_ROWS: usize = 8;
// The tile rows of a macroblock's height an interpolation job fills: nine jobs at 640x480, one past the chip's cores.
const INTERPOLATION_TILES: usize = 4;
// The frames after which the golden frame is refreshed by the frame coded; every 10 coded the test scenes alike (measured).
const GOLDEN_INTERVAL: u32 = 30;
// What a bit is worth in 256ths of an absolute difference, per step of the AC quantizer; 20 to 32 coded the test scenes best, 48 and 72 worse (measured).
const LAMBDA_PER_STEP: u32 = 24;
// The loop filter's level is the quantizer index over this; over 8 / 3 and over 8 the test scenes coded worse (measured).
const LEVEL_DIVISOR: i32 = 4;
// The loop filter's deltas by reference and by mode stay off: libvpx's, sent at each key frame, gained nothing on the test scenes (measured).
const DELTAS: Deltas = Deltas { is_enabled: false, reference: [0; LF_DELTAS], mode: [0; LF_DELTAS] };
// A probability update's value: eight bits for a coefficient's and a mode's, seven for a vector's (RFC 6386, 13.4, 9.10 and 17.2).
const COEFFICIENT_UPDATE_BITS: u32 = 8;
const MV_UPDATE_BITS: u32 = 7;
const MODE_UPDATE_BITS: u32 = 8;
// A row of a key frame codes a macroblock once the row above has coded the one above it and the one after, whose pixels B_PRED's right subblocks read (RFC 6386, 12.3); a row of an inter frame, whose intra macroblocks the coding predicts whole, once it has coded the one above.
const KEY_LEAD: usize = 2;
const INTER_LEAD: usize = 1;

// The AC step of a quantizer index, which a frame's bits fall with as it grows.
pub(crate) fn ac_step(quantizer: u8) -> u32 {
    AC_QLOOKUP[usize::from(quantizer).min(QINDEX_RANGE - 1)] as u32
}

// The finest quantizer index whose AC step is at least `step`; the coarsest where none is.
pub(crate) fn quantizer_of(step: u32) -> u8 {
    AC_QLOOKUP.iter().position(|&ac| ac as u32 >= step).unwrap_or(QINDEX_RANGE - 1) as u8
}

/// What the caller asks of a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
    // A key frame, which the far side decodes without what came before: after a PLI or a FIR.
    pub(crate) is_key: bool,
    // The quantizer index of 0 to 127 (RFC 6386, 9.6); past 127 it is 127.
    pub(crate) quantizer: u8,
}

// A coded frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Coded {
    // The frame's bytes; their count is what a rate controller weighs.
    pub(crate) frame: Vec<u8>,
    // A key frame: asked for, or the first of a stream or of a new size.
    pub(crate) is_key: bool,
}

/// A real-time encoder of one VP8 stream (RFC 6386), its work cut into steps a caller may run on several threads.
#[derive(Default)]
pub struct Encoder {
    width: usize,
    height: usize,
    columns: usize,
    rows: usize,
    geometries: [Geometry; 3],
    source: Frame,
    // The frame being coded, and the frames coded before it that the last and golden frames are among.
    coding: Frame,
    frames: [Frame; 2],
    last: usize,
    golden: usize,
    halves: [Vec<u8>; 3],
    has_reference: bool,
    since_golden: u32,
    header: Header,
    state: State,
    // What the loop filter's levels are taken from: the state, its deltas off.
    filtering: State,
    start: Probabilities,
    mode_costs: ModeCosts,
    mv_costs: MvCosts,
    probabilities: Probabilities,
    references: [u8; 3],
    steps: Steps,
    lambda: u32,
    grid: Grid,
    choices: Vec<Choice>,
    levels: Vec<Levels>,
    vectors: Vec<Mv>,
    strengths: Vec<Strength>,
    above: Vec<[u8; CONTEXTS]>,
    handed: Handed,
    tallies: Vec<Tally>,
    partitions: [Vec<u8>; MOST_PARTITIONS],
}

// What the rows of a step that runs them at once hand to the rows below them, each row its own: the coding's last line of each plane as `Lines` keeps it, the tokens' contexts and the records below each macroblock, and the loop filter's last luma lines.
#[derive(Default)]
struct Handed {
    lines: [Handoff; 3],
    contexts: Handoff,
    records: Handoff,
    edges: Handoff,
}

impl Handed {
    fn resize(&mut self, geometries: &[Geometry; 3], (rows, columns): (usize, usize)) {
        for (plane, (lines, &geometry)) in self.lines.iter_mut().zip(geometries).enumerate() {
            lines.resize(rows * Handoff::room(line_bytes(plane, geometry)));
        }
        self.contexts.resize(rows * Handoff::room(columns * CONTEXTS));
        self.records.resize(rows * Handoff::room(columns * HANDED_BYTES));
        self.edges.resize(filter::handed_bytes(rows, geometries[0].width));
    }
}

// What a band of rows coded counted: its tokens' branches, its skipped macroblocks, and in an inter frame its macroblocks' references and intra modes and its vectors' bits.
#[derive(Default)]
struct Tally {
    counts: Counts,
    skips: [u32; 2],
    intras: [u32; 2],
    lasts: [u32; 2],
    goldens: [u32; 2],
    y_modes: [[u32; 2]; 4],
    uv_modes: [[u32; 2]; 3],
    mvs: MvCounts,
}

impl Tally {
    // Counts what `record` codes beside its tokens, its neighbours and place as `Neighbours::differences` takes them.
    fn count(&mut self, header: &Header, record: &Macroblock, (neighbours, position): Placed<'_>) {
        self.skips[usize::from(record.skip)] += 1;
        if header.is_key {
            return;
        }
        self.intras[usize::from(record.reference != INTRA_FRAME)] += 1;
        if record.reference == INTRA_FRAME {
            let (steps, depth) = path(&YMODE_TREE, record.y_mode);
            steps[..depth].iter().for_each(|&(node, bit)| self.y_modes[node / 2][usize::from(bit)] += 1);
            let (steps, depth) = path(&UV_MODE_TREE, record.uv_mode);
            steps[..depth].iter().for_each(|&(node, bit)| self.uv_modes[node / 2][usize::from(bit)] += 1);
            return;
        }
        self.lasts[usize::from(record.reference != LAST_FRAME)] += 1;
        if record.reference != LAST_FRAME {
            self.goldens[usize::from(record.reference != GOLDEN_FRAME)] += 1;
        }
        neighbours.differences(header, record, position, |difference| count_mv(&mut self.mvs, difference));
    }

    // Adds what another band counted.
    fn add(&mut self, other: &Tally) {
        let counts = self.counts.as_flattened_mut().as_flattened_mut().as_flattened_mut().as_flattened_mut();
        let others = other.counts.as_flattened().as_flattened().as_flattened().as_flattened();
        counts.iter_mut().zip(others).for_each(|(sum, other)| *sum += other);
        let sums = (self.mvs.iter_mut().flatten()).chain(&mut self.y_modes).chain(&mut self.uv_modes).chain([
            &mut self.skips,
            &mut self.intras,
            &mut self.lasts,
            &mut self.goldens,
        ]);
        let others = (other.mvs.iter().flatten()).chain(&other.y_modes).chain(&other.uv_modes).chain([
            &other.skips,
            &other.intras,
            &other.lasts,
            &other.goldens,
        ]);
        for (sum, other) in sums.zip(others) {
            sum[0] += other[0];
            sum[1] += other[1];
        }
    }
}

// A macroblock's neighbours, and its place and the frame's size, as `Neighbours::context` takes them.
type Placed<'a> = (&'a Neighbours<'a>, ((usize, usize), (usize, usize)));

// What the coding left of its macroblocks that their tokens are walked by: the records, the levels, and the contexts above each.
type Tokens<'a> = (&'a Grid, &'a [Levels], &'a [[u8; CONTEXTS]]);

// The tokens of rows of `columns` macroblocks into `sink`, each row in the contexts the coding left above it.
fn write_rows<S: Sink>(
    sink: &mut S,
    (grid, levels, above): Tokens<'_>,
    rows: impl Iterator<Item = usize>,
    columns: usize,
) {
    let mut contexts = vec![[0; CONTEXTS]; columns];
    for row in rows {
        contexts.copy_from_slice(&above[row * columns..(row + 1) * columns]);
        let mut left = [0; CONTEXTS];
        for (column, above) in contexts.iter_mut().enumerate() {
            let record = grid.get(row, column);
            let has_y2 = record.y_mode != B_PRED && record.y_mode != SPLITMV;
            if record.skip {
                tokens::skip(has_y2, (&mut left, above));
            } else {
                tokens::write_macroblock(sink, has_y2, &levels[row * columns + column], (&mut left, above));
            }
        }
    }
}

// Counts what a band of rows coded: its tokens and its macroblocks' kinds, modes and vectors.
struct Count<'a> {
    rows: Range<usize>,
    header: &'a Header,
    coded: Tokens<'a>,
    size: (usize, usize),
    tally: &'a mut Tally,
}

impl Count<'_> {
    fn run(self) {
        let Count { rows, header, coded, size, tally } = self;
        *tally = Tally::default();
        write_rows(&mut tally.counts, coded, rows.clone(), size.1);
        for (row, column) in rows.flat_map(|row| (0..size.1).map(move |column| (row, column))) {
            let neighbours = coded.0.neighbours_of((row, column));
            tally.count(header, coded.0.get(row, column), (&neighbours, ((row, column), size)));
        }
    }
}

// Writes one token partition: the rows of macroblocks it holds.
struct Partition<'a> {
    index: usize,
    count: usize,
    coded: Tokens<'a>,
    probabilities: &'a Probabilities,
    size: (usize, usize),
    out: &'a mut Vec<u8>,
}

impl Partition<'_> {
    fn run(self) {
        let (rows, columns) = self.size;
        let mut encoder = BoolEncoder::new();
        let mut writer = Writer { encoder: &mut encoder, probabilities: &self.probabilities.coeff };
        write_rows(&mut writer, self.coded, (self.index..rows).step_by(self.count), columns);
        *self.out = encoder.finish();
    }
}

// Filters a chroma plane of the frame just coded and repeats its edges into its border.
struct Chroma<'a> {
    plane: &'a mut [u8],
    geometry: Geometry,
    strengths: &'a [Strength],
    columns: usize,
    level: i32,
    kind: Kind,
}

impl Chroma<'_> {
    fn run(self) {
        if self.level > 0 {
            for (row, strengths) in self.strengths.chunks(self.columns).enumerate() {
                filter::filter_plane_row((&mut *self.plane, self.geometry), true, row, strengths, self.kind);
            }
        }
        extend_plane(self.plane, self.geometry);
    }
}

// What every row of the coding reads: the frame's settings, its source, its mode and vector costs, and what the rows hand over.
#[derive(Clone, Copy)]
struct Coding<'a> {
    header: &'a Header,
    filtering: &'a State,
    source: &'a Frame,
    geometries: &'a [Geometry; 3],
    steps: &'a Steps,
    costs: (&'a ModeCosts, &'a MvCosts),
    // What a bit is worth, and what the intra flag of an inter frame costs at that worth.
    lambda: u32,
    intra_cost: u32,
    size: (usize, usize),
    handed: &'a Handed,
}

// A row of macroblocks the coding codes in order, at the same time as the rows above and below it: what the row above handed it, its decisions, and what it fills in, its macroblocks' records behind the border's.
struct CodeRow<'a> {
    row: usize,
    coding: Coding<'a>,
    choices: &'a [Choice],
    levels: &'a mut [Levels],
    records: &'a mut [Macroblock],
    // The row's lines of each plane of the frame coded.
    planes: [&'a mut [u8]; 3],
    // The tokens' contexts above each macroblock, as the row above left them.
    above: &'a mut [[u8; CONTEXTS]],
    strengths: &'a mut [Strength],
}

impl CodeRow<'_> {
    // Codes the row's macroblocks in order, those the band predicted within the frame or whose intra modes cost less than its choice by the edges the coding reconstructs reconstructed by the decoder's own code, each once the row above has handed over what it reads, and hands on what the row below reads.
    fn run(self, rows: &dyn Rows) {
        let CodeRow { row, coding, choices, levels, records, mut planes, above, strengths } = self;
        let Coding { header, filtering, source, geometries, steps, costs, lambda, intra_cost, size, handed } = coding;
        let ([luma, chroma, _], columns) = (*geometries, size.1);
        let line_at = |row: usize, plane: usize| row * Handoff::room(line_bytes(plane, geometries[plane]));
        let (context_at, record_at) = (
            |row: usize, column: usize| row * Handoff::room(columns * CONTEXTS) + column * CONTEXTS,
            |row: usize, column: usize| row * Handoff::room(columns * HANDED_BYTES) + column * HANDED_BYTES,
        );
        if row == 0 {
            above.fill([0; CONTEXTS]);
        }
        let mut residue = Residue::default();
        let mut left = [0; CONTEXTS];
        // The line above the macroblock in each plane from the pixel left of its first on, as `Lines` keeps it: the frame's top edge above the first row.
        let mut luma_above = [ABOVE_EDGE; 1 + MACROBLOCK + ABOVE_RIGHT];
        let mut chroma_above = [[ABOVE_EDGE; 1 + CHROMA_BLOCK]; 2];
        let reach = if header.is_key { luma_above.len() } else { 1 + MACROBLOCK };
        let (mut above_record, mut above_left) = (Macroblock::default(), Macroblock::default());
        for column in 0..columns {
            rows.wait(row, column);
            if let Some(up) = row.checked_sub(1) {
                handed.lines[0].take(line_at(up, 0) + column * MACROBLOCK, &mut luma_above[..reach]);
                for (index, line) in chroma_above.iter_mut().enumerate() {
                    handed.lines[1 + index].take(line_at(up, 1 + index) + column * CHROMA_BLOCK, line);
                }
                handed.contexts.take(context_at(up, column), &mut above[column]);
                if !header.is_key {
                    let mut bytes = [0; HANDED_BYTES];
                    handed.records.take(record_at(up, column), &mut bytes);
                    above_left = core::mem::replace(&mut above_record, Macroblock::from_handed(&bytes));
                }
            }
            let has = (column > 0, row > 0);
            let (luma_at, chroma_at) = (luma.border + column * MACROBLOCK, chroma.border + column * CHROMA_BLOCK);
            let choice = choices[column];
            let mut record = choice.record;
            if let Some(intra) = choice.intra {
                let from = (&source.planes[0][..], luma.at(column * MACROBLOCK, row * MACROBLOCK), luma.stride);
                let edges = (&planes[0][..], luma_at, luma.stride);
                let (y_mode, cost) = choose::choose_whole(from, edges, (&luma_above, has), (costs.0, lambda));
                if cost + intra_cost < choice.cost {
                    record = Macroblock { y_mode, modes: [subblock_mode(y_mode); SUBBLOCKS], ..intra };
                }
            }
            let neighbours = Neighbours { above: &above_record, left: &records[column], above_left: &above_left };
            let position = ((row, column), size);
            if record.reference == INTRA_FRAME {
                let levels = &mut levels[column];
                let mut coder =
                    Coder { source, geometries, position: (row, column), steps, residue: &mut residue, levels };
                let [y, u, v] = &mut planes;
                let targets = ((&mut **y, luma_at, luma.stride), ([&mut **u, &mut **v], chroma_at, chroma.stride));
                reconstruct::intra(
                    &record,
                    &mut coder,
                    targets,
                    [&luma_above, &chroma_above[0], &chroma_above[1]],
                    has,
                );
            } else if record.y_mode != SPLITMV {
                let context = neighbours.context(header, position, record.reference);
                let modes = choose::inter_mode_costs(&context);
                record.y_mode = choose::label(&context, &modes, record.mvs[0], costs.1).0;
            }
            let has_y2 = record.y_mode != B_PRED && record.y_mode != SPLITMV;
            let is_coded = levels[column].is_coded(has_y2);
            record.skip = !is_coded;
            let mut context = above[column];
            if is_coded {
                tokens::contexts(has_y2, &levels[column], (&mut left, &mut context));
            } else {
                tokens::skip(has_y2, (&mut left, &mut context));
            }
            let level = filter_level(header, filtering, &record);
            strengths[column] = Strength::new(level, header.sharpness, header.is_key, is_coded || !has_y2);
            records[column + 1] = record;
            for (plane, geometry) in [luma, chroma, chroma].into_iter().enumerate() {
                let side = side(plane);
                let last = (side - 1) * geometry.stride + geometry.border + column * side;
                let pixels = &planes[plane][last..last + side];
                reconstruct::hand_line(&handed.lines[plane], line_at(row, plane), pixels, (column, columns), plane);
            }
            handed.contexts.put(context_at(row, column), &context);
            if !header.is_key {
                handed.records.put(record_at(row, column), &record.to_handed());
            }
            rows.note(row, column + 1);
        }
    }
}

// A plane's lines from its first visible one, `lines` at a time.
fn lines_of(plane: &mut [u8], geometry: Geometry, lines: usize) -> core::slice::ChunksMut<'_, u8> {
    plane[geometry.border * geometry.stride..].chunks_mut(lines * geometry.stride)
}

// The probability a bool of `counts` is coded with: `kept`, or `wanted` where it saves more than updating to it costs, the update's flag at `update` and its `bits` of value.
fn updated(kept: u8, wanted: u8, counts: [u32; 2], (update, bits): (u8, u32)) -> u8 {
    let price = bits * ONE_BIT + bool_cost(update, true);
    if counts_cost(kept, counts) + bool_cost(update, false) > counts_cost(wanted, counts) + price {
        wanted
    } else {
        kept
    }
}

// The probabilities of a tree's nodes that fit how often each branch was taken, where they save more than `bits` costs.
fn tree_update<const NODES: usize>(kept: [u8; NODES], counts: &[[u32; 2]; NODES], bits: u32) -> [u8; NODES] {
    let wanted: [u8; NODES] = core::array::from_fn(|node| probability_of(counts[node]));
    let cost = |probabilities: &[u8; NODES]| -> u32 {
        probabilities.iter().zip(counts).map(|(&probability, &counts)| counts_cost(probability, counts)).sum()
    };
    if cost(&kept) > cost(&wanted) + bits * ONE_BIT { wanted } else { kept }
}

/// A step of a frame's coding, in the order the steps run: jobs that may run at once, the code's rows a wave, the probabilities one job, and the ends' luma rows a wave beside their other jobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Interpolations,
    Bands,
    Code,
    Counts,
    Probabilities,
    Ends,
}

impl Step {
    // The steps in the order they run and are declared.
    const ALL: [Step; 6] =
        [Step::Interpolations, Step::Bands, Step::Code, Step::Counts, Step::Probabilities, Step::Ends];

    // Its name, as a line tells it.
    const fn name(self) -> &'static str {
        match self {
            Step::Interpolations => "interpolations",
            Step::Bands => "bands",
            Step::Code => "code",
            Step::Counts => "counts",
            Step::Probabilities => "probabilities",
            Step::Ends => "ends",
        }
    }
}

// Each step stands in `Step::ALL` at the place it is declared at, so that a step is its own place there.
const _: () = {
    let mut at = 0;
    while at < Step::ALL.len() {
        assert!(Step::ALL[at] as usize == at, "a step out of its place in `Step::ALL`");
        at += 1;
    }
};

/// What each step of codings took in all, and the longest coding of a picture, a key frame's among them.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Laps {
    times: [Duration; Step::ALL.len()],
    // The steps of the picture under way so far.
    picture: Duration,
    pub(crate) longest: Duration,
}

impl Laps {
    /// Adds the time a step took; the last step ends a picture.
    pub fn add(&mut self, step: Step, time: Duration) {
        self.times[step as usize] += time;
        self.picture += time;
        if Step::ALL.last() == Some(&step) {
            (self.longest, self.picture) = (self.longest.max(self.picture), Duration::ZERO);
        }
    }

    // Each step's time on the mean over `pictures` codings, as a line tells it.
    pub(crate) fn to_text(self, pictures: u64) -> String {
        let each = |time: Duration| time.as_micros() as u64 / pictures.max(1);
        let steps: Vec<String> =
            Step::ALL.iter().zip(self.times).map(|(step, time)| format!("{} {} us", step.name(), each(time))).collect();
        steps.join(", ")
    }
}

impl Encoder {
    // Codes one picture in one go, the steps run in order on the caller's thread.
    #[cfg(test)]
    fn encode(&mut self, picture: &Picture<'_>, request: Request) -> Result<Coded, Flaw> {
        self.begin(picture, request)?;
        self.code_steps(|_, work| work.in_order());
        Ok(self.finish())
    }

    /// Codes the frame begun by its steps in order, each step's work handed to `run`, which may run its parts at once.
    pub fn code_steps(&mut self, mut run: impl FnMut(Step, Work<'_>)) {
        run(Step::Interpolations, Work::Parts(self.interpolations()));
        run(Step::Bands, Work::Parts(self.bands()));
        run(Step::Code, Work::Wave(self.code_rows()));
        run(Step::Counts, Work::Parts(self.counts()));
        run(Step::Probabilities, Work::Parts(vec![Box::new(|| self.settle()) as Part<'_>]));
        run(Step::Ends, Work::Wave(self.ends()));
    }

    // The bands of rows whose coding is counted at once, each into a tally of its own.
    fn counts(&mut self) -> Vec<Part<'_>> {
        let coded = (&self.grid, &self.levels[..], &self.above[..]);
        let (header, size) = (&self.header, (self.rows, self.columns));
        let bands = (0..self.rows).step_by(BAND_ROWS).map(|first| first..(first + BAND_ROWS).min(self.rows));
        bands
            .zip(&mut self.tallies)
            .map(|(rows, tally)| {
                let count = Count { rows, header, coded, size, tally };
                Box::new(move || count.run()) as Part<'_>
            })
            .collect()
    }

    fn resize(&mut self, (width, height): (usize, usize)) {
        let (columns, rows) = (width.div_ceil(MACROBLOCK), height.div_ceil(MACROBLOCK));
        (self.width, self.height, self.columns, self.rows) = (width, height, columns, rows);
        let luma = Geometry::new(columns * MACROBLOCK, rows * MACROBLOCK, BORDER);
        let chroma = Geometry::new(columns * CHROMA_BLOCK, rows * CHROMA_BLOCK, BORDER / 2);
        self.geometries = [luma, chroma, chroma];
        for frame in self.frames.iter_mut().chain([&mut self.coding, &mut self.source]) {
            frame.allocate(&self.geometries);
        }
        (self.last, self.golden) = (0, 0);
        for plane in &mut self.halves {
            plane.clear();
            plane.resize(luma.bytes(), 0);
        }
        let count = columns * rows;
        self.grid.resize(columns, rows);
        self.choices = vec![Choice::default(); count];
        self.levels.clear();
        self.levels.resize_with(count, Levels::default);
        self.vectors = vec![Mv::default(); count];
        self.strengths = vec![Strength::default(); count];
        self.above = vec![[0; CONTEXTS]; count];
        self.handed.resize(&self.geometries, (rows, columns));
        self.tallies.clear();
        self.tallies.resize_with(rows.div_ceil(BAND_ROWS), Tally::default);
        self.has_reference = false;
    }

    // Copies the picture's visible pixels, repeating the last of each line and the last line out to whole macroblocks.
    fn copy(&mut self, picture: &Picture<'_>) {
        let sizes = [(picture.width, picture.height), (picture.width.div_ceil(2), picture.height.div_ceil(2))];
        for (index, plane) in self.source.planes.iter_mut().enumerate() {
            let (geometry, (width, height)) = (self.geometries[index], sizes[usize::from(index > 0)]);
            let stride = picture.strides[index];
            for y in 0..geometry.height {
                let from = &picture.planes[index][y.min(height - 1) * stride..][..width];
                let start = geometry.at(0, y);
                let line = &mut plane[start..start + geometry.width];
                line[..width].copy_from_slice(from);
                line[width..].fill(from[width - 1]);
            }
        }
    }

    // Starts a frame of `picture`: checks and copies it, and settles the frame's header.
    pub(crate) fn begin(&mut self, picture: &Picture<'_>, request: Request) -> Result<(), Flaw> {
        let (width, height) = (picture.width, picture.height);
        if width == 0 || height == 0 {
            return Err(Flaw::Invalid("picture size"));
        }
        check_size(width, height)?;
        let sizes = [(width, height), (width.div_ceil(2), height.div_ceil(2)), (width.div_ceil(2), height.div_ceil(2))];
        for ((plane, &stride), (width, height)) in picture.planes.iter().zip(&picture.strides).zip(sizes) {
            if stride < width || plane.len() < (height - 1) * stride + width {
                return Err(Flaw::Short);
            }
        }
        if (width, height) != (self.width, self.height) {
            self.resize((width, height));
        }
        self.copy(picture);
        let is_key = request.is_key || !self.has_reference;
        let quantizer = i32::from(request.quantizer).min(QINDEX_RANGE as i32 - 1);
        let refresh_golden = is_key || self.since_golden + 1 >= GOLDEN_INTERVAL;
        self.header = Header {
            is_key,
            filter: Filter::Sixtap,
            is_full_pixel: false,
            kind: Kind::Normal,
            level: quantizer / LEVEL_DIVISOR,
            sharpness: 0,
            // One until the coding has counted the tokens, which settles how many.
            partitions: 1,
            quantizers: Quantizers { index: quantizer, ..Quantizers::default() },
            refresh_golden,
            refresh_altref: is_key,
            copy_to_golden: COPY_NONE,
            copy_to_altref: COPY_NONE,
            sign_bias_golden: false,
            sign_bias_altref: false,
            refresh_entropy: false,
            refresh_last: true,
            skip_prob: None,
            prob_intra: EVEN,
            prob_last: EVEN,
            prob_golden: EVEN,
        };
        if is_key {
            self.references = [EVEN; 3];
        }
        self.start = if is_key { Probabilities::default() } else { self.state.probabilities };
        self.mode_costs = ModeCosts::new(is_key, &self.start);
        self.mv_costs = mv_costs(&self.start.mv);
        self.steps = Steps::new(&dequantization(&self.header, &self.state)[0]);
        self.filtering = State { deltas: DELTAS, ..self.state };
        self.lambda = ac_step(request.quantizer) * LAMBDA_PER_STEP;
        Ok(())
    }

    // The jobs that fill the half-pel planes of the last frame, before the bands; none for a key frame.
    fn interpolations(&mut self) -> Vec<Part<'_>> {
        if self.header.is_key {
            return Vec::new();
        }
        let geometry = self.geometries[0];
        let job = INTERPOLATION_TILES * MACROBLOCK * geometry.stride;
        let reference = (&self.frames[self.last].planes[0][..], geometry);
        let [across, down, both] = &mut self.halves;
        let jobs = across.chunks_mut(job).zip(down.chunks_mut(job)).zip(both.chunks_mut(job));
        jobs.enumerate()
            .map(|(index, ((across, down), both))| {
                let first = index * INTERPOLATION_TILES;
                let tiles = first..first + across.len() / (MACROBLOCK * geometry.stride);
                Box::new(move || search::interpolate(reference, tiles, [across, down, both])) as Part<'_>
            })
            .collect()
    }

    // The bands of macroblock rows whose decisions may be made at once, each by itself: the motion search, the modes, and the residue of those predicted from a frame, reconstructed into the band's lines of the frame coded.
    fn bands(&mut self) -> Vec<Part<'_>> {
        let golden = (self.golden != self.last).then(|| &self.frames[self.golden]);
        let inputs = Inputs {
            source: &self.source,
            geometries: &self.geometries,
            last: &self.frames[self.last],
            golden,
            halves: [&self.halves[0], &self.halves[1], &self.halves[2]],
            header: &self.header,
            modes: &self.mode_costs,
            mv_costs: &self.mv_costs,
            steps: &self.steps,
            lambda: self.lambda,
            step: ac_step(self.header.quantizers.index as u8),
            references: self.references,
            vectors: &self.vectors,
            previous: &self.grid,
            size: (self.rows, self.columns),
        };
        let (columns, band) = (self.columns, BAND_ROWS * self.columns);
        let [luma, u, v] = &mut self.coding.planes;
        let [luma_geometry, u_geometry, v_geometry] = self.geometries;
        let planes = lines_of(luma, luma_geometry, BAND_ROWS * MACROBLOCK)
            .zip(lines_of(u, u_geometry, BAND_ROWS * CHROMA_BLOCK))
            .zip(lines_of(v, v_geometry, BAND_ROWS * CHROMA_BLOCK));
        let parts = self.choices.chunks_mut(band).zip(self.levels.chunks_mut(band)).zip(planes);
        parts
            .enumerate()
            .map(|(index, ((choices, levels), ((luma, u), v)))| {
                let first = index * BAND_ROWS;
                let rows = first..first + choices.len() / columns;
                Box::new(move || choose::band(&inputs, rows, choices, levels, [luma, u, v])) as Part<'_>
            })
            .collect()
    }

    // The rows of macroblocks the coding codes at once, each from what the row above hands it.
    fn code_rows(&mut self) -> Wave<'_> {
        let Encoder {
            header, filtering, grid, choices, levels, coding, geometries, source, steps, columns, rows, ..
        } = self;
        let Encoder { mode_costs, mv_costs, lambda, references, above, strengths, handed, .. } = self;
        let frame = Coding {
            header,
            filtering,
            source,
            geometries,
            steps,
            costs: (mode_costs, mv_costs),
            lambda: *lambda,
            intra_cost: weigh(*lambda, bool_cost(references[0], false)),
            size: (*rows, *columns),
            handed,
        };
        let [luma, u, v] = &mut coding.planes;
        let planes = lines_of(luma, geometries[0], MACROBLOCK)
            .zip(lines_of(u, geometries[1], CHROMA_BLOCK))
            .zip(lines_of(v, geometries[2], CHROMA_BLOCK));
        let parts = (choices.chunks(*columns).zip(levels.chunks_mut(*columns)).zip(grid.rows_mut()))
            .zip(planes.zip(above.chunks_mut(*columns)))
            .zip(strengths.chunks_mut(*columns));
        let rows = parts
            .enumerate()
            .map(|(row, ((((choices, levels), records), (((luma, u), v), above)), strengths))| {
                let planes = [luma, u, v];
                let code = CodeRow { row, coding: frame, choices, levels, records, planes, above, strengths };
                Box::new(move |rows: &dyn Rows| code.run(rows)) as Row<'_>
            })
            .collect();
        let lead = if header.is_key { KEY_LEAD } else { INTER_LEAD };
        Wave { rows, places: *columns, lead, beside: Vec::new() }
    }

    // Settles the frame's probabilities from what the bands of its coding counted, and the token partitions it is cut into.
    fn settle(&mut self) {
        let mut tally = Tally::default();
        self.tallies.iter().for_each(|band| tally.add(band));
        let Tally { counts, skips, intras, lasts, goldens, y_modes, uv_modes, mvs } = tally;
        let header = &mut self.header;
        let kept = self.start;
        let mut probabilities = kept;
        let coefficients = probabilities.coeff.iter_mut().flatten().flatten().flatten();
        let counted = counts.iter().flatten().flatten().flatten();
        for ((probability, &counts), &update) in
            coefficients.zip(counted).zip(COEFF_UPDATE_PROBS.iter().flatten().flatten().flatten())
        {
            *probability = updated(*probability, probability_of(counts), counts, (update, COEFFICIENT_UPDATE_BITS));
        }
        // The tokens' bytes at the probabilities they are written with, their signs and extra bits left out.
        let coefficients = probabilities.coeff.iter().flatten().flatten().flatten();
        let costs = coefficients.zip(counts.iter().flatten().flatten().flatten());
        let bits = costs.map(|(&probability, &counts)| u64::from(counts_cost(probability, counts))).sum::<u64>();
        let wanted = (bits / u64::from(ONE_BIT * u8::BITS)).div_ceil(BYTES_PER_PARTITION) as usize;
        header.partitions = 1 << wanted.clamp(1, MOST_PARTITIONS).min(self.rows).ilog2();
        if !header.is_key {
            probabilities.y_mode = tree_update(kept.y_mode, &y_modes, 4 * MODE_UPDATE_BITS);
            probabilities.uv_mode = tree_update(kept.uv_mode, &uv_modes, 3 * MODE_UPDATE_BITS);
            for ((component, counts), updates) in probabilities.mv.iter_mut().zip(&mvs).zip(&MV_UPDATE_PROBS) {
                for ((probability, &counts), &update) in component.iter_mut().zip(counts).zip(updates) {
                    // A vector's probability is sent in its seven high bits, nought standing for 1 (RFC 6386, 17.2).
                    let wanted = header::mv_probability(probability_of(counts) >> 1);
                    *probability = updated(*probability, wanted, counts, (update, MV_UPDATE_BITS));
                }
            }
            (header.prob_intra, header.prob_last, header.prob_golden) =
                (probability_of(intras), probability_of(lasts), probability_of(goldens));
            self.references = [header.prob_intra, header.prob_last, header.prob_golden];
        }
        header.skip_prob = Some(probability_of(skips));
        self.probabilities = probabilities;
    }

    // The work that ends a frame: the luma's loop filter row by row at once, the luma's edges handed between its rows, and beside it the token partitions and each chroma plane's loop filter.
    fn ends(&mut self) -> Wave<'_> {
        let Encoder { header, partitions, coding, geometries, handed, .. } = self;
        let coded = (&self.grid, &self.levels[..], &self.above[..]);
        let (probabilities, strengths) = (&self.probabilities, &self.strengths);
        let (count, size, columns) = (header.partitions, (self.rows, self.columns), self.columns);
        let partitions = partitions[..count].iter_mut().enumerate().map(|(index, out)| {
            let partition = Partition { index, count, coded, probabilities, size, out };
            Box::new(move || partition.run()) as Part<'_>
        });
        let [luma, u, v] = &mut coding.planes;
        let (level, kind, [geometry, chroma_geometry, _]) = (header.level, header.kind, *geometries);
        let chroma = [u, v].into_iter().map(|plane| {
            let chroma = Chroma { plane, geometry: chroma_geometry, strengths, columns, level, kind };
            Box::new(move || chroma.run()) as Part<'_>
        });
        let beside = partitions.chain(chroma).collect();
        let edges = &handed.edges;
        let rows = if level > 0 {
            let rows = lines_of(luma, geometry, MACROBLOCK).zip(strengths.chunks(columns)).enumerate();
            rows.map(|(row, (pixels, strengths))| {
                Box::new(move |rows: &dyn Rows| {
                    filter::filter_luma_row((pixels, geometry), row, (strengths, kind), (edges, rows));
                }) as Row<'_>
            })
            .collect()
        } else {
            Vec::new()
        };
        Wave { rows, places: columns, lead: filter::LUMA_LEAD, beside }
    }

    // Ends the frame: its luma's edges the rows handed between them put back and its border, its first partition, its bytes, and the references the next frame is predicted from.
    pub(crate) fn finish(&mut self) -> Coded {
        let (luma, geometry) = (&mut self.coding.planes[0], self.geometries[0]);
        if self.header.level > 0 {
            filter::put_back(luma, geometry, self.rows, &self.handed.edges);
        }
        extend_plane(luma, geometry);
        let mut encoder = BoolEncoder::new();
        header::write(&mut encoder, &self.header, &mut self.state, &self.probabilities, &DELTAS);
        for row in 0..self.rows {
            for column in 0..self.columns {
                self.grid.write(&mut encoder, &self.header, &self.state, (row, column));
            }
        }
        let first = encoder.finish();
        let count = self.header.partitions;
        let mut frame = Vec::with_capacity(first.len() + self.partitions[..count].iter().map(Vec::len).sum::<usize>());
        let tag = Tag { is_key: self.header.is_key, version: 0, is_shown: true, first_size: first.len() };
        header::write_tag(tag, (self.width, self.height), &mut frame);
        frame.extend_from_slice(&first);
        header::write_partitions(&self.partitions[..count], &mut frame);
        if let Some(saved) = self.state.saved.take() {
            self.state.probabilities = saved;
        }
        for (index, vector) in self.vectors.iter_mut().enumerate() {
            let record = self.grid.get(index / self.columns, index % self.columns);
            *vector = if record.reference == LAST_FRAME { record.mvs[0] } else { Mv::default() };
        }
        self.since_golden = if self.header.refresh_golden { 0 } else { self.since_golden + 1 };
        // The frame coded is the last frame from now on, and the golden one where refreshed: it takes the place of a frame no longer read, which is coded into next.
        let other = (self.last + 1) % self.frames.len();
        let place = if self.header.refresh_golden || self.last != self.golden { self.last } else { other };
        core::mem::swap(&mut self.coding, &mut self.frames[place]);
        self.last = place;
        if self.header.refresh_golden {
            self.golden = place;
        }
        self.has_reference = true;
        Coded { frame, is_key: self.header.is_key }
    }

    // The picture the frame coded last reconstructs, as a decoder shows it.
    #[cfg(test)]
    fn reconstruction(&self) -> Picture<'_> {
        self.frames[self.last].picture(&self.geometries, (self.width, self.height))
    }
}

#[cfg(test)]
mod tests {
    use super::super::Decoder;
    use super::*;
    use crate::base::checksum::crc32;
    use crate::base::crew::on_threads;
    use crate::base::math::{log2_f64, xorshift32};
    use crate::codec::picture::samples::i420;

    // The CRC32 of each size's frames in `reconstruction_decodes_exactly`, as first coded.
    const PINNED_CRCS: [u32; 6] = [0xE6BB_92AF, 0xECAE_E414, 0xD878_6E5F, 0xC7AC_D982, 0x6118_369F, 0x00B9_50A0];
    // The scene of `quality_bounds` measured: its mean luma PSNR and its bytes; bounds a tenth of a dB and 5 % off.
    const MEASURED_PSNR: f64 = 36.058;
    const PSNR_MARGIN: f64 = 0.1;
    const MEASURED_BYTES: usize = 7887;

    // The planes of frame `index` of a scene made by a formula: a gradient, a textured disc that moves across it, and noise from a fixed seed, which a camera's picture has.
    fn scene((width, height): (usize, usize), index: usize, noise: u32) -> [Vec<u8>; 3] {
        let mut state = 0x2545_F491 ^ index as u32;
        let mut grain = || {
            state = xorshift32(state);
            if noise == 0 { 0 } else { (state % (2 * noise + 1)) as i32 - noise as i32 }
        };
        let (center_x, center_y) = ((width * (8 + 3 * index) / 32) as i32, (height * (12 + index) / 32) as i32);
        let radius = (width.min(height) / 3) as i32;
        let mut luma = vec![0u8; width * height];
        for (at, pixel) in luma.iter_mut().enumerate() {
            let (x, y) = ((at % width) as i32, (at / width) as i32);
            let (dx, dy) = (x - center_x, y - center_y);
            let base = 40 + x * 120 / width as i32 + y * 60 / height as i32;
            let disc = if dx * dx + dy * dy < radius * radius { 50 + ((dx ^ dy) & 15) * 6 } else { 0 };
            *pixel = (base + disc + grain()).clamp(0, 255) as u8;
        }
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let chroma = |tilt: i32| {
            (0..chroma_width * chroma_height)
                .map(|at| (128 + tilt * ((at % chroma_width) as i32 - (at / chroma_width) as i32) / 8) as u8)
                .collect()
        };
        [luma, chroma(3), chroma(-2)]
    }

    fn picture(planes: &[Vec<u8>; 3], (width, height): (usize, usize)) -> Picture<'_> {
        let chroma = width.div_ceil(2);
        Picture { width, height, planes: [&planes[0], &planes[1], &planes[2]], strides: [width, chroma, chroma] }
    }

    fn psnr(source: &[u8], reconstruction: &[u8]) -> f64 {
        let error: u64 = source.iter().zip(reconstruction).map(|(&a, &b)| u64::from(a.abs_diff(b)).pow(2)).sum();
        let mse = error as f64 / source.len() as f64;
        if mse == 0.0 { f64::INFINITY } else { 10.0 * log2_f64(255.0 * 255.0 / mse) / log2_f64(10.0) }
    }

    // Codes frames of the scene at each size with the quantizers given, key frames where asked, each frame as the decoder shows it being the encoder's reconstruction; returns the CRC32 of all the frames' bytes.
    fn run(size: (usize, usize), quantizers: &[u8], keys: &[usize], noise: u32) -> u32 {
        let (mut encoder, mut decoder) = (Encoder::default(), Decoder::default());
        let mut bytes = Vec::new();
        for (index, &quantizer) in quantizers.iter().enumerate() {
            let planes = scene(size, index, noise);
            let request = Request { is_key: keys.contains(&index), quantizer };
            let coded = encoder.encode(&picture(&planes, size), request).unwrap();
            assert_eq!(coded.is_key, index == 0 || keys.contains(&index), "{size:?} frame {index}: the kind asked for");
            let decoded = decoder.decode(&coded.frame).unwrap().map(|picture| i420(&picture));
            let reconstruction = i420(&encoder.reconstruction());
            assert_eq!(decoded.as_ref(), Some(&reconstruction), "{size:?} frame {index}: the decoder's picture");
            bytes.extend_from_slice(&coded.frame);
        }
        crc32(&bytes)
    }

    #[test]
    fn reconstruction_decodes_exactly() {
        let quantizers = [4, 40, 90, 127, 0, 60, 20, 110];
        let crcs: Vec<u32> = [(2, 2), (16, 16), (18, 34), (33, 17), (64, 48), (176, 144)]
            .into_iter()
            .map(|size| run(size, &quantizers, &[5], 6))
            .collect();
        assert_eq!(crcs, PINNED_CRCS, "the bytes of every frame at every size, as first coded");
    }

    // The disc uncovers the first frame's background, which the golden frame keeps, about parts of it that move: macroblocks the golden frame predicts best whole and SPLITMV from the last frame better in quarters.
    #[test]
    fn golden_and_split_decode_exactly() {
        for (quantizer, noise) in [(30, 0), (20, 2)] {
            run((176, 144), &[quantizer; 20], &[], noise);
        }
    }

    // A frame of the scene turned to its negative after one of the scene: the bands predict its macroblocks from the last frame and reconstruct them so, and the coding, by the edges it reconstructs, codes many in their place predicted within the frame.
    #[test]
    fn intra_in_place_of_inter_decodes_exactly() {
        let size = (64, 48);
        let (mut encoder, mut decoder) = (Encoder::default(), Decoder::default());
        for index in 0..3 {
            let mut planes = scene(size, index, 2);
            if index == 2 {
                planes[0].iter_mut().for_each(|pixel| *pixel = 255 - *pixel);
            }
            encoder.begin(&picture(&planes, size), Request { is_key: false, quantizer: 40 }).unwrap();
            encoder.code_steps(|_, work| work.in_order());
            let columns = encoder.columns;
            let replaced = (encoder.choices.iter().enumerate())
                .filter(|(at, choice)| {
                    let coded = encoder.grid.get(at / columns, at % columns).reference;
                    choice.record.reference != INTRA_FRAME && coded == INTRA_FRAME
                })
                .count();
            assert!(index < 2 || replaced > 0, "the negative: {replaced} macroblocks coded in place of the bands'");
            let coded = encoder.finish();
            let decoded = decoder.decode(&coded.frame).unwrap().map(|picture| i420(&picture));
            assert_eq!(decoded, Some(i420(&encoder.reconstruction())), "frame {index}: the decoder's picture");
        }
    }

    #[test]
    fn steps_in_order() {
        let size = (64, 48);
        let mut encoder = Encoder::default();
        let mut steps = Vec::new();
        for index in 0..2 {
            encoder.begin(&picture(&scene(size, index, 0), size), Request { is_key: false, quantizer: 30 }).unwrap();
            encoder.code_steps(|step, work| {
                steps.push(step);
                work.in_order();
            });
            encoder.finish();
        }
        assert_eq!(
            steps,
            [Step::ALL, Step::ALL].concat(),
            "a key frame and an inter frame each run every step of `Step::ALL` once, in its order"
        );
    }

    #[test]
    fn laps_of_steps() {
        let mut laps = Laps::default();
        let millis = Duration::from_millis;
        for (step, time) in [(Step::Bands, 12), (Step::Code, 3), (Step::Ends, 1), (Step::Code, 3), (Step::Ends, 2)] {
            laps.add(step, millis(time));
        }
        assert_eq!(
            (laps.times, laps.longest),
            ([Duration::ZERO, millis(12), millis(6), Duration::ZERO, Duration::ZERO, millis(3)], millis(16)),
            "each step's times summed in its place, and the longest picture's steps"
        );
        assert_eq!(
            laps.to_text(2),
            "interpolations 0 us, bands 6000 us, code 3000 us, counts 0 us, probabilities 0 us, ends 1500 us",
            "each step's mean by its name, in the order the steps run"
        );
    }

    #[test]
    fn threaded_steps_match_whole() {
        let size = (176, 144);
        let (mut whole, mut stepped) = (Encoder::default(), Encoder::default());
        for index in 0..6 {
            let planes = scene(size, index, 4);
            let request = Request { is_key: index == 3, quantizer: [10, 50, 90][index % 3] };
            let expected = whole.encode(&picture(&planes, size), request).unwrap();
            stepped.begin(&picture(&planes, size), request).unwrap();
            stepped.code_steps(|_, work| on_threads(work));
            assert_eq!(stepped.finish(), expected, "frame {index}: the steps on threads code the same bytes");
        }
    }

    #[test]
    fn key_frame_on_demand() {
        let size = (64, 48);
        let mut encoder = Encoder::default();
        let frames: Vec<(Coded, Vec<u8>)> = (0..8)
            .map(|index| {
                let planes = scene(size, index, 4);
                let coded = encoder.encode(&picture(&planes, size), Request { is_key: index == 5, quantizer: 30 });
                (coded.unwrap(), i420(&encoder.reconstruction()))
            })
            .collect();
        assert_eq!(
            frames.iter().map(|(coded, _)| super::super::is_key_frame(&coded.frame)).collect::<Vec<_>>(),
            [true, false, false, false, false, true, false, false],
            "the first frame and the one asked for are key frames"
        );
        let mut decoder = Decoder::default();
        for (index, (coded, reconstruction)) in frames.iter().enumerate().skip(5) {
            let decoded = decoder.decode(&coded.frame).unwrap().map(|picture| i420(&picture));
            assert_eq!(decoded.as_ref(), Some(reconstruction), "frame {index} decoded from the key frame on alone");
        }
    }

    #[test]
    fn loss_keeps_probabilities() {
        let size = (64, 48);
        let mut encoder = Encoder::default();
        let frames: Vec<Coded> = (0..6)
            .map(|index| {
                encoder.encode(&picture(&scene(size, index, 4), size), Request { is_key: false, quantizer: 20 })
            })
            .collect::<Result<_, _>>()
            .unwrap();
        for coded in &frames {
            let tag = super::super::header::read_tag(&coded.frame).unwrap();
            let start = if tag.is_key { header::KEY_BYTES } else { header::TAG_BYTES };
            let mut decoder = crate::codec::boolean::BoolDecoder::new(&coded.frame[start..]);
            let header = header::read(&mut decoder, tag, &mut State::default()).unwrap();
            assert!(
                !header.refresh_entropy,
                "no frame keeps its probabilities past itself, so a loss leaves them whole"
            );
        }
        let mut decoder = Decoder::default();
        for (index, coded) in frames.iter().enumerate().filter(|&(index, _)| index != 2) {
            assert!(decoder.decode(&coded.frame).is_ok_and(|picture| picture.is_some()), "frame {index} after a loss");
        }
    }

    #[test]
    fn quality_bounds() {
        let size = (176, 144);
        let mut encoder = Encoder::default();
        let (mut bytes, mut sum) = (0, 0.0);
        for index in 0..10 {
            let planes = scene(size, index, 4);
            let coded = encoder.encode(&picture(&planes, size), Request { is_key: false, quantizer: 40 }).unwrap();
            bytes += coded.frame.len();
            sum += psnr(&planes[0], &i420(&encoder.reconstruction())[..size.0 * size.1]);
        }
        let mean = sum / 10.0;
        assert!(mean > MEASURED_PSNR - PSNR_MARGIN, "the mean luma PSNR {mean} against {MEASURED_PSNR} measured");
        assert!(bytes < MEASURED_BYTES * 105 / 100, "{bytes} bytes against {MEASURED_BYTES} measured");
    }

    #[test]
    fn extremes_and_refusals() {
        for (name, value) in [("black", Some(0u8)), ("white", Some(255)), ("noise", None)] {
            let size = (48, 32);
            let (mut encoder, mut decoder) = (Encoder::default(), Decoder::default());
            for (index, quantizer) in [0, 127, 64].into_iter().enumerate() {
                let mut state = 0x1357_9BDF ^ index as u32;
                let mut fill = |count: usize| -> Vec<u8> {
                    (0..count)
                        .map(|_| {
                            state = xorshift32(state);
                            value.unwrap_or(state as u8)
                        })
                        .collect()
                };
                let planes = [fill(48 * 32), fill(24 * 16), fill(24 * 16)];
                let coded = encoder.encode(&picture(&planes, size), Request { is_key: false, quantizer }).unwrap();
                let decoded = decoder.decode(&coded.frame).unwrap().map(|picture| i420(&picture));
                assert_eq!(decoded, Some(i420(&encoder.reconstruction())), "{name} at quantizer {quantizer}");
            }
        }
        // The decoder's largest picture, of 8192 macroblocks: `run` asserts each frame decodes as reconstructed.
        run((4096, 512), &[127, 127], &[], 0);
        let mut encoder = Encoder::default();
        let request = Request { is_key: false, quantizer: 0 };
        let planes = scene((2, 2), 0, 0);
        for (size, flaw) in [
            ((0, 2), Flaw::Invalid("picture size")),
            ((4112, 16), Flaw::Unsupported("a picture of more than 8192 macroblocks or 256 across or down")),
            ((2048, 1040), Flaw::Unsupported("a picture of more than 8192 macroblocks or 256 across or down")),
            ((4, 4), Flaw::Short),
        ] {
            assert_eq!(encoder.encode(&picture(&planes, size), request), Err(flaw), "a picture of {size:?} refused");
        }
    }
}
