use super::read::read_version;
use super::{FIRST_VERSION_INFO, Symbol, VERSIONS, read, side, to_version};
use crate::base::math;
use crate::base::math::round;
use crate::base::math::sqrt;
use crate::base::pixel::PixelFormat;
use crate::base::ycbcr::jfif_luma;
use alloc::vec::Vec;
use core::cmp::Reverse;

const WEIGHT_BITS: u32 = 16;
const BLOCK: usize = 8;
const BLOCK_RADIUS: usize = 2;
const FLAT_RANGE: u8 = 24;
const FINDER: [f32; 5] = [1.0, 1.0, 3.0, 1.0, 1.0];
const FINDER_MODULES: u32 = 7;
const TOLERANCE: f32 = 0.5;
const CROSS_TOLERANCE: f32 = 0.4;
const LEAST_SIGHTINGS: u32 = 2;
const MOST_FINDERS: usize = 15;
const SKEW: f32 = 0.5;
const ALIGNMENT_REACH: f32 = 16.0;
const FINDER_MIDDLE: f32 = 3.5;
const ALIGNMENT_INSET: f32 = 6.5;

/// The luma of every pixel word, into `out`.
pub fn to_luma(words: &[u32], format: PixelFormat, out: &mut Vec<u8>) {
    out.clear();
    out.extend(words.iter().map(|&word| {
        let channels = [format.r_shift, format.g_shift, format.b_shift].map(|shift| (word >> shift & 0xFF) as i32);
        jfif_luma::<WEIGHT_BITS>(channels)
    }));
}

/// Buffers kept between pictures: a repeated size takes no memory.
#[derive(Default)]
pub struct Scanner {
    plane: Plane,
    points: Vec<u32>,
}

impl Scanner {
    /// The bytes of the symbol a luma picture shows.
    pub fn scan(&mut self, light: &[u8], width: usize) -> Option<Vec<u8>> {
        if !self.plane.binarize(light, width, &mut self.points) {
            return None;
        }
        let plane = &self.plane;
        triples(&finders(plane)).into_iter().find_map(|three| read_three(plane, three))
    }
}

#[derive(Default)]
struct Plane {
    dark: Vec<bool>,
    width: usize,
    height: usize,
}

impl Plane {
    fn binarize(&mut self, light: &[u8], width: usize, points: &mut Vec<u32>) -> bool {
        let Some(height) = light.len().checked_div(width) else {
            return false;
        };
        let smallest = BLOCK * (2 * BLOCK_RADIUS + 1);
        if width < smallest || height < smallest {
            return false;
        }
        let (columns, rows) = (width.div_ceil(BLOCK), height.div_ceil(BLOCK));
        // The last block of a line or a column lies against the picture's edge.
        let corner =
            |column: usize, row: usize| ((column * BLOCK).min(width - BLOCK), (row * BLOCK).min(height - BLOCK));
        points.clear();
        points.resize(columns * rows, 0);
        for row in 0..rows {
            for column in 0..columns {
                let (left, top) = corner(column, row);
                let (mut sum, mut darkest, mut brightest) = (0u32, u8::MAX, 0u8);
                for line in light[top * width..].chunks(width).take(BLOCK) {
                    for &pixel in &line[left..left + BLOCK] {
                        (sum, darkest, brightest) = (sum + u32::from(pixel), darkest.min(pixel), brightest.max(pixel));
                    }
                }
                let mut point = sum / (BLOCK * BLOCK) as u32;
                if brightest - darkest <= FLAT_RANGE {
                    point = u32::from(darkest) / 2;
                    if row > 0 && column > 0 {
                        let neighbours = (points[(row - 1) * columns + column]
                            + 2 * points[row * columns + column - 1]
                            + points[(row - 1) * columns + column - 1])
                            / 4;
                        if u32::from(darkest) < neighbours {
                            point = neighbours;
                        }
                    }
                }
                points[row * columns + column] = point;
            }
        }
        self.dark.clear();
        self.dark.resize(width * height, false);
        (self.width, self.height) = (width, height);
        let span = 2 * BLOCK_RADIUS + 1;
        for row in 0..rows {
            for column in 0..columns {
                let (middle_column, middle_row) = (
                    column.clamp(BLOCK_RADIUS, columns - 1 - BLOCK_RADIUS),
                    row.clamp(BLOCK_RADIUS, rows - 1 - BLOCK_RADIUS),
                );
                let sum: u32 = (middle_row - BLOCK_RADIUS..=middle_row + BLOCK_RADIUS)
                    .flat_map(|row| points[row * columns..][middle_column - BLOCK_RADIUS..][..span].iter())
                    .sum();
                let threshold = sum / (span * span) as u32;
                let (left, top) = corner(column, row);
                for y in top..top + BLOCK {
                    for x in left..left + BLOCK {
                        self.dark[y * width + x] = u32::from(light[y * width + x]) <= threshold;
                    }
                }
            }
        }
        true
    }

    fn is_dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.width + x]
    }
}

#[derive(Clone, Copy, Debug)]
struct Finder {
    x: f32,
    y: f32,
    module: f32,
    sightings: u32,
}

impl Finder {
    const fn at(&self) -> (f32, f32) {
        (self.x, self.y)
    }
}

fn distance((x, y): (f32, f32), (u, v): (f32, f32)) -> f32 {
    sqrt((x - u) * (x - u) + (y - v) * (y - v))
}

fn floor(value: f32) -> Option<i32> {
    (!value.is_nan()).then(|| math::floor(value) as i32)
}

fn is_finder_like(runs: &[u32; 5]) -> bool {
    let total: u32 = runs.iter().sum();
    if total < FINDER_MODULES {
        return false;
    }
    let module = total as f32 / FINDER_MODULES as f32;
    runs.iter().zip(FINDER).all(|(&run, modules)| (run as f32 - modules * module).abs() < modules * module * TOLERANCE)
}

fn middle(runs: &[u32; 5], end: usize) -> f32 {
    end as f32 - runs[4] as f32 - runs[3] as f32 - runs[2] as f32 / 2.0
}

fn finders(plane: &Plane) -> Vec<Finder> {
    let mut finders = Vec::new();
    for y in 0..plane.height {
        let (mut runs, mut ring) = ([0u32; 5], 0);
        for x in 0..plane.width {
            if plane.is_dark(x, y) {
                // Even rings are dark.
                if ring % 2 == 1 {
                    ring += 1;
                }
                runs[ring] += 1;
            } else if ring % 2 == 1 {
                runs[ring] += 1;
            } else if ring == 4 {
                if is_finder_like(&runs) {
                    confirm_finder(plane, &runs, (x, y), &mut finders);
                }
                // The last two rings may begin another finder.
                (runs, ring) = ([runs[2], runs[3], runs[4], 1, 0], 3);
            } else {
                ring += 1;
                runs[ring] += 1;
            }
        }
        if ring == 4 && is_finder_like(&runs) {
            confirm_finder(plane, &runs, (plane.width, y), &mut finders);
        }
    }
    finders
}

fn confirm_finder(plane: &Plane, runs: &[u32; 5], (end, y): (usize, usize), finders: &mut Vec<Finder>) {
    let total: u32 = runs.iter().sum();
    let x = middle(runs, end);
    let Some(y) = cross_check(plane, (x as usize, y), false, runs[2], total) else {
        return;
    };
    let Some(x) = cross_check(plane, (x as usize, y as usize), true, runs[2], total) else {
        return;
    };
    let new = Finder { x, y, module: total as f32 / FINDER_MODULES as f32, sightings: 1 };
    let is_near = |existing: &&mut Finder| {
        (existing.x - new.x).abs() <= new.module
            && (existing.y - new.y).abs() <= new.module
            && (existing.module - new.module).abs() <= existing.module
    };
    match finders.iter_mut().find(is_near) {
        Some(existing) => {
            let (count, next) = (existing.sightings as f32, existing.sightings as f32 + 1.0);
            *existing = Finder {
                x: (existing.x * count + new.x) / next,
                y: (existing.y * count + new.y) / next,
                module: (existing.module * count + new.module) / next,
                sightings: existing.sightings + 1,
            };
        }
        None => finders.push(new),
    }
}

fn cross_check(plane: &Plane, (x, y): (usize, usize), is_row: bool, limit: u32, total: u32) -> Option<f32> {
    let (length, at) = if is_row { (plane.width, x) } else { (plane.height, y) };
    let is_dark = |position: usize| if is_row { plane.is_dark(position, y) } else { plane.is_dark(x, position) };
    // The modules alike, dark or light, from `position` on by `step`, `cap` of them at most: how many, and `position` left where they end.
    let run = |position: &mut isize, step: isize, dark: bool, cap: u32| {
        let mut count = 0;
        while count < cap && (0..length as isize).contains(position) && is_dark(*position as usize) == dark {
            (count, *position) = (count + 1, *position + step);
        }
        count
    };
    let mut runs = [0u32; 5];
    // Back from the middle: the centre, the light ring, the dark ring.
    let mut position = at as isize;
    runs[2] = run(&mut position, -1, true, u32::MAX);
    runs[1] = run(&mut position, -1, false, limit + 1);
    if position < 0 || runs[1] > limit {
        return None;
    }
    runs[0] = run(&mut position, -1, true, limit + 1);
    if runs[0] > limit {
        return None;
    }
    // On from the middle, the same, each ring a module shorter at most.
    let mut position = at as isize + 1;
    runs[2] += run(&mut position, 1, true, u32::MAX);
    runs[3] = run(&mut position, 1, false, limit);
    if position == length as isize || runs[3] >= limit {
        return None;
    }
    runs[4] = run(&mut position, 1, true, limit);
    if runs[4] >= limit {
        return None;
    }
    let sum: u32 = runs.iter().sum();
    let is_match = sum.abs_diff(total) as f32 <= total as f32 * CROSS_TOLERANCE;
    (is_match && is_finder_like(&runs)).then(|| middle(&runs, position as usize))
}

fn to_ordered([first, second, third]: [Finder; 3]) -> [Finder; 3] {
    let (a, b, c) =
        (distance(second.at(), third.at()), distance(first.at(), third.at()), distance(first.at(), second.at()));
    let (corner, right, below) = if a >= b && a >= c {
        (first, second, third)
    } else if b >= a && b >= c {
        (second, first, third)
    } else {
        (third, first, second)
    };
    // In a picture whose rows run down, the turn from the top right to the bottom left is clockwise.
    let turn = (right.x - corner.x) * (below.y - corner.y) - (right.y - corner.y) * (below.x - corner.x);
    if turn < 0.0 { [corner, below, right] } else { [corner, right, below] }
}

fn skew([corner, right, below]: &[Finder; 3]) -> Option<f32> {
    let (top, left, diagonal) =
        (distance(corner.at(), right.at()), distance(corner.at(), below.at()), distance(right.at(), below.at()));
    if top <= 0.0 || left <= 0.0 {
        return None;
    }
    let sides = (top - left).abs() / top.max(left);
    let angle = (diagonal * diagonal - top * top - left * left).abs() / (diagonal * diagonal);
    let modules = [corner.module, right.module, below.module];
    let (smallest, largest) = modules
        .iter()
        .fold((f32::MAX, 0.0f32), |(smallest, largest), &module| (smallest.min(module), largest.max(module)));
    let size = (largest - smallest) / largest;
    (sides <= SKEW && angle <= SKEW && size <= SKEW).then_some(sides + angle + size)
}

fn triples(finders: &[Finder]) -> Vec<[Finder; 3]> {
    let mut candidates: Vec<Finder> =
        finders.iter().copied().filter(|finder| finder.sightings >= LEAST_SIGHTINGS).collect();
    candidates.sort_by_key(|finder| Reverse(finder.sightings));
    candidates.truncate(MOST_FINDERS);
    let mut triples = Vec::new();
    for first in 0..candidates.len() {
        for second in first + 1..candidates.len() {
            for third in second + 1..candidates.len() {
                let three = to_ordered([candidates[first], candidates[second], candidates[third]]);
                if let Some(score) = skew(&three) {
                    triples.push((score, three));
                }
            }
        }
    }
    triples.sort_by(|first, second| first.0.total_cmp(&second.0));
    triples.into_iter().map(|(_, three)| three).collect()
}

#[derive(Clone, Copy, Debug)]
struct Perspective([[f32; 3]; 3]);

impl Perspective {
    fn from_square(quad: [(f32, f32); 4]) -> Option<Perspective> {
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = quad;
        let (dx3, dy3) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
        let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
        let denominator = dx1 * dy2 - dx2 * dy1;
        if denominator == 0.0 {
            return None;
        }
        let (g, h) = ((dx3 * dy2 - dx2 * dy3) / denominator, (dx1 * dy3 - dx3 * dy1) / denominator);
        Some(Perspective([
            [x1 - x0 + g * x1, x3 - x0 + h * x3, x0],
            [y1 - y0 + g * y1, y3 - y0 + h * y3, y0],
            [g, h, 1.0],
        ]))
    }

    fn inverse(self) -> Option<Perspective> {
        let m = self.0;
        let adjugate = [
            [
                m[1][1] * m[2][2] - m[1][2] * m[2][1],
                m[0][2] * m[2][1] - m[0][1] * m[2][2],
                m[0][1] * m[1][2] - m[0][2] * m[1][1],
            ],
            [
                m[1][2] * m[2][0] - m[1][0] * m[2][2],
                m[0][0] * m[2][2] - m[0][2] * m[2][0],
                m[0][2] * m[1][0] - m[0][0] * m[1][2],
            ],
            [
                m[1][0] * m[2][1] - m[1][1] * m[2][0],
                m[0][1] * m[2][0] - m[0][0] * m[2][1],
                m[0][0] * m[1][1] - m[0][1] * m[1][0],
            ],
        ];
        let determinant = m[0][0] * adjugate[0][0] + m[0][1] * adjugate[1][0] + m[0][2] * adjugate[2][0];
        (determinant != 0.0).then_some(Perspective(adjugate))
    }

    fn compose(self, first: Perspective) -> Perspective {
        let (m, n) = (self.0, first.0);
        Perspective(core::array::from_fn(|row| {
            core::array::from_fn(|column| (0..3).map(|at| m[row][at] * n[at][column]).sum())
        }))
    }

    fn from_quads(source: [(f32, f32); 4], target: [(f32, f32); 4]) -> Option<Perspective> {
        Some(Perspective::from_square(target)?.compose(Perspective::from_square(source)?.inverse()?))
    }

    fn apply(&self, (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let [a, b, c] = self.0;
        let w = c[0] * x + c[1] * y + c[2];
        (w != 0.0).then(|| ((a[0] * x + a[1] * y + a[2]) / w, (b[0] * x + b[1] * y + b[2]) / w))
    }
}

fn find_alignment(plane: &Plane, guess: (f32, f32), module: f32) -> Option<(f32, f32)> {
    let reach = ALIGNMENT_REACH * module;
    let span = |at: f32, length: usize| {
        let (first, last) = (floor(at - reach)?.max(0), floor(at + reach)?.min(length as i32 - 1));
        (first <= last).then_some((first as usize, last as usize))
    };
    let ((left, right), (top, bottom)) = (span(guess.0, plane.width)?, span(guess.1, plane.height)?);
    let is_module = |run: usize| (run as f32 - module).abs() < module * TOLERANCE;
    // The nearest middle so far with its distance off the guess, and the runs of a row: dark or light, where each begins, and how long it is.
    let mut best: Option<((f32, f32), f32)> = None;
    let mut runs: Vec<(bool, usize, usize)> = Vec::new();
    for y in top..=bottom {
        runs.clear();
        for x in left..=right {
            let is_dark = plane.is_dark(x, y);
            match runs.last_mut() {
                Some((run_dark, _, length)) if *run_dark == is_dark => *length += 1,
                _ => runs.push((is_dark, x, 1)),
            }
        }
        for at in 2..runs.len().saturating_sub(2) {
            let (is_dark, start, length) = runs[at];
            if !is_dark || !is_module(length) || !is_module(runs[at - 1].2) || !is_module(runs[at + 1].2) {
                continue;
            }
            let x = start as f32 + length as f32 / 2.0;
            let Some(center_y) = alignment_center(plane, (x as usize, y), &is_module) else {
                continue;
            };
            let offset = distance((x, center_y), guess);
            if best.is_none_or(|(_, nearest)| offset < nearest) {
                best = Some(((x, center_y), offset));
            }
        }
    }
    best.map(|(at, _)| at)
}

fn alignment_center(plane: &Plane, (x, y): (usize, usize), is_module: &impl Fn(usize) -> bool) -> Option<f32> {
    if !plane.is_dark(x, y) {
        return None;
    }
    let (mut top, mut bottom) = (y, y);
    while top > 0 && plane.is_dark(x, top - 1) {
        top -= 1;
    }
    while bottom + 1 < plane.height && plane.is_dark(x, bottom + 1) {
        bottom += 1;
    }
    let (mut above, mut below) = (top, bottom);
    while above > 0 && !plane.is_dark(x, above - 1) {
        above -= 1;
    }
    while below + 1 < plane.height && !plane.is_dark(x, below + 1) {
        below += 1;
    }
    let is_bounded = above > 0 && below + 1 < plane.height;
    (is_bounded && is_module(bottom + 1 - top) && is_module(top - above) && is_module(below - bottom))
        .then_some((top + bottom + 1) as f32 / 2.0)
}

fn sample_modules(plane: &Plane, perspective: &Perspective, side: usize) -> Option<Symbol> {
    let mut dark = Vec::with_capacity(side * side);
    let pixel = |at: f32, length: usize| {
        floor(at).filter(|at| (-1..=length as i32).contains(at)).map(|at| at.clamp(0, length as i32 - 1) as usize)
    };
    for y in 0..side {
        for x in 0..side {
            let (column, line) = perspective.apply((x as f32 + 0.5, y as f32 + 0.5))?;
            dark.push(plane.is_dark(pixel(column, plane.width)?, pixel(line, plane.height)?));
        }
    }
    Symbol::new(side, dark)
}

fn read_three(plane: &Plane, finders: [Finder; 3]) -> Option<Vec<u8>> {
    let [corner, right, below] = finders;
    let module = (corner.module + right.module + below.module) / 3.0;
    let modules = (distance(corner.at(), right.at()) + distance(corner.at(), below.at())) / 2.0 / module;
    let estimate = usize::try_from(round(modules)).ok()? + FINDER_MODULES as usize;
    let nearest = VERSIONS.clone().map(side).min_by_key(|side| side.abs_diff(estimate))?;
    let mut sides = Vec::from([nearest]);
    sides.extend(
        [nearest.checked_sub(4), Some(nearest + 4)].into_iter().flatten().filter(|side| to_version(*side).is_some()),
    );
    let mut next = 0;
    while let Some(&side) = sides.get(next) {
        next += 1;
        let Some(symbol) = sample_symbol(plane, &finders, module, side) else {
            continue;
        };
        if let Some(bytes) = read(&symbol) {
            return Some(bytes);
        }
        let version_side =
            (side >= self::side(FIRST_VERSION_INFO)).then(|| read_version(&symbol)).flatten().map(self::side);
        if let Some(version_side) = version_side.filter(|version_side| !sides.contains(version_side)) {
            sides.push(version_side);
        }
    }
    None
}

fn sample_symbol(plane: &Plane, [corner, right, below]: &[Finder; 3], module: f32, side: usize) -> Option<Symbol> {
    let version = to_version(side)?;
    let span = side as f32 - 2.0 * FINDER_MIDDLE;
    let (x_step, y_step) = (
        ((right.x - corner.x) / span, (right.y - corner.y) / span),
        ((below.x - corner.x) / span, (below.y - corner.y) / span),
    );
    // Where a point of the symbol lies if the picture only slants it, by the finders alone.
    let affine = |position: f32| {
        let offset = position - FINDER_MIDDLE;
        (corner.x + offset * (x_step.0 + y_step.0), corner.y + offset * (x_step.1 + y_step.1))
    };
    let (fourth, point) = if version == 1 {
        let far = side as f32 - FINDER_MIDDLE;
        (far, affine(far))
    } else {
        let inside = side as f32 - ALIGNMENT_INSET;
        let guess = affine(inside);
        (inside, find_alignment(plane, guess, module).unwrap_or(guess))
    };
    let far = side as f32 - FINDER_MIDDLE;
    let source = [(FINDER_MIDDLE, FINDER_MIDDLE), (far, FINDER_MIDDLE), (fourth, fourth), (FINDER_MIDDLE, far)];
    let perspective = Perspective::from_quads(source, [corner.at(), right.at(), point, below.at()])?;
    sample_modules(plane, &perspective, side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::qr::Level;
    use crate::codec::qr::QUIET_ZONE;
    use crate::codec::qr::make;

    fn scan(light: &[u8], width: usize) -> Option<Vec<u8>> {
        Scanner::default().scan(light, width)
    }

    fn picture(
        symbol: &Symbol,
        (width, height): (usize, usize),
        corners: [(f32, f32); 4],
        (dark, light): (u8, u8),
    ) -> Vec<u8> {
        let outer = (symbol.side() + 2 * QUIET_ZONE) as f32;
        let square = [(0.0, 0.0), (outer, 0.0), (outer, outer), (0.0, outer)];
        let to_symbol = Perspective::from_quads(corners, square).unwrap();
        (0..width * height)
            .map(|at| {
                let (x, y) = to_symbol.apply(((at % width) as f32 + 0.5, (at / width) as f32 + 0.5)).unwrap();
                let module = |at: f32| {
                    floor(at)
                        .and_then(|at| usize::try_from(at - QUIET_ZONE as i32).ok())
                        .filter(|at| *at < symbol.side())
                };
                match (module(x), module(y)) {
                    (Some(x), Some(y)) if symbol.is_dark(x, y) => dark,
                    _ => light,
                }
            })
            .collect()
    }

    fn blur(light: &[u8], width: usize, seed: usize) -> Vec<u8> {
        let height = light.len() / width;
        (0..width * height)
            .map(|at| {
                let (x, y) = (at % width, at / width);
                let mut sum = 0u32;
                for row in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                    for column in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                        sum += u32::from(light[row * width + column]);
                    }
                }
                let count = ((y + 1).min(height - 1) + 1 - y.saturating_sub(1))
                    * ((x + 1).min(width - 1) + 1 - x.saturating_sub(1));
                let mean = sum / count as u32;
                let falloff = (x + y) * 60 / (width + height);
                let grain = (at * 7919 + seed * 104_729) % 21;
                (mean as usize * (100 - falloff) / 100 + grain).min(255) as u8
            })
            .collect()
    }

    const KEY: &[u8] = b"sk-proj-Zx8kQ2mP7vR4tW1yB6nC3dF9gH5jL0aS-_eU2iO8pK4lM7zX1cV6bN3qW9eR5tY0uI2oP8aS4dF7gH1jK6lZ3xC9vB5nM0qW2eR8tY4uI7oP1aS6dF3gH9jK5lZ0xC2vB8nM4qW7eR1tY6uI3oP9aS5dF0gH";

    #[test]
    fn poses() {
        assert_eq!(KEY.len(), 164);
        let symbol = make(KEY, Level::Medium).unwrap();
        let size = (368, 368);
        let placings: [[(f32, f32); 4]; 6] = [
            [(40.0, 40.0), (330.0, 40.0), (330.0, 330.0), (40.0, 330.0)],
            [(330.0, 40.0), (330.0, 330.0), (40.0, 330.0), (40.0, 40.0)],
            [(60.0, 30.0), (340.0, 70.0), (300.0, 345.0), (20.0, 300.0)],
            [(70.0, 60.0), (300.0, 50.0), (330.0, 320.0), (40.0, 330.0)],
            [(330.0, 330.0), (40.0, 330.0), (40.0, 40.0), (330.0, 40.0)],
            [(40.0, 330.0), (40.0, 40.0), (330.0, 40.0), (330.0, 330.0)],
        ];
        for (placing, corners) in placings.iter().enumerate() {
            let light = picture(&symbol, size, *corners, (30, 220));
            assert_eq!(scan(&light, size.0).as_deref(), Some(KEY), "placing {placing}, sharp");
            assert_eq!(
                scan(&blur(&light, size.0, placing), size.0).as_deref(),
                Some(KEY),
                "placing {placing}, blurred"
            );
        }
        let mirror = [(330.0, 40.0), (40.0, 40.0), (40.0, 330.0), (330.0, 330.0)];
        assert_eq!(scan(&picture(&symbol, size, mirror, (30, 220)), size.0).as_deref(), Some(KEY), "in a mirror");
    }

    #[test]
    fn small_modules_and_dim() {
        for (bytes, level) in
            [(&b"r1"[..], Level::High), (&b"https://example.org/"[..], Level::Quartile), (KEY, Level::Low)]
        {
            let symbol = make(bytes, level).unwrap();
            let outer = (symbol.side() + 2 * QUIET_ZONE) as f32;
            for pixels in [3.0, 4.5, 7.0] {
                let width = (outer * pixels) as usize + 20;
                let corners = [
                    (10.0, 10.0),
                    (10.0 + outer * pixels, 10.0),
                    (10.0 + outer * pixels, 10.0 + outer * pixels),
                    (10.0, 10.0 + outer * pixels),
                ];
                let light = picture(&symbol, (width, width), corners, (70, 140));
                assert_eq!(
                    scan(&light, width).as_deref(),
                    Some(bytes),
                    "{} modules at {pixels} pixels each",
                    symbol.side()
                );
            }
        }
    }

    #[test]
    fn no_symbol() {
        let noise: Vec<u8> = (0..368 * 368).map(|at| ((at * 7919) % 251) as u8).collect();
        assert_eq!(scan(&noise, 368), None);
        assert_eq!(scan(&[128; 368 * 368], 368), None, "an even grey");
        assert_eq!(scan(&[0; 39 * 39], 39), None);
        assert_eq!(scan(&[], 0), None);
        let symbol = make(KEY, Level::Medium).unwrap();
        let half: Vec<u8> =
            picture(&symbol, (368, 368), [(40.0, 40.0), (330.0, 40.0), (330.0, 330.0), (40.0, 330.0)], (30, 220))
                .into_iter()
                .enumerate()
                .map(|(at, light)| if at % 368 > 184 { 220 } else { light })
                .collect();
        assert_eq!(scan(&half, 368), None, "half a symbol");
    }

    #[test]
    fn binarize_reuses_memory() {
        let (mut scanner, grey) = (Scanner::default(), [128u8; 368 * 368]);
        assert!(scanner.plane.binarize(&grey, 368, &mut scanner.points));
        let before = crate::base::alloc_count::allocations();
        assert!(scanner.plane.binarize(&grey, 368, &mut scanner.points));
        assert_eq!(crate::base::alloc_count::allocations(), before);
    }

    #[test]
    fn finder_limit() {
        let finder = |(x, y): (f32, f32), sightings: u32| Finder { x, y, module: 4.0, sightings };
        let real = [(1000.0, 1000.0), (1200.0, 1000.0), (1000.0, 1200.0)];
        let mut finders: Vec<Finder> =
            (0..64).map(|at| finder(((at % 8) as f32 * 50.0, (at / 8) as f32 * 50.0), LEAST_SIGHTINGS)).collect();
        finders.extend(real.map(|at| finder(at, 20)));
        let tried = triples(&finders);
        let mut corners: Vec<(f32, f32)> = tried.iter().flatten().map(Finder::at).collect();
        corners.sort_by(|first, second| first.0.total_cmp(&second.0).then(first.1.total_cmp(&second.1)));
        corners.dedup();
        assert!(corners.len() <= MOST_FINDERS, "{} finders tried", corners.len());
        assert!(tried.iter().any(|three| three.map(|finder| finder.at()) == real), "the symbol's own three among them");
    }

    #[test]
    fn perspective_and_luma() {
        let quad = [(12.0, 7.0), (90.0, 15.0), (80.0, 70.0), (5.0, 60.0)];
        let square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let forward = Perspective::from_square(quad).unwrap();
        let back = forward.inverse().unwrap();
        for (corner, (x, y)) in square.iter().zip(quad) {
            let (u, v) = forward.apply(*corner).unwrap();
            assert!((u - x).abs() < 1e-3 && (v - y).abs() < 1e-3, "{corner:?} goes to ({x}, {y})");
            let (s, t) = back.apply((x, y)).unwrap();
            assert!((s - corner.0).abs() < 1e-4 && (t - corner.1).abs() < 1e-4, "and comes back");
        }
        let between = Perspective::from_quads(quad, [(0.0, 0.0), (21.0, 0.0), (21.0, 21.0), (0.0, 21.0)]).unwrap();
        let (u, v) = between.apply(quad[2]).unwrap();
        assert!((u - 21.0).abs() < 1e-3 && (v - 21.0).abs() < 1e-3);
        assert!(Perspective::from_square([(0.0, 0.0); 4]).is_none(), "a square taken to a point");
        assert_eq!(
            (distance((0.0, 0.0), (3.0, 4.0)), floor(-0.5), floor(2.0), floor(f32::NAN), floor(-1.0e12)),
            (5.0, Some(-1), Some(2), None, Some(i32::MIN))
        );
        let mut light = Vec::from([9u8]);
        to_luma(&[0x00FF_FFFF, 0x0000_0000, 0x00FF_0000, 0x0000_FF00, 0x0000_00FF], PixelFormat::XRGB8888, &mut light);
        assert_eq!(light, [255, 0, 76, 150, 29], "JFIF's luma of white, black, red, green and blue");
    }
}
