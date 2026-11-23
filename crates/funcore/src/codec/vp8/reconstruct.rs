use super::frame::{
    CHROMA_ACROSS, CHROMA_BLOCK, CHROMA_SUBBLOCKS, MACROBLOCK, SUBBLOCK, SUBBLOCKS, SUBBLOCKS_ACROSS, side,
};
use super::inter::{self, Filter};
use super::intra::{self, ABOVE_EDGE, ABOVE_RIGHT, Edges, LEFT_EDGE};
use super::modes::Macroblock;
use super::tables::{B_PRED, SPLITMV};
use super::tokens::{COEFFICIENTS, Residue, U_FIRST, V_FIRST, Y2_BLOCK};
use super::transform;
use crate::base::crew::Handoff;
use crate::codec::picture::{Frame, Geometry};
use alloc::vec::Vec;

// A luma vector in quarters is doubled to eighths, the unit the filters and chroma vectors count in (RFC 6386, 18.1).
const QUARTERS_TO_EIGHTHS: i32 = 2;
// Four luma vectors in eighths sum to the chroma vector times 8, which is rounded away from zero (RFC 6386, 18.1).
const CHROMA_DIVISOR: i32 = 8;
const CHROMA_ROUNDING: i32 = 4;
// Version 3 drops the fraction of a chroma vector (RFC 6386, 18.1).
const WHOLE_PIXELS: i32 = !7;
// A chroma subblock covers a quarter of the macroblock: luma subblocks two across and two down.
const QUARTER_ACROSS: usize = SUBBLOCKS_ACROSS / CHROMA_ACROSS;

// The last line of the row above, unfiltered, with the pixels around it that intra prediction reads (RFC 6386, 12).
#[derive(Default)]
pub(super) struct Lines {
    planes: [Vec<u8>; 3],
}

// The pixels past a line's last that intra prediction reads: B_PRED's above and right of luma's last subblocks (RFC 6386, 12.3).
const fn extra(plane: usize) -> usize {
    if plane == 0 { ABOVE_RIGHT } else { 0 }
}

// The bytes of a line of plane `plane` of `geometry` as `Lines` keeps it.
pub(super) const fn line_bytes(plane: usize, geometry: Geometry) -> usize {
    1 + geometry.width + extra(plane)
}

// Writes through `put`, at its places in a line of plane `plane` as `Lines` keeps it, the last line `pixels` of the macroblocks from `column` of a row of `columns`, those of a macroblock or of the whole row: the left edge before the row's first pixel, the row's last repeated past it (RFC 6386, 12.3).
fn write_line(mut put: impl FnMut(usize, &[u8]), pixels: &[u8], (column, columns): (usize, usize), plane: usize) {
    let start = 1 + column * pixels.len();
    put(start, pixels);
    if column == 0 {
        put(0, &[LEFT_EDGE]);
    }
    if let Some(&last) = pixels.last().filter(|_| column + 1 == columns) {
        put(start + pixels.len(), &[last; ABOVE_RIGHT][..extra(plane)]);
    }
}

// Hands the row below, in `line` from `at` on, the last line `pixels` of the macroblock of `column` of `columns` in plane `plane`.
pub(super) fn hand_line(line: &Handoff, at: usize, pixels: &[u8], columns: (usize, usize), plane: usize) {
    write_line(|place, bytes| line.put(at + place, bytes), pixels, columns, plane);
}

impl Lines {
    // The lines above the first row: the frame's top edge.
    pub(super) fn reset(&mut self, geometries: &[Geometry; 3]) {
        for (index, (line, geometry)) in self.planes.iter_mut().zip(geometries).enumerate() {
            line.clear();
            line.resize(line_bytes(index, *geometry), ABOVE_EDGE);
        }
    }

    // Keeps the row's last line.
    pub(super) fn keep(&mut self, frame: &Frame, geometries: &[Geometry; 3], row: usize) {
        for (index, ((line, plane), geometry)) in self.planes.iter_mut().zip(&frame.planes).zip(geometries).enumerate()
        {
            let start = geometry.at(0, (row + 1) * side(index) - 1);
            let Some(pixels) = plane.get(start..start + geometry.width) else {
                continue;
            };
            let put = |place: usize, bytes: &[u8]| line[place..place + bytes.len()].copy_from_slice(bytes);
            write_line(put, pixels, (0, 1), index);
        }
    }

    // What each plane's line holds above the macroblock of column `column`, from the pixel left of its first on.
    pub(super) fn above(&self, column: usize) -> [&[u8]; 3] {
        let [luma, u, v] = &self.planes;
        [&luma[column * MACROBLOCK..], &u[column * CHROMA_BLOCK..], &v[column * CHROMA_BLOCK..]]
    }
}

// Where a macroblock's residue comes from once its prediction stands in its place in the frame, which an encoder's band holds its lines of: the tokens read, or the encoder's source less that prediction; each part is asked for after it is predicted, a subblock of B_PRED after those before it.
pub(super) trait Residual {
    // The residue once the macroblock's luma is predicted at `at` of `plane`.
    fn luma(&mut self, plane: &[u8], at: usize, stride: usize, has_y2: bool) -> &mut Residue;

    // The residue once luma subblock `block` of B_PRED is predicted at `at` of `plane`.
    fn subblock(&mut self, block: usize, plane: &[u8], at: usize, stride: usize) -> &mut Residue;

    // The residue once U and V are predicted at `at` of their planes.
    fn chroma(&mut self, planes: [&[u8]; 2], at: usize, stride: usize) -> &mut Residue;
}

impl Residual for Residue {
    fn luma(&mut self, _: &[u8], _: usize, _: usize, _: bool) -> &mut Residue {
        self
    }

    fn subblock(&mut self, _: usize, _: &[u8], _: usize, _: usize) -> &mut Residue {
        self
    }

    fn chroma(&mut self, _: [&[u8]; 2], _: usize, _: usize) -> &mut Residue {
        self
    }
}

// Adds a block's residue, its DC alone where nothing follows; `dc` is Y2's for a luma block under Y2 (RFC 6386, 14).
fn add_block(block: &mut [i16; COEFFICIENTS], end: u8, dc: Option<i16>, plane: &mut [u8], at: usize, stride: usize) {
    let dc = match dc {
        Some(dc) if end > 1 => {
            block[0] = dc;
            None
        }
        Some(dc) => Some(dc),
        None if end == 1 => Some(block[0]),
        None => None,
    };
    match (dc, end) {
        (Some(0), _) => {}
        (Some(dc), _) => transform::add_dc(dc, plane, at, stride),
        (None, 0) => {}
        (None, _) => transform::add_residue(&transform::inverse_dct(block), plane, at, stride),
    }
}

fn add_luma(residue: &mut Residue, has_y2: bool, (plane, at, stride): Target<'_>) {
    let dcs = (has_y2 && residue.ends[Y2_BLOCK] > 0).then(|| transform::inverse_wht(&residue.blocks[Y2_BLOCK]));
    for block in 0..SUBBLOCKS {
        let place = at + block / SUBBLOCKS_ACROSS * SUBBLOCK * stride + block % SUBBLOCKS_ACROSS * SUBBLOCK;
        let dc = has_y2.then(|| dcs.map_or(0, |dcs| dcs[block]));
        add_block(&mut residue.blocks[block], residue.ends[block], dc, plane, place, stride);
    }
}

fn add_chroma(residue: &mut Residue, planes: [Target<'_>; 2]) {
    for ((plane, at, stride), first) in planes.into_iter().zip([U_FIRST, V_FIRST]) {
        for block in 0..CHROMA_SUBBLOCKS {
            let place = at + block / CHROMA_ACROSS * SUBBLOCK * stride + block % CHROMA_ACROSS * SUBBLOCK;
            add_block(&mut residue.blocks[first + block], residue.ends[first + block], None, plane, place, stride);
        }
    }
}

// A block read in a plane: the plane, the place of the block's top left in it, and its stride.
pub(super) type Pixels<'a> = (&'a [u8], usize, usize);

// The edges of the block of `size` at `at` of `plane`: `above`, the line above it from the pixel left of its first on as `Lines` keeps it; the column to its left, which `left` is filled with; and the frame's edge values where the frame has none, as `has` tells, left and above (RFC 6386, 12.2).
pub(super) fn edges<'a>(
    (plane, at, stride): Pixels<'_>,
    above: &'a [u8],
    (has_left, has_above): (bool, bool),
    size: usize,
    left: &'a mut [u8; MACROBLOCK],
) -> Edges<'a> {
    *left = [LEFT_EDGE; MACROBLOCK];
    if has_left {
        for (place, pixel) in left.iter_mut().take(size).enumerate() {
            *pixel = plane[at - 1 + place * stride];
        }
    }
    Edges { above: &above[1..=size], left: &left[..size], corner: above[0], has_above, has_left }
}

// The eight pixels above B_PRED's subblock `block` of the macroblock at `at` of `plane`, the four to its left and the corner: those of the subblocks before it, `above` as `edges` takes it, and the frame's edge values where it has none to its left (RFC 6386, 12.3).
pub(super) fn subblock_edges(
    (plane, at, stride): Pixels<'_>,
    above: &[u8],
    has_left: bool,
    block: usize,
) -> ([u8; SUBBLOCK + ABOVE_RIGHT], [u8; SUBBLOCK], u8) {
    let (across, down) = (block % SUBBLOCKS_ACROSS * SUBBLOCK, block / SUBBLOCKS_ACROSS * SUBBLOCK);
    let place = at + down * stride + across;
    let mut pixels = [0u8; SUBBLOCK + ABOVE_RIGHT];
    if down == 0 {
        pixels.copy_from_slice(&above[1 + across..1 + across + SUBBLOCK + ABOVE_RIGHT]);
    } else {
        let start = place - stride;
        pixels[..SUBBLOCK].copy_from_slice(&plane[start..start + SUBBLOCK]);
        if across + SUBBLOCK < MACROBLOCK {
            pixels[SUBBLOCK..].copy_from_slice(&plane[start + SUBBLOCK..start + SUBBLOCK + ABOVE_RIGHT]);
        } else {
            // The right column's subblocks take the pixels right of the top one's, the next macroblock's not decoded.
            pixels[SUBBLOCK..].copy_from_slice(&above[1 + MACROBLOCK..1 + MACROBLOCK + ABOVE_RIGHT]);
        }
    }
    let has_left = has_left || across > 0;
    let mut left = [LEFT_EDGE; SUBBLOCK];
    if has_left {
        for (line, pixel) in left.iter_mut().enumerate() {
            *pixel = plane[place - 1 + line * stride];
        }
    }
    let corner = match (down, has_left) {
        (0, _) => above[across],
        (_, false) => LEFT_EDGE,
        _ => plane[place - stride - 1],
    };
    (pixels, left, corner)
}

// Predicts an intra macroblock into its targets and adds its residue, `above` each plane's line above it as `edges` takes it and `has` whether it has a macroblock to its left and above (RFC 6386, 12 and 14).
pub(super) fn intra<R: Residual>(
    mb: &Macroblock,
    residual: &mut R,
    ((luma, at, stride), ([u, v], chroma_at, chroma_stride)): (Target<'_>, ChromaTarget<'_>),
    above: [&[u8]; 3],
    has: (bool, bool),
) {
    let mut left = [LEFT_EDGE; MACROBLOCK];
    if mb.y_mode == B_PRED {
        predict_subblocks(mb, residual, (luma, at, stride), above[0], has.0);
    } else {
        let edges = edges((luma, at, stride), above[0], has, MACROBLOCK, &mut left);
        intra::predict(mb.y_mode, MACROBLOCK, &edges, luma, at, stride);
        add_luma(residual.luma(luma, at, stride, true), true, (luma, at, stride));
    }
    for (plane, above) in [(&mut *u, above[1]), (&mut *v, above[2])] {
        let edges = edges((plane, chroma_at, chroma_stride), above, has, CHROMA_BLOCK, &mut left);
        intra::predict(mb.uv_mode, CHROMA_BLOCK, &edges, plane, chroma_at, chroma_stride);
    }
    let residue = residual.chroma([&u[..], &v[..]], chroma_at, chroma_stride);
    add_chroma(residue, [(&mut u[..], chroma_at, chroma_stride), (&mut v[..], chroma_at, chroma_stride)]);
}

// Predicts the sixteen subblocks of B_PRED one by one, each from those decoded before it (RFC 6386, 12.3).
fn predict_subblocks<R: Residual>(
    mb: &Macroblock,
    residual: &mut R,
    (plane, at, stride): Target<'_>,
    above: &[u8],
    has_left: bool,
) {
    for block in 0..SUBBLOCKS {
        let place = at + block / SUBBLOCKS_ACROSS * SUBBLOCK * stride + block % SUBBLOCKS_ACROSS * SUBBLOCK;
        let (pixels, left, corner) = subblock_edges((plane, at, stride), above, has_left, block);
        let predicted = intra::predict_subblock(mb.modes[block], &pixels, &left, corner);
        put(&predicted, SUBBLOCK, (plane, place, stride));
        let residue = residual.subblock(block, plane, place, stride);
        add_block(&mut residue.blocks[block], residue.ends[block], None, plane, place, stride);
    }
}

// The chroma vector of four luma vectors in quarters: their average in eighths of a chroma pixel (RFC 6386, 18.1).
fn chroma_vector(quarters: [(i16, i16); 4], is_full_pixel: bool) -> (i32, i32) {
    let average = |component: fn(&(i16, i16)) -> i16| {
        let sum: i32 = quarters.iter().map(|vector| QUARTERS_TO_EIGHTHS * i32::from(component(vector))).sum();
        let eighths = (sum + if sum < 0 { -CHROMA_ROUNDING } else { CHROMA_ROUNDING }) / CHROMA_DIVISOR;
        if is_full_pixel { eighths & WHOLE_PIXELS } else { eighths }
    };
    (average(|vector| vector.0), average(|vector| vector.1))
}

// The four luma subblocks of the quarter of a macroblock that chroma subblock `block` covers.
const fn quarter(block: usize) -> [usize; 4] {
    let first = (block % CHROMA_ACROSS + block / CHROMA_ACROSS * SUBBLOCKS_ACROSS) * QUARTER_ACROSS;
    [first, first + 1, first + SUBBLOCKS_ACROSS, first + SUBBLOCKS_ACROSS + 1]
}

// Where a prediction goes: a plane, the place of the macroblock's or chroma block's top left in it, and its stride.
pub(super) type Target<'a> = (&'a mut [u8], usize, usize);

// Puts a square block of `size`, its lines one after another, in its place in a plane.
pub(super) fn put(block: &[u8], size: usize, (plane, at, stride): Target<'_>) {
    for (line, pixels) in plane[at..].chunks_mut(stride).zip(block.chunks_exact(size)) {
        line[..size].copy_from_slice(pixels);
    }
}

// Predicts an inter macroblock's luma from its reference into `target` (RFC 6386, 18).
pub(super) fn predict_luma(
    mb: &Macroblock,
    reference: (&[u8], Geometry),
    (row, column_index): (usize, usize),
    filter: Filter,
    (plane, origin, stride): Target<'_>,
) {
    let (x, y) = (column_index * MACROBLOCK, row * MACROBLOCK);
    let mut predict = |(across, down): (usize, usize), subblock: usize, side: usize| {
        let place = (x + across * SUBBLOCK, y + down * SUBBLOCK);
        let mv = mb.mvs[subblock];
        let vector = (QUARTERS_TO_EIGHTHS * i32::from(mv.x), QUARTERS_TO_EIGHTHS * i32::from(mv.y));
        let at = origin + down * SUBBLOCK * stride + across * SUBBLOCK;
        inter::predict(reference, (place.0 as i32, place.1 as i32), vector, (side, side), filter, plane, at, stride);
    };
    if mb.y_mode != SPLITMV {
        predict((0, 0), 0, MACROBLOCK);
        return;
    }
    // A quarter whose four vectors are the same is predicted in one go, as its four subblocks would be.
    for block in 0..CHROMA_SUBBLOCKS {
        let group = quarter(block);
        if group.iter().all(|&subblock| mb.mvs[subblock] == mb.mvs[group[0]]) {
            let first = group[0];
            predict((first % SUBBLOCKS_ACROSS, first / SUBBLOCKS_ACROSS), first, QUARTER_ACROSS * SUBBLOCK);
        } else {
            for subblock in group {
                predict((subblock % SUBBLOCKS_ACROSS, subblock / SUBBLOCKS_ACROSS), subblock, SUBBLOCK);
            }
        }
    }
}

// Predicts an inter macroblock's U and V from their references into their targets (RFC 6386, 18).
fn predict_chroma(
    mb: &Macroblock,
    references: [(&[u8], Geometry); 2],
    (row, column_index): (usize, usize),
    (filter, is_full_pixel): (Filter, bool),
    targets: [Target<'_>; 2],
) {
    let (x, y) = (column_index * CHROMA_BLOCK, row * CHROMA_BLOCK);
    let quarters = |subblock: usize| (mb.mvs[subblock].x, mb.mvs[subblock].y);
    for (reference, (plane, origin, stride)) in references.into_iter().zip(targets) {
        if mb.y_mode != SPLITMV {
            let vector = chroma_vector([quarters(0); 4], is_full_pixel);
            let size = (CHROMA_BLOCK, CHROMA_BLOCK);
            inter::predict(reference, (x as i32, y as i32), vector, size, filter, plane, origin, stride);
            continue;
        }
        for block in 0..CHROMA_SUBBLOCKS {
            let vector = chroma_vector(quarter(block).map(quarters), is_full_pixel);
            let (across, down) = (block % CHROMA_ACROSS * SUBBLOCK, block / CHROMA_ACROSS * SUBBLOCK);
            let place = ((x + across) as i32, (y + down) as i32);
            let at = origin + down * stride + across;
            inter::predict(reference, place, vector, (SUBBLOCK, SUBBLOCK), filter, plane, at, stride);
        }
    }
}

// Where an inter macroblock's U and V go: their planes, the place of its chroma block in both, and their stride.
pub(super) type ChromaTarget<'a> = ([&'a mut [u8]; 2], usize, usize);

// Reconstructs the inter macroblock at `position` whose luma `luma` holds predicted: adds its residue there, predicts its U and V from `reference` into `chroma` and adds theirs (RFC 6386, 18).
pub(super) fn inter<R: Residual>(
    mb: &Macroblock,
    residual: &mut R,
    ((plane, at, stride), chroma): (Target<'_>, ChromaTarget<'_>),
    (reference, geometries): (&Frame, &[Geometry; 3]),
    position: (usize, usize),
    (filter, is_full_pixel): (Filter, bool),
) {
    let has_y2 = mb.y_mode != SPLITMV;
    add_luma(residual.luma(plane, at, stride, has_y2), has_y2, (plane, at, stride));
    let ([u, v], at, stride) = chroma;
    let references = [(&reference.planes[1][..], geometries[1]), (&reference.planes[2][..], geometries[2])];
    let targets = [(&mut *u, at, stride), (&mut *v, at, stride)];
    predict_chroma(mb, references, position, (filter, is_full_pixel), targets);
    let residue = residual.chroma([&u[..], &v[..]], at, stride);
    add_chroma(residue, [(u, at, stride), (v, at, stride)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chroma_vectors_rounded_away_from_zero() {
        assert_eq!(chroma_vector([(3, -3); 4], false), (3, -3), "four equal vectors average to themselves in eighths");
        assert_eq!(chroma_vector([(1, -1), (0, 0), (0, 0), (0, 0)], false), (0, 0), "(2 + 4) / 8 and (-2 - 4) / 8");
        assert_eq!(chroma_vector([(1, -1), (1, -1), (0, 0), (0, 0)], false), (1, -1), "(4 + 4) / 8 and (-4 - 4) / 8");
        assert_eq!(
            chroma_vector([(5, -5); 4], true),
            (0, -8),
            "whole pixels: the fraction dropped toward minus infinity"
        );
        assert_eq!([quarter(0), quarter(3)], [[0, 1, 4, 5], [10, 11, 14, 15]], "the chroma subblocks' luma quarters");
    }

    #[test]
    fn dc_only_blocks() {
        let mut plane = [100u8; 64];
        let mut block = [0i16; COEFFICIENTS];
        block[0] = 40;
        add_block(&mut block, 1, None, &mut plane, 0, 8);
        assert_eq!(plane[..4], [105, 105, 105, 105], "a block whose last coefficient is its DC: (40 + 4) >> 3");
        add_block(&mut [0; COEFFICIENTS], 0, Some(-24), &mut plane, 4, 8);
        assert_eq!(plane[4..8], [97, 97, 97, 97], "a luma block with Y2 and no other coefficient: (-24 + 4) >> 3");
        let mut ac = [0i16; COEFFICIENTS];
        ac[1] = 100;
        add_block(&mut ac, 2, Some(0), &mut plane, 32, 8);
        assert_eq!(plane[32..36], [116, 107, 93, 84], "Y2's DC of nought set into a block with a coefficient after it");
    }
}
