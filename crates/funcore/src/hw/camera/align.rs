// Where each tile of a burst's reference lies in another of its frames: coarse to fine over a pyramid of four levels (IPOL's HDR+).

use crate::base::fft::SIDE;
use crate::base::sampling::{filter_down, filter_row, reflect};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Norm {
    Absolute,
    Squared,
}

#[derive(Clone, Copy)]
struct Step {
    factor: usize,
    tile: usize,
    radius: i32,
    norm: Norm,
    is_subpixel: bool,
}

// Finest first, as `params.py` of IPOL's implementation has them; the finest level's tile, 16, is the merge's, whose tiles its offsets move.
const STEPS: [Step; 4] = [
    Step { factor: 1, tile: SIDE, radius: 1, norm: Norm::Absolute, is_subpixel: false },
    Step { factor: 2, tile: 16, radius: 4, norm: Norm::Squared, is_subpixel: true },
    Step { factor: 4, tile: 16, radius: 4, norm: Norm::Squared, is_subpixel: true },
    Step { factor: 4, tile: 8, radius: 4, norm: Norm::Squared, is_subpixel: true },
];
// A Gaussian's reach in its deviations, as SciPy's `gaussian_filter` truncates it.
const TRUNCATE: f64 = 4.0;
const WEIGHT_BITS: u32 = 16;
// IPOL's least squares fit of a paraboloid to the 3x3 distances about the least, row by row.
const FIT_ACROSS: [f32; 9] = [0.25, -0.5, 0.25, 0.5, -1.0, 0.5, 0.25, -0.5, 0.25];
const FIT_DOWN: [f32; 9] = [0.25, 0.5, 0.25, -0.5, -1.0, -0.5, 0.25, 0.5, 0.25];
const FIT_BOTH: [f32; 9] = [0.25, 0.0, -0.25, 0.0, 0.0, 0.0, -0.25, 0.0, 0.25];
const SLOPE_ACROSS: [f32; 9] = [-0.125, 0.0, 0.125, -0.25, 0.0, 0.25, -0.125, 0.0, 0.125];
const SLOPE_DOWN: [f32; 9] = [-0.125, -0.25, -0.125, 0.0, 0.0, 0.0, 0.125, 0.25, 0.125];

// A picture of grey samples.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Image {
    width: usize,
    height: usize,
    samples: Vec<u16>,
}

impl Image {
    pub(super) const fn new(width: usize, height: usize, samples: Vec<u16>) -> Image {
        Image { width, height, samples }
    }

    pub(super) fn samples(&self) -> &[u16] {
        &self.samples
    }

    pub(super) const fn width(&self) -> usize {
        self.width
    }

    pub(super) const fn height(&self) -> usize {
        self.height
    }

    const fn at(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    // The sum of the steps between neighbours across and down: how sharp the picture is.
    pub(super) fn sharpness(&self) -> u64 {
        let mut sum = 0u64;
        for y in 0..self.height.saturating_sub(1) {
            for x in 0..self.width.saturating_sub(1) {
                let here = self.samples[self.at(x, y)];
                sum += u64::from(here.abs_diff(self.samples[self.at(x + 1, y)]))
                    + u64::from(here.abs_diff(self.samples[self.at(x, y + 1)]));
            }
        }
        sum
    }

    fn distance(
        &self,
        other: &Image,
        (x, y): (usize, usize),
        (other_x, other_y): (usize, usize),
        tile: usize,
        norm: Norm,
    ) -> u64 {
        let mut sum = 0u64;
        for row in 0..tile {
            let own = &self.samples[self.at(x, y + row)..][..tile];
            let theirs = &other.samples[other.at(other_x, other_y + row)..][..tile];
            // A row's sum fits 32 bits, 16 squares of 10-bit steps, so the compiler keeps it in vector lanes.
            let row_sum = match norm {
                Norm::Absolute => own.iter().zip(theirs).map(|(a, b)| u32::from(a.abs_diff(*b))).sum::<u32>(),
                Norm::Squared => own.iter().zip(theirs).map(|(a, b)| u32::from(a.abs_diff(*b)).pow(2)).sum::<u32>(),
            };
            sum += u64::from(row_sum);
        }
        sum
    }

    fn blurred_down(&self, factor: usize) -> Image {
        let sigma = factor as f64 / 2.0;
        let reach = (TRUNCATE * sigma + 0.5) as isize;
        let raw: Vec<f64> = (-reach..=reach)
            .map(|at| {
                crate::base::math::pow2_f64(-((at * at) as f64) / (2.0 * sigma * sigma) * core::f64::consts::LOG2_E)
            })
            .collect();
        let total: f64 = raw.iter().sum();
        // A Gaussian of a deviation of half a pixel or more weighs no tap as much as one, so each weight fits 16 bits.
        let weights: Vec<u16> = raw
            .iter()
            .map(|weight| (weight / total * f64::from(1u32 << WEIGHT_BITS) + 0.5) as u64)
            .map(|weight| u16::try_from(weight).expect("a weight under one"))
            .collect();
        let (width, height) = (self.width / factor, self.height / factor);
        let mut across = vec![0u32; width * self.height];
        let rows = self.samples.chunks_exact(self.width.max(1)).zip(across.chunks_exact_mut(width.max(1)));
        for (row, out) in rows {
            filter_row(row, (&weights, factor), reflect, out);
        }
        let wide: Vec<u32> = weights.iter().map(|&weight| weight.into()).collect();
        let (mut samples, mut sums) = (vec![0u16; width * height], vec![0u64; width]);
        for (y, out) in samples.chunks_exact_mut(width.max(1)).enumerate() {
            let row = |at: usize| &across[at * width..][..width];
            filter_down(row, (&wide, y * factor, self.height), reflect, &mut sums);
            for (sample, &sum) in out.iter_mut().zip(&sums) {
                *sample = ((sum + (1 << (2 * WEIGHT_BITS - 1))) >> (2 * WEIGHT_BITS)) as u16;
            }
        }
        Image { width, height, samples }
    }
}

// A frame's grey at the four levels the alignment looks at, finest first.
pub(super) struct Pyramid {
    levels: Vec<Image>,
}

impl Pyramid {
    pub(super) fn new(grey: Image) -> Pyramid {
        let mut levels = vec![grey];
        for step in &STEPS[1..] {
            let next = levels.last().expect("the finest level").blurred_down(step.factor);
            levels.push(next);
        }
        Pyramid { levels }
    }

    pub(super) fn finest(&self) -> &Image {
        &self.levels[0]
    }
}

// Where each tile of a level of the reference lies in another frame: its offset down and across, in that level's samples. The tiles are `tile` samples a side and lie half a tile apart.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct Field {
    tile: usize,
    rows: usize,
    columns: usize,
    offsets: Vec<(f32, f32)>,
}

impl Field {
    const fn tiles(image: &Image, tile: usize) -> (usize, usize) {
        ((image.height / (tile / 2)).saturating_sub(1), (image.width / (tile / 2)).saturating_sub(1))
    }

    fn of(image: &Image, tile: usize, offsets: Vec<(f32, f32)>) -> Field {
        let (rows, columns) = Field::tiles(image, tile);
        Field { tile, rows, columns, offsets }
    }

    // The whole offset of the tile in row `row` and column `column`, none where the field has no such tile.
    pub(super) fn offset(&self, row: usize, column: usize) -> (i32, i32) {
        if row >= self.rows || column >= self.columns {
            return (0, 0);
        }
        let (down, across) = self.offsets[row * self.columns + column];
        (crate::base::math::round(down), crate::base::math::round(across))
    }

    pub(super) const fn rows(&self) -> usize {
        self.rows
    }

    pub(super) const fn columns(&self) -> usize {
        self.columns
    }

    pub(super) const fn tile(&self) -> usize {
        self.tile
    }
}

fn subpixel(distances: [u64; 9]) -> (f32, f32) {
    let fit = |filter: &[f32; 9]| {
        filter.iter().zip(distances).map(|(weight, distance)| weight * distance as f32).sum::<f32>()
    };
    let (across, down, mut both) = (fit(&FIT_ACROSS).max(0.0), fit(&FIT_DOWN).max(0.0), fit(&FIT_BOTH));
    let (slope_across, slope_down) = (fit(&SLOPE_ACROSS), fit(&SLOPE_DOWN));
    if across * down - both * both < 0.0 {
        both = 0.0;
    }
    let determinant = across * down - both * both;
    if determinant == 0.0 {
        return (0.0, 0.0);
    }
    let shift_down = -(across * slope_down - both * slope_across) / determinant;
    let shift_across = -(down * slope_across - both * slope_down) / determinant;
    if shift_down * shift_down + shift_across * shift_across < 1.0 { (shift_down, shift_across) } else { (0.0, 0.0) }
}

fn place(at: usize, offset: i32, limit: usize) -> Option<usize> {
    let moved = at as i64 + i64::from(offset);
    (0..=limit as i64).contains(&moved).then_some(moved as usize)
}

fn search(
    reference: &Image,
    alternate: &Image,
    step: &Step,
    guesses: &[(i32, i32)],
    columns: usize,
) -> Vec<(f32, f32)> {
    let (tile, radius, half) = (step.tile, step.radius, step.tile / 2);
    let (last_x, last_y) = (alternate.width - tile, alternate.height - tile);
    let side = (2 * radius + 1) as usize;
    let mut distances = vec![u64::MAX; side * side];
    guesses
        .iter()
        .enumerate()
        .map(|(index, &(guess_down, guess_across))| {
            let (x, y) = ((index % columns) * half, (index / columns) * half);
            for (place_index, distance) in distances.iter_mut().enumerate() {
                let down = guess_down + place_index as i32 / side as i32 - radius;
                let across = guess_across + place_index as i32 % side as i32 - radius;
                *distance = match (place(x, across, last_x), place(y, down, last_y)) {
                    (Some(other_x), Some(other_y)) => {
                        reference.distance(alternate, (x, y), (other_x, other_y), tile, step.norm)
                    }
                    _ => u64::MAX,
                };
            }
            let least = (0..distances.len()).min_by_key(|&at| distances[at]).expect("a place to look");
            if distances[least] == u64::MAX {
                return (guess_down as f32, guess_across as f32);
            }
            let (row, column) = ((least / side) as i32, (least % side) as i32);
            let is_inside = (1..side as i32 - 1).contains(&row) && (1..side as i32 - 1).contains(&column);
            let around: [u64; 9] = core::array::from_fn(|at| {
                let (row, column) = (row + at as i32 / 3 - 1, column + at as i32 % 3 - 1);
                if is_inside { distances[(row * side as i32 + column) as usize] } else { u64::MAX }
            });
            let (fine_down, fine_across) =
                if step.is_subpixel && is_inside && around.iter().all(|&distance| distance != u64::MAX) {
                    subpixel(around)
                } else {
                    (0.0, 0.0)
                };
            ((guess_down + row - radius) as f32 + fine_down, (guess_across + column - radius) as f32 + fine_across)
        })
        .collect()
}

fn upsample(reference: &Image, alternate: &Image, coarse: &Field, step: &Step, factor: usize) -> Vec<(i32, i32)> {
    let (rows, columns) = Field::tiles(reference, step.tile);
    let repeat = factor / (step.tile / coarse.tile);
    let (last_x, last_y) = (alternate.width - step.tile, alternate.height - step.tile);
    let scaled = |row: usize, column: usize| {
        let (down, across) = coarse.offsets[row * coarse.columns + column];
        (crate::base::math::round(down * factor as f32), crate::base::math::round(across * factor as f32))
    };
    let mut guesses = vec![(0, 0); rows * columns];
    if coarse.rows == 0 || coarse.columns == 0 {
        return guesses;
    }
    for row in 0..rows {
        for column in 0..columns {
            // A tile past the coarse level's last takes that last one's guess, where IPOL's takes none.
            let (coarse_row, coarse_column) =
                ((row / repeat).min(coarse.rows - 1), (column / repeat).min(coarse.columns - 1));
            let neighbour = |at: usize, coarse_at: usize, count: usize| {
                if at % repeat < repeat / 2 { coarse_at.saturating_sub(1) } else { (coarse_at + 1).min(count - 1) }
            };
            let candidates = [
                scaled(coarse_row, coarse_column),
                scaled(neighbour(row, coarse_row, coarse.rows), coarse_column),
                scaled(coarse_row, neighbour(column, coarse_column, coarse.columns)),
            ];
            let (x, y) = (column * step.tile / 2, row * step.tile / 2);
            let cost = |&(down, across): &(i32, i32)| match (place(x, across, last_x), place(y, down, last_y)) {
                (Some(other_x), Some(other_y)) => {
                    reference.distance(alternate, (x, y), (other_x, other_y), step.tile, Norm::Absolute)
                }
                _ => u64::MAX,
            };
            guesses[row * columns + column] = candidates.into_iter().min_by_key(cost).expect("three candidates");
        }
    }
    guesses
}

// Where the reference's finest tiles lie in `alternate`.
pub(super) fn align(reference: &Pyramid, alternate: &Pyramid) -> Field {
    let mut coarse: Option<Field> = None;
    for (level, step) in STEPS.iter().enumerate().rev() {
        let (own, other) = (&reference.levels[level], &alternate.levels[level]);
        let (rows, columns) = Field::tiles(own, step.tile);
        let offsets = if own.width < step.tile || own.height < step.tile {
            vec![(0.0, 0.0); rows * columns]
        } else {
            let guesses = match &coarse {
                Some(field) => upsample(own, other, field, step, STEPS[level + 1].factor),
                None => vec![(0, 0); rows * columns],
            };
            search(own, other, step, &guesses, columns)
        };
        coarse = Some(Field::of(own, step.tile, offsets));
    }
    coarse.expect("four levels")
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;

    // A field that leaves every tile of `image` where it is.
    pub(crate) fn still(image: &Image, tile: usize) -> Field {
        let (rows, columns) = Field::tiles(image, tile);
        Field::of(image, tile, vec![(0.0, 0.0); rows * columns])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lattice(x: i32, y: i32, cell: i32) -> f32 {
        let corner = |column: i32, row: i32| {
            let hash = (column as u32).wrapping_mul(73_856_093) ^ (row as u32).wrapping_mul(19_349_663) ^ cell as u32;
            (crate::base::math::xorshift32(hash | 1) % 256) as f32
        };
        let (column, row) = (x.div_euclid(cell), y.div_euclid(cell));
        let (across, down) = (x.rem_euclid(cell) as f32 / cell as f32, y.rem_euclid(cell) as f32 / cell as f32);
        let upper = corner(column, row) * (1.0 - across) + corner(column + 1, row) * across;
        let lower = corner(column, row + 1) * (1.0 - across) + corner(column + 1, row + 1) * across;
        upper * (1.0 - down) + lower * down
    }

    // Smooth noise at every scale the pyramid looks at, the same nowhere twice.
    fn scene(width: usize, height: usize, shift: (i32, i32)) -> Image {
        let samples = (0..width * height)
            .map(|at| {
                let (x, y) = ((at % width) as i32 - shift.1, (at / width) as i32 - shift.0);
                let sum: f32 = [64, 32, 16, 8, 4].iter().map(|&cell| lattice(x, y, cell)).sum();
                (sum * 0.8) as u16
            })
            .collect();
        Image { width, height, samples }
    }

    #[test]
    fn pyramid_levels() {
        let pyramid = Pyramid::new(Image { width: 1240, height: 1240, samples: vec![500; 1240 * 1240] });
        let sizes: Vec<(usize, usize)> = pyramid.levels.iter().map(|level| (level.width, level.height)).collect();
        assert_eq!(sizes, [(1240, 1240), (620, 620), (155, 155), (38, 38)], "halved, then quartered twice");
        assert!(
            pyramid.levels.iter().all(|level| level.samples.iter().all(|&sample| sample == 500)),
            "flat stays flat"
        );
        assert_eq!(Field::tiles(&pyramid.levels[0], 16), (154, 154));
        assert_eq!(Field::tiles(&pyramid.levels[3], 8), (8, 8));
    }

    #[test]
    fn shifted_frame_found() {
        const SIDE: usize = 1240;
        let reference = Pyramid::new(scene(SIDE, SIDE, (0, 0)));
        for shift in [(0, 0), (5, -3), (-37, 22), (61, 48)] {
            let field = align(&reference, &Pyramid::new(scene(SIDE, SIDE, shift)));
            let (rows, columns) = (field.rows, field.columns);
            // A coarse tile's reach from the frame's edges, where part of what a tile shows has no match.
            let margin = 32i32;
            let is_matched =
                |at: usize, by: i32, length: usize| (margin..=length as i32 - 16 - margin).contains(&(at as i32 + by));
            let tiles = (0..rows).flat_map(|row| (0..columns).map(move |column| (row, column)));
            let matched: alloc::vec::Vec<(usize, usize)> = tiles
                .filter(|&(row, column)| is_matched(column * 8, shift.1, SIDE) && is_matched(row * 8, shift.0, SIDE))
                .collect();
            let wrong = matched.iter().filter(|&&(row, column)| field.offset(row, column) != shift).count();
            // Near an edge a coarse tile holds what has no match; the merge leaves out what such a tile misplaces.
            assert!(
                wrong * 20 <= matched.len(),
                "a frame moved by {shift:?}: {wrong} of {} tiles elsewhere",
                matched.len()
            );
        }
    }

    #[test]
    fn paraboloid_minimum() {
        let bowl = |down: f32, across: f32| -> [u64; 9] {
            core::array::from_fn(|at| {
                let (row, column) = ((at / 3) as f32 - 1.0 - down, (at % 3) as f32 - 1.0 - across);
                (1000.0 + 100.0 * (row * row + column * column)) as u64
            })
        };
        let (down, across) = subpixel(bowl(0.25, -0.4));
        assert!((down - 0.25).abs() < 0.02 && (across + 0.4).abs() < 0.02, "{down} {across}");
        assert_eq!(subpixel([7; 9]), (0.0, 0.0), "a flat floor moves nothing");
    }
}
