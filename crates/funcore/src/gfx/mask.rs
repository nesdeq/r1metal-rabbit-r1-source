//! Antialiased outline fill by the nonzero rule.

use super::Rect;
use super::key::Key;
use crate::base::math::ceil;
use crate::base::math::floor;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

const SPILL: usize = 2;
pub(crate) const SHADES: usize = 256;
const CHUNK: usize = 64 / size_of::<f32>();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Strip {
    pub(super) row: u16,
    pub(super) left: u16,
    pub(super) right: u16,
    pub(super) shade: u8,
}

fn next_change(changes: &[f32], from: usize) -> usize {
    let rest = &changes[from.min(changes.len())..];
    let none = |chunk: &&[f32; CHUNK]| chunk.iter().fold(0, |any, change| any | change.to_bits()) == 0;
    let zeros = rest.as_chunks::<CHUNK>().0.iter().take_while(none).count() * CHUNK;
    from + zeros + rest[zeros..].iter().position(|change| change.to_bits() != 0).unwrap_or(rest.len() - zeros)
}

fn shade(covered: f32) -> u8 {
    (covered.abs().min(1.0) * (SHADES - 1) as f32 + 0.5) as u8
}

pub(super) fn each_run(first: usize, changes: &[f32], mut paint: impl FnMut(usize, usize, u8)) {
    let (mut covered, mut at) = (0.0f32, next_change(changes, 0));
    while at < changes.len() {
        covered += changes[at];
        // An edge that crosses several cells changes the next one too, which is looked at before a line of them is.
        let next = match changes.get(at + 1) {
            Some(change) if change.to_bits() != 0 => at + 1,
            _ => next_change(changes, at + 1),
        };
        paint(first + at, first + next, shade(covered));
        at = next;
    }
}

const ROUNDING: f32 = 1.0 / 1024.0;

#[derive(Clone, Copy)]
struct Run {
    first: usize,
    last: usize,
    way: i32,
}

/// Per-pixel coverage of outlines.
pub struct Mask {
    width: usize,
    height: usize,
    stride: usize,
    cells: Vec<f32>,
    rows: Range<usize>,
    spans: Vec<Range<usize>>,
    runs: Vec<Run>,
    points: Vec<(f32, f32)>,
    is_filled: bool,
    slopes: Vec<f32>,
    stops: Vec<f32>,
    pending: Vec<usize>,
    order: Vec<usize>,
    at: Vec<usize>,
    pub(super) key: Key,
    shift: f32,
}

impl Default for Mask {
    fn default() -> Self {
        Mask::new(0, 0)
    }
}

impl Mask {
    pub fn new(width: usize, height: usize) -> Self {
        let stride = width + SPILL;
        Mask {
            width,
            height,
            stride,
            cells: vec![0.0; stride * height],
            rows: height..0,
            spans: vec![stride..0; height],
            runs: Vec::new(),
            points: Vec::new(),
            is_filled: true,
            slopes: Vec::new(),
            stops: Vec::new(),
            pending: Vec::new(),
            order: Vec::new(),
            at: Vec::new(),
            key: Key::default(),
            shift: 0.0,
        }
    }

    pub(super) const fn shift(&mut self, by: f32) {
        self.shift = by;
    }

    pub(crate) fn reserve(&mut self, points: usize) {
        self.points.reserve(2 * points);
        self.slopes.reserve(2 * points);
        self.stops.reserve(2 * points);
        self.runs.reserve(points);
        self.pending.reserve(points);
        self.order.reserve(points);
        self.at.reserve(points);
    }

    pub(crate) fn clear(&mut self) {
        self.wipe();
        self.runs.clear();
        self.points.clear();
        self.is_filled = true;
        self.key = Key::default();
    }

    fn wipe(&mut self) {
        for row in self.rows.start..self.rows.end.max(self.rows.start) {
            let span = core::mem::replace(&mut self.spans[row], self.stride..0);
            if !span.is_empty() {
                self.cells[row * self.stride..][span].fill(0.0);
            }
        }
        self.rows = self.height..0;
    }

    fn close(&mut self, open: &mut Option<Run>) {
        if let Some(run) = open.take() {
            if run.way < 0 {
                self.points[run.first..=run.last].reverse();
            }
            self.runs.push(run);
        }
    }

    fn extend(&mut self, open: &mut Option<Run>, from: (f32, f32), to: (f32, f32)) {
        if !(from.0.is_finite() && from.1.is_finite() && to.0.is_finite() && to.1.is_finite()) || from.1 == to.1 {
            // A level piece or one with no finite coordinate bounds nothing visible.
            return self.close(open);
        }
        let way = if from.1 < to.1 { 1 } else { -1 };
        if let Some(run) = open.as_mut().filter(|run| run.way == way) {
            // A collinear piece lengthens the one before, so a straight outline in pieces stays one piece.
            let (before, end) = (self.points[run.last - 1], self.points[run.last]);
            let (across, down) = (to.0 - before.0, to.1 - before.1);
            if ((end.0 - before.0) * down - (end.1 - before.1) * across).abs() <= ROUNDING * (across.abs() + down.abs())
            {
                self.points[run.last] = to;
            } else {
                self.points.push(to);
                run.last += 1;
            }
            return;
        }
        self.close(open);
        let first = self.points.len();
        self.points.extend([from, to]);
        *open = Some(Run { first, last: first + 1, way });
        self.is_filled = false;
    }

    #[cfg(test)]
    fn edge(&mut self, from: (f32, f32), to: (f32, f32)) {
        let (from, to) = ((from.0, from.1 + self.shift), (to.0, to.1 + self.shift));
        self.key = self.key.word(0).point(from).point(to);
        let mut open = None;
        self.extend(&mut open, from, to);
        self.close(&mut open);
    }

    /// Adds a closed outline, the last point joined to the first.
    pub fn outline(&mut self, points: impl DoubleEndedIterator<Item = (f32, f32)> + Clone) {
        let shift = self.shift;
        let points = points.map(move |(x, y)| (x, y + shift));
        let Some(mut from) = points.clone().next_back() else {
            return;
        };
        let (mut open, mut key, mut count) = (None, self.key, 0u64);
        for to in points {
            self.extend(&mut open, from, to);
            (key, count, from) = (key.point(to), count + 1, to);
        }
        self.key = key.word(count);
        self.close(&mut open);
    }

    fn ends(&self, run: usize) -> (f32, f32) {
        let Run { first, last, .. } = self.runs[run];
        (self.points[first].1, self.points[last].1)
    }

    fn x(&self, piece: usize, y: f32) -> f32 {
        let from = self.points[piece];
        from.0 + (y - from.1) * self.slopes[piece]
    }

    fn across(&self, run: usize, mut piece: usize, y: f32) -> (f32, usize) {
        while piece + 1 < self.runs[run].last && self.points[piece + 1].1 <= y {
            piece += 1;
        }
        (self.x(piece, y), piece)
    }

    pub(super) fn fill(&mut self) {
        if self.is_filled {
            return;
        }
        self.is_filled = true;
        self.wipe();
        self.slopes.clear();
        self.slopes.resize(self.points.len(), 0.0);
        for run in &self.runs {
            for piece in run.first..run.last {
                let (from, to) = (self.points[piece], self.points[piece + 1]);
                self.slopes[piece] = (to.0 - from.0) / (to.1 - from.1);
            }
        }
        let floor = self.height as f32;
        let (mut stops, mut pending, mut order, mut at) = (
            core::mem::take(&mut self.stops),
            core::mem::take(&mut self.pending),
            core::mem::take(&mut self.order),
            core::mem::take(&mut self.at),
        );
        stops.clear();
        stops.extend((0..self.runs.len()).flat_map(|run| {
            let (top, bottom) = self.ends(run);
            [top.clamp(0.0, floor), bottom.clamp(0.0, floor)]
        }));
        stops.sort_unstable_by(f32::total_cmp);
        stops.dedup();
        pending.clear();
        pending.extend(0..self.runs.len());
        pending.sort_unstable_by(|one, other| self.ends(*other).0.total_cmp(&self.ends(*one).0));
        at.clear();
        at.extend(self.runs.iter().map(|run| run.first));
        order.clear();
        for band in stops.windows(2) {
            let (mut top, bottom) = (band[0], band[1]);
            order.retain(|&run| self.ends(run).1 > top);
            for run in order.iter().copied() {
                at[run] = self.across(run, at[run], top).1;
            }
            while let Some(&run) = pending.last().filter(|run| self.ends(**run).0 <= top) {
                pending.pop();
                if self.ends(run).1 <= top {
                    continue;
                }
                let (x, piece) = self.across(run, at[run], top);
                at[run] = piece;
                let place = order.partition_point(|&other| {
                    let (there, along) = self.across(other, at[other], top);
                    (there, self.slopes[along]) < (x, self.slopes[piece])
                });
                order.insert(place, run);
            }
            // The band ends where two neighbours cross, and they swap; the count of swaps bounds the loop where rounding keeps pairs crossing.
            let mut swaps = 0;
            loop {
                let mut cut = (bottom, 0);
                if swaps < order.len() * order.len() {
                    for place in 1..order.len() {
                        if let Some(meets) = self.crossing(order[place - 1], order[place], &at, top, cut.0) {
                            cut = (meets, place);
                        }
                    }
                }
                self.band(&order, &at, top, cut.0);
                if cut.1 == 0 {
                    break;
                }
                order.swap(cut.1 - 1, cut.1);
                (top, swaps) = (cut.0, swaps + 1);
                for run in order.iter().copied() {
                    at[run] = self.across(run, at[run], top).1;
                }
            }
        }
        (self.stops, self.pending, self.order, self.at) = (stops, pending, order, at);
    }

    fn crossing(&self, left: usize, right: usize, at: &[usize], top: f32, bottom: f32) -> Option<f32> {
        let (mut one, mut other, mut y) = (at[left], at[right], top);
        let apart = |one: usize, other: usize, y: f32| self.x(one, y) - self.x(other, y);
        let mut before = apart(one, other, y);
        if before > ROUNDING {
            return Some(top);
        }
        loop {
            let next = self.points[one + 1].1.min(self.points[other + 1].1).min(bottom);
            if next <= y {
                return None;
            }
            let after = apart(one, other, next);
            if after > ROUNDING {
                // A height's last bit moves a nearly level run by more than the rounding: the cut goes down until the pair, swapped, does not read as crossed back.
                let mut meets = y + (next - y) * (ROUNDING - before) / (after - before);
                while meets < next && apart(one, other, meets) < -ROUNDING {
                    meets = meets.next_up();
                }
                return Some(meets);
            }
            (y, before) = (next, after);
            if self.points[one + 1].1 <= y && one + 1 < self.runs[left].last {
                one += 1;
            }
            if self.points[other + 1].1 <= y && other + 1 < self.runs[right].last {
                other += 1;
            }
        }
    }

    fn band(&mut self, order: &[usize], at: &[usize], top: f32, bottom: f32) {
        if bottom <= top {
            return;
        }
        let (mut winding, mut left) = (0, 0);
        for &run in order {
            let before = winding;
            winding += self.runs[run].way;
            if before == 0 {
                left = run;
            } else if winding == 0 {
                self.side(left, at[left], top, bottom, true);
                self.side(run, at[run], top, bottom, false);
            }
        }
    }

    fn side(&mut self, run: usize, mut piece: usize, top: f32, bottom: f32, is_left: bool) {
        while piece < self.runs[run].last && self.points[piece].1 < bottom {
            let (upper, lower) = (self.points[piece], self.points[piece + 1]);
            let from = if upper.1 >= top { upper } else { (self.x(piece, top), top) };
            let to = if lower.1 <= bottom { lower } else { (self.x(piece, bottom), bottom) };
            if to.1 > from.1 {
                let slope = self.slopes[piece];
                if is_left { self.piece(from, to, slope) } else { self.piece(to, from, slope) }
            }
            piece += 1;
        }
    }

    fn piece(&mut self, from: (f32, f32), to: (f32, f32), slope: f32) {
        let right = self.width as f32;
        if from.0.min(to.0) >= 0.0 && from.0.max(to.0) <= right {
            return self.accumulate(from, to, slope);
        }
        let (across, down) = (to.0 - from.0, to.1 - from.1);
        let mut cuts = [0.0, 1.0, 1.0, 1.0];
        let mut count = 1;
        for border in [0.0, right] {
            let share = (border - from.0) / across;
            if share > 0.0 && share < 1.0 {
                cuts[count] = share;
                count += 1;
            }
        }
        if count == 3 && cuts[1] > cuts[2] {
            cuts.swap(1, 2);
        }
        let point = |share: f32| if share >= 1.0 { to } else { (from.0 + across * share, from.1 + down * share) };
        for part in 0..count {
            let (start, end) = (point(cuts[part]), point(cuts[part + 1]));
            let middle = (start.0 + end.0) / 2.0;
            if middle < 0.0 {
                self.accumulate((0.0, start.1), (0.0, end.1), 0.0);
            } else if middle > right {
                self.accumulate((right, start.1), (right, end.1), 0.0);
            } else {
                self.accumulate((start.0.clamp(0.0, right), start.1), (end.0.clamp(0.0, right), end.1), slope);
            }
        }
    }

    fn accumulate(&mut self, from: (f32, f32), to: (f32, f32), slope: f32) {
        if from.1 == to.1 {
            return;
        }
        let (direction, upper, lower) = if from.1 < to.1 { (1.0, from, to) } else { (-1.0, to, from) };
        let (least, most) = (upper.0.min(lower.0), upper.0.max(lower.0));
        let across = |y: f32| (upper.0 + (y - upper.1) * slope).clamp(least, most);
        let first = upper.1.max(0.0) as usize;
        let end = (ceil(lower.1).max(0.0) as usize).min(self.height);
        if first >= end {
            return;
        }
        self.rows = self.rows.start.min(first)..self.rows.end.max(end);
        for row in first..end {
            let line = row * self.stride;
            let (high, low) = ((row as f32).max(upper.1), ((row + 1) as f32).min(lower.1));
            let (x, next) = (across(high), across(low));
            let area = (low - high) * direction;
            let (left, far) = if x < next { (x, next) } else { (next, x) };
            let left_cell = floor(left);
            let far_cell = ceil(far);
            let (start, stop) = (left_cell as usize, far_cell as usize);
            let span = &mut self.spans[row];
            *span = span.start.min(start)..span.end.max(stop.max(start + 1) + 1);
            if stop <= start + 1 {
                let middle = 0.5 * (x + next) - left_cell;
                self.cells[line + start] += area - area * middle;
                self.cells[line + start + 1] += area * middle;
            } else {
                let per_cell = 1.0 / (far - left);
                let into_first = left - left_cell;
                let first_share = 0.5 * per_cell * (1.0 - into_first) * (1.0 - into_first);
                let into_last = far - far_cell + 1.0;
                let last_share = 0.5 * per_cell * into_last * into_last;
                self.cells[line + start] += area * first_share;
                if stop == start + 2 {
                    self.cells[line + start + 1] += area * (1.0 - first_share - last_share);
                } else {
                    let second_share = per_cell * (1.5 - into_first);
                    self.cells[line + start + 1] += area * (second_share - first_share);
                    let whole = area * per_cell;
                    self.cells[line + start + 2..line + stop - 1].iter_mut().for_each(|cell| *cell += whole);
                    let before_last = second_share + (stop - start - 3) as f32 * per_cell;
                    self.cells[line + stop - 1] += area * (1.0 - before_last - last_share);
                }
                self.cells[line + stop] += area * last_share;
            }
        }
    }

    pub(super) fn rows(&self) -> Range<usize> {
        debug_assert!(self.is_filled, "a mask is read once it is filled");
        self.rows.start..self.rows.end.max(self.rows.start)
    }

    pub(super) fn changes(&self, row: usize) -> (usize, &[f32]) {
        debug_assert!(self.is_filled, "a mask is read once it is filled");
        let span = &self.spans[row];
        let end = span.end.min(self.width);
        let start = span.start.min(end);
        (start, &self.cells[row * self.stride..][start..end])
    }

    pub(super) fn strips(&mut self, into: &mut Vec<Strip>) -> Option<Rect> {
        self.fill();
        let (mut left, mut right, mut rows) = (usize::MAX, 0, None::<(usize, usize)>);
        let width = self.width;
        for row in self.rows() {
            let span = core::mem::replace(&mut self.spans[row], self.stride..0);
            if span.is_empty() {
                continue;
            }
            let cells = &mut self.cells[row * self.stride..][span.clone()];
            let visible = span.end.min(width).saturating_sub(span.start);
            each_run(span.start, &cells[..visible], |from, to, shade| {
                let to = to.min(width);
                if shade == 0 || from >= to {
                    return;
                }
                (left, right) = (left.min(from), right.max(to));
                rows = Some(rows.map_or((row, row), |(top, _)| (top, row)));
                match into.last_mut() {
                    Some(strip)
                        if usize::from(strip.row) == row
                            && usize::from(strip.right) == from
                            && strip.shade == shade =>
                    {
                        strip.right = to as u16;
                    }
                    _ => into.push(Strip { row: row as u16, left: from as u16, right: to as u16, shade }),
                }
            });
            cells.fill(0.0);
        }
        self.rows = self.height..0;
        rows.map(|(top, bottom)| Rect::new(left as i32, top as i32, (right - left) as i32, (bottom + 1 - top) as i32))
    }

    #[cfg(test)]
    pub(super) fn coverage(&self, row: usize) -> impl Iterator<Item = f32> + '_ {
        debug_assert!(self.is_filled, "a mask is read once it is filled");
        let line = row * self.stride;
        self.cells[line..line + self.width].iter().scan(0.0f32, |sum, cell| {
            *sum += cell;
            Some(sum.clamp(0.0, 1.0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::support::close;
    use super::*;

    fn outline(mask: &mut Mask, corners: &[(f32, f32)]) {
        mask.outline(corners.iter().copied());
        mask.fill();
    }

    fn total(mask: &Mask) -> f32 {
        mask.rows().map(|row| mask.coverage(row).sum::<f32>()).sum()
    }

    #[test]
    fn grid_square_full_coverage() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(2.0, 1.0), (5.0, 1.0), (5.0, 4.0), (2.0, 4.0)]);
        assert_eq!(mask.rows(), 1..4);
        let row: Vec<f32> = mask.coverage(2).collect();
        assert_eq!(row, [0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        assert!(close(total(&mask), 9.0));
    }

    #[test]
    fn offset_square_partial_coverage() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(2.5, 1.5), (4.5, 1.5), (4.5, 3.5), (2.5, 3.5)]);
        let top: Vec<f32> = mask.coverage(1).collect();
        let middle: Vec<f32> = mask.coverage(2).collect();
        assert_eq!(top, [0.0, 0.0, 0.25, 0.5, 0.25, 0.0, 0.0, 0.0]);
        assert_eq!(middle, [0.0, 0.0, 0.5, 1.0, 0.5, 0.0, 0.0, 0.0]);
        assert!(close(total(&mask), 4.0));
    }

    #[test]
    fn slanted_edges_cover_by_area() {
        let mut mask = Mask::new(16, 16);
        outline(&mut mask, &[(1.0, 1.0), (13.0, 5.0), (3.0, 14.0)]);
        assert!(close(total(&mask), 74.0));
        assert!(mask.rows().all(|row| mask.coverage(row).all(|share| (0.0..=1.0).contains(&share))));
    }

    #[test]
    fn outline_and_edges_agree() {
        let corners: Vec<(f32, f32)> = (0..64)
            .map(|step| {
                let (sine, cosine) = crate::base::math::sine_cosine(step as f64 / 64.0);
                ((8.0 + 6.0 * cosine) as f32, (8.0 + 6.0 * sine) as f32)
            })
            .collect();
        let mut whole = Mask::new(16, 16);
        outline(&mut whole, &corners);
        let mut edges = Mask::new(16, 16);
        for (from, to) in corners.iter().zip(corners.iter().cycle().skip(1)) {
            edges.edge(*from, *to);
        }
        edges.fill();
        let polygon =
            corners.iter().zip(corners.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum::<f32>();
        assert!(close(total(&whole), polygon.abs() / 2.0), "a polygon of 64 corners: {}", total(&whole));
        assert!(
            (0..16).all(|row| whole.coverage(row).zip(edges.coverage(row)).all(|(one, other)| close(one, other))),
            "the same whether its pieces come as one outline or one by one"
        );
    }

    #[test]
    fn reverse_outline_cuts_hole() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(1.0, 1.0), (1.0, 7.0), (7.0, 7.0), (7.0, 1.0)]);
        assert!(close(total(&mask), 36.0));
        outline(&mut mask, &[(3.0, 3.0), (5.0, 3.0), (5.0, 5.0), (3.0, 5.0)]);
        assert!(close(total(&mask), 32.0));
        assert_eq!(mask.coverage(4).nth(4), Some(0.0));
    }

    #[test]
    fn overlaps_covered_once() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(1.0, 1.5), (5.0, 1.5), (5.0, 5.5), (1.0, 5.5)]);
        outline(&mut mask, &[(3.0, 3.5), (7.0, 3.5), (7.0, 7.5), (3.0, 7.5)]);
        assert!(close(total(&mask), 28.0), "two squares that overlap: {}", total(&mask));
        assert_eq!(mask.coverage(1).nth(2), Some(0.5), "half a pixel at the top edge");
        assert_eq!(mask.coverage(3).nth(4), Some(1.0), "and where both are, whole");
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(0.5, 0.5), (0.5, 7.5), (7.5, 7.5), (7.5, 0.5)]);
        outline(&mut mask, &[(2.5, 2.5), (5.5, 2.5), (5.5, 5.5), (2.5, 5.5)]);
        outline(&mut mask, &[(2.5, 2.5), (5.5, 2.5), (5.5, 5.5), (2.5, 5.5)]);
        assert!(
            close(total(&mask), 49.0) && (1..7).all(|row| mask.coverage(row).skip(1).take(6).all(|share| share == 1.0)),
            "an island that fills a hole exactly, running the way the hole runs: no seam where they meet"
        );
        let mut mask = Mask::new(9, 9);
        outline(&mut mask, &[(0.5, 0.5), (8.0, 0.5), (0.5, 8.0), (8.0, 8.0)]);
        assert!(
            close(total(&mask), 2.0 * 7.5 * 3.75 / 2.0),
            "an outline that crosses itself inside a pixel: two triangles, each the other way round, {}",
            total(&mask)
        );
        let mut mask = Mask::new(16, 16);
        outline(&mut mask, &[(1.0, 1.0), (9.0, 1.0), (9.0, 15.0), (1.0, 15.0)]);
        outline(&mut mask, &[(5.5, 8.0), (15.0, 2.5), (15.0, 13.5)]);
        assert!(
            close(total(&mask), 8.0 * 14.0 + 9.5 * 11.0 / 2.0 - 3.5 * 3.5 * 11.0 / 9.5 / 2.0),
            "a triangle whose point is inside a square, two of its sides crossing the square's, curving runs and all: {}",
            total(&mask)
        );
    }

    #[test]
    fn nearly_level_crossing() {
        let mut mask = Mask::new(360, 490);
        for step in 0..64 {
            let edge = 244.0 + step as f32 / 64.0;
            mask.clear();
            outline(&mut mask, &[(178.0, 463.0), (edge, 463.0), (edge, 506.0), (178.0, 506.0)]);
            outline(&mut mask, &[(234.71, 471.856), (353.38, 471.964), (353.37, 485.35), (234.7, 485.244)]);
            assert_eq!(
                mask.coverage(478).nth(300),
                Some(1.0),
                "a bar whose nearly level edge leaves a box at {edge} is covered past the box"
            );
        }
    }

    #[test]
    fn clips_and_keeps_left_slope() {
        let mut mask = Mask::new(4, 4);
        outline(&mut mask, &[(-10.0, -10.0), (20.0, -10.0), (20.0, 2.0), (-10.0, 2.0)]);
        assert_eq!(mask.rows(), 0..2);
        let row: Vec<f32> = mask.coverage(1).collect();
        assert_eq!(row, [1.0, 1.0, 1.0, 1.0]);
        assert!(close(total(&mask), 8.0));
        assert!(mask.coverage(2).all(|share| share == 0.0));
        mask.clear();
        outline(&mut mask, &[(-4.0, 0.0), (4.0, 4.0), (-4.0, 4.0)]);
        let row: Vec<f32> = mask.coverage(2).collect();
        assert!(
            close(total(&mask), 4.0) && close(row[0], 0.75) && close(row[1], 0.25) && row[2] == 0.0,
            "a triangle through the left border shows the part inside it: {} {row:?}",
            total(&mask)
        );
    }

    #[test]
    fn row_changes_sum_to_coverage() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(2.5, 1.0), (5.0, 1.0), (5.0, 4.0), (2.5, 4.0)]);
        let (first, changes) = mask.changes(2);
        assert_eq!(first, 2);
        assert_eq!(changes, [0.5, 0.5, 0.0, -1.0, 0.0]);
        assert_eq!(mask.changes(6), (0, &[][..]));
        mask.clear();
        assert_eq!(mask.changes(2), (0, &[][..]));
    }

    #[test]
    fn clear_empties_used_rows() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(1.0, 0.0), (3.0, 0.0), (3.0, 2.0), (1.0, 2.0)]);
        outline(&mut mask, &[(1.0, 6.0), (3.0, 6.0), (3.0, 8.0), (1.0, 8.0)]);
        assert_eq!(mask.rows(), 0..8);
        assert_eq!(mask.changes(4), (0, &[][..]));
        assert!(close(total(&mask), 8.0));
        mask.clear();
        assert!(mask.rows().is_empty());
        assert!((0..8).all(|row| mask.coverage(row).all(|share| share == 0.0)));
        outline(&mut mask, &[(0.0, 3.0), (1.0, 3.0), (1.0, 4.0), (0.0, 4.0)]);
        assert!(close(total(&mask), 1.0), "and a cleared mask is filled anew");
    }

    #[test]
    fn changes_end_at_right_border() {
        let mut mask = Mask::new(4, 2);
        outline(&mut mask, &[(1.0, 0.0), (9.0, 0.0), (9.0, 2.0), (1.0, 2.0)]);
        assert_eq!(mask.changes(1), (1, &[1.0, 0.0, 0.0][..]));
    }

    #[test]
    fn empty_mask_stays_empty() {
        let mut mask = Mask::new(0, 0);
        outline(&mut mask, &[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)]);
        assert!(mask.rows().is_empty());
        mask.clear();
    }

    #[test]
    fn shifted_mask_moves_down() {
        let corners = [(1.5, 0.5), (6.0, 1.0), (4.0, 5.5)];
        let mut lower = Mask::new(8, 12);
        lower.shift(4.0);
        outline(&mut lower, &corners);
        let mut moved = Mask::new(8, 12);
        outline(&mut moved, &corners.map(|(x, y)| (x, y + 4.0)));
        assert!((0..12).all(|row| lower.coverage(row).eq(moved.coverage(row))));
        assert!(lower.key == moved.key, "and its key is the key of the points it covers");
    }

    #[test]
    fn edges_after_fill() {
        let mut mask = Mask::new(8, 8);
        outline(&mut mask, &[(1.5, 1.5), (3.5, 1.5), (3.5, 3.5), (1.5, 3.5)]);
        outline(&mut mask, &[(1.5, 1.5), (3.5, 1.5), (3.5, 3.5), (1.5, 3.5)]);
        outline(&mut mask, &[(5.0, 5.0), (7.0, 5.0), (7.0, 7.0), (5.0, 7.0)]);
        assert!(close(total(&mask), 8.0), "the same square twice is covered once, beside another: {}", total(&mask));
        assert_eq!(mask.coverage(1).nth(1), Some(0.25), "its corner a quarter of a pixel, not half");
    }
}
