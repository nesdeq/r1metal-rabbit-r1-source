use super::frame::{CHROMA_ACROSS, CHROMA_SUBBLOCKS, SUBBLOCK, SUBBLOCKS, SUBBLOCKS_ACROSS, side};
use super::reconstruct::Residual;
use super::tables::{DCT_CAT_BASE, PCAT6, ZIGZAG};
use super::tokens::{COEFFICIENTS, Factors, Levels, Residue, U_FIRST, V_FIRST, Y2_BLOCK};
use super::transform::{forward_dct, forward_wht};
use crate::codec::picture::{Frame, Geometry};

// A step's dead zone, its rounding and the dead zone's growth with each nought before a coefficient, in 128ths of the step: a dead zone of 84 or 116, a rounding of 36 or 56 and a growth of 0 or 8 coded the test scenes worse, a growth held at 44 alike (measured).
const ZERO_BIN: i32 = 100;
const ROUNDING: i32 = 48;
const RUN_BOOST: i32 = 4;
const SCALE_BITS: u32 = 7;
// The largest level a token codes: DCT_CAT6's base and its eleven extra bits (RFC 6386, 13.2).
const MOST_LEVEL: i32 = DCT_CAT_BASE[5] + (1 << PCAT6.len()) - 1;

// How the coefficients of one kind are quantised: the decoder's factor, the dead zone by the run of noughts before, the rounding.
#[derive(Clone, Copy, Default)]
pub(super) struct Step {
    factor: i32,
    bins: [i32; COEFFICIENTS],
    rounding: i32,
}

impl Step {
    fn new(factor: i32) -> Step {
        let scale = |share: i32| (factor * share + (1 << (SCALE_BITS - 1))) >> SCALE_BITS;
        Step {
            factor,
            bins: core::array::from_fn(|run| scale(ZERO_BIN + run as i32 * RUN_BOOST)),
            rounding: scale(ROUNDING),
        }
    }
}

// The steps of a segment's DC and other coefficients, by the kind of block.
#[derive(Clone, Copy, Default)]
pub(super) struct Steps {
    y1: [Step; 2],
    y2: [Step; 2],
    uv: [Step; 2],
}

impl Steps {
    pub(super) fn new(factors: &Factors) -> Steps {
        let pair = |[dc, ac]: [i32; 2]| [Step::new(dc), Step::new(ac)];
        Steps { y1: pair(factors.y1), y2: pair(factors.y2), uv: pair(factors.uv) }
    }
}

// Quantises a block's coefficients in raster order from zigzag place `first` on: its levels in zigzag order, their values as the decoder dequantises them in raster order, and the place after the last level not nought, or nought.
fn block(
    coefficients: &[i16; COEFFICIENTS],
    steps: &[Step; 2],
    first: usize,
    levels: &mut [i16; COEFFICIENTS],
    dequantized: &mut [i16; COEFFICIENTS],
) -> u8 {
    *levels = [0; COEFFICIENTS];
    *dequantized = [0; COEFFICIENTS];
    // A block whose coefficients all lie inside the narrowest dead zone, that of no nought before, codes none.
    let is_inside = |at: usize, step: &Step| i32::from(coefficients[at]).abs() < step.bins[0];
    if (first > 0 || is_inside(0, &steps[0])) && (1..COEFFICIENTS).all(|at| is_inside(at, &steps[1])) {
        return 0;
    }
    let (mut end, mut run) = (0, 0);
    for place in first..COEFFICIENTS {
        let at = usize::from(ZIGZAG[place]);
        let step = &steps[usize::from(place > 0)];
        let value = i32::from(coefficients[at]);
        let magnitude = value.abs();
        if magnitude >= step.bins[run] {
            let level = ((magnitude + step.rounding) / step.factor).min(MOST_LEVEL);
            if level > 0 {
                let level = if value < 0 { -level } else { level };
                levels[place] = level as i16;
                // The product is a 16-bit word, as the decoder forms it (RFC 6386, 14.1).
                dequantized[at] = (level * step.factor) as i16;
                (end, run) = (place + 1, 0);
                continue;
            }
        }
        run = (run + 1).min(COEFFICIENTS - 1);
    }
    end as u8
}

// The source less a prediction over a 4x4 block, both of lines their stride apart.
fn difference(
    (source, at, stride): (&[u8], usize, usize),
    (plane, place, plane_stride): (&[u8], usize, usize),
) -> [i16; 16] {
    core::array::from_fn(|index| {
        let (line, x) = (index / SUBBLOCK, index % SUBBLOCK);
        i16::from(source[at + line * stride + x]) - i16::from(plane[place + line * plane_stride + x])
    })
}

// The encoder's residue: the source less the prediction, transformed and quantised into levels for the tokens and their dequantised values for the reconstruction.
pub(super) struct Coder<'a> {
    pub(super) source: &'a Frame,
    pub(super) geometries: &'a [Geometry; 3],
    pub(super) position: (usize, usize),
    pub(super) steps: &'a Steps,
    pub(super) residue: &'a mut Residue,
    pub(super) levels: &'a mut Levels,
}

impl Coder<'_> {
    // Codes block `index` of plane `plane` at `x`, `y` of the macroblock against its prediction: its coefficients.
    fn code(
        &mut self,
        (plane, (x, y)): (usize, (usize, usize)),
        predicted: (&[u8], usize, usize),
        (steps, first): (&[Step; 2], usize),
        index: usize,
    ) -> [i16; COEFFICIENTS] {
        let geometry = self.geometries[plane];
        let (row, column) = self.position;
        let at = geometry.at(column * side(plane) + x, row * side(plane) + y);
        let coefficients = forward_dct(&difference((&self.source.planes[plane], at, geometry.stride), predicted));
        let end = block(&coefficients, steps, first, &mut self.levels.blocks[index], &mut self.residue.blocks[index]);
        (self.levels.ends[index], self.residue.ends[index]) = (end, end);
        coefficients
    }
}

impl Residual for Coder<'_> {
    fn luma(&mut self, plane: &[u8], at: usize, stride: usize, has_y2: bool) -> &mut Residue {
        let steps = self.steps;
        let mut dcs = [0; SUBBLOCKS];
        for (index, dc) in dcs.iter_mut().enumerate() {
            let (x, y) = (index % SUBBLOCKS_ACROSS * SUBBLOCK, index / SUBBLOCKS_ACROSS * SUBBLOCK);
            let predicted = (plane, at + y * stride + x, stride);
            *dc = self.code((0, (x, y)), predicted, (&steps.y1, usize::from(has_y2)), index)[0];
        }
        let (levels, residue) = (&mut self.levels.blocks[Y2_BLOCK], &mut self.residue.blocks[Y2_BLOCK]);
        let end = if has_y2 { block(&forward_wht(&dcs), &steps.y2, 0, levels, residue) } else { 0 };
        (self.levels.ends[Y2_BLOCK], self.residue.ends[Y2_BLOCK]) = (end, end);
        self.residue
    }

    fn subblock(&mut self, index: usize, plane: &[u8], at: usize, stride: usize) -> &mut Residue {
        let (x, y) = (index % SUBBLOCKS_ACROSS * SUBBLOCK, index / SUBBLOCKS_ACROSS * SUBBLOCK);
        let steps = self.steps;
        self.code((0, (x, y)), (plane, at, stride), (&steps.y1, 0), index);
        self.residue
    }

    fn chroma(&mut self, planes: [&[u8]; 2], at: usize, stride: usize) -> &mut Residue {
        let steps = self.steps;
        for (index, (plane, first)) in planes.into_iter().zip([U_FIRST, V_FIRST]).enumerate() {
            for block in 0..CHROMA_SUBBLOCKS {
                let (x, y) = (block % CHROMA_ACROSS * SUBBLOCK, block / CHROMA_ACROSS * SUBBLOCK);
                let predicted = (plane, at + y * stride + x, stride);
                self.code((1 + index, (x, y)), predicted, (&steps.uv, 0), first + block);
            }
        }
        self.residue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_zone_and_rounding() {
        let steps = [Step::new(20), Step::new(10)];
        let mut coefficients = [0i16; COEFFICIENTS];
        coefficients[0] = 45;
        coefficients[1] = -9;
        coefficients[4] = 6;
        coefficients[15] = 9;
        let (mut levels, mut dequantized) = ([0; COEFFICIENTS], [0; COEFFICIENTS]);
        let end = block(&coefficients, &steps, 0, &mut levels, &mut dequantized);
        assert_eq!(levels[..3], [2, -1, 0], "45 + 8 over 20, -9 past the dead zone of 8; 6 is inside it");
        assert_eq!((dequantized[0], dequantized[1]), (40, -10), "dequantised by the decoder's factors");
        assert_eq!(end, 2, "9 at the last place lies inside the dead zone of 12 a run of thirteen noughts grew");
        let mut alone = [0i16; COEFFICIENTS];
        alone[15] = 9;
        alone[1] = 30;
        let end = block(&alone, &steps, 1, &mut levels, &mut dequantized);
        assert_eq!((levels[0], levels[1], end), (0, 3, 2), "from place 1 on, the DC left out");
        let mut huge = [0i16; COEFFICIENTS];
        huge[0] = i16::MAX;
        block(&huge, &[Step::new(4), Step::new(4)], 0, &mut levels, &mut dequantized);
        assert_eq!(i32::from(levels[0]), MOST_LEVEL, "held to what a token codes");
    }
}
