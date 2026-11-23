use super::Rect;
use alloc::vec::Vec;
use core::ops::Range;

pub(super) struct Region {
    size: Rect,
    grain: usize,
    bits: Vec<u64>,
    rows: Range<usize>,
    bands: Vec<(i32, i32)>,
}

impl Default for Region {
    fn default() -> Self {
        Region::new(Rect::default())
    }
}

const fn ones(from: usize, to: usize) -> u64 {
    let width = to - from;
    (if width >= u64::BITS as usize { u64::MAX } else { (1 << width) - 1 }) << from
}

impl Region {
    pub(super) fn new(size: Rect) -> Region {
        let (width, rows) = (size.w.max(0) as usize, size.h.max(0) as usize);
        let mut grain = 1;
        while width.div_ceil(grain) > u64::BITS as usize {
            grain *= 2;
        }
        Region { size, grain, bits: alloc::vec![0; rows], rows: 0..0, bands: Vec::with_capacity(rows) }
    }

    fn used_rows(&self) -> &[u64] {
        &self.bits[self.rows.clone()]
    }

    fn extend(&mut self, rows: Range<usize>) {
        self.rows =
            if self.rows.is_empty() { rows } else { self.rows.start.min(rows.start)..self.rows.end.max(rows.end) };
    }

    pub(super) fn clear(&mut self) {
        self.bits[self.rows.clone()].fill(0);
        self.rows = 0..0;
        self.bands.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.used_rows().iter().all(|word| *word == 0)
    }

    pub(super) fn has_bands(&self) -> bool {
        !self.bands.is_empty()
    }

    #[cfg(test)]
    pub(super) fn pixels(&self) -> usize {
        let width = self.size.w.max(0) as usize;
        let last = width.saturating_sub(1) / self.grain;
        let past = (last + 1) * self.grain - width;
        let pixels =
            |word: &u64| word.count_ones() as usize * self.grain - if word >> last & 1 == 1 { past } else { 0 };
        self.used_rows().iter().map(pixels).sum()
    }

    pub(super) fn add(&mut self, rect: Rect) {
        let Some(visible) = rect.clip(self.size) else {
            return;
        };
        let (from, to) = ((visible.x - self.size.x) as usize, (visible.right() - self.size.x) as usize);
        let groups = ones(from / self.grain, to.div_ceil(self.grain));
        let rows = (visible.y - self.size.y) as usize..(visible.bottom() - self.size.y) as usize;
        self.bits[rows.clone()].iter_mut().for_each(|word| *word |= groups);
        self.extend(rows);
    }

    pub(super) fn add_whole(&mut self) {
        let size = self.size;
        self.add(size);
    }

    pub(super) fn add_region(&mut self, other: &Region) {
        self.bits[other.rows.clone()].iter_mut().zip(other.used_rows()).for_each(|(word, added)| *word |= added);
        if !other.rows.is_empty() {
            self.extend(other.rows.clone());
        }
    }

    fn set_masked(&mut self, one: &Region, other: &Region, mask: impl Fn(u64) -> u64) {
        self.clear();
        for row in one.rows.clone() {
            self.bits[row] = one.bits[row] & mask(other.bits[row]);
        }
        self.rows = one.rows.clone();
    }

    pub(super) fn set_inside(&mut self, one: &Region, other: &Region) {
        self.set_masked(one, other, |word| word);
    }

    pub(super) fn set_outside(&mut self, one: &Region, other: &Region) {
        self.set_masked(one, other, |word| !word);
    }

    pub(super) fn find_bands(&mut self) {
        self.bands.clear();
        let mut rows = self.bits[self.rows.clone()].iter().peekable();
        let mut y = self.rows.start;
        while let Some(&word) = rows.next() {
            let mut height = 1;
            while rows.next_if(|next| **next == word).is_some() {
                height += 1;
            }
            if word != 0 {
                self.bands.push((self.size.y + y as i32, height as i32));
            }
            y += height;
        }
    }

    pub(super) fn parts(&self, bounds: Rect) -> impl Iterator<Item = Rect> + '_ {
        let rows = bounds.clip(self.size);
        let first =
            rows.map_or(self.bands.len(), |rows| self.bands.partition_point(|(top, height)| top + height <= rows.y));
        let bands =
            self.bands[first..].iter().take_while(move |(top, _)| rows.is_some_and(|rows| *top < rows.bottom()));
        bands.flat_map(move |&(top, height)| {
            let rows = rows.unwrap_or(bounds);
            let (y, bottom) = (top.max(rows.y), (top + height).min(rows.bottom()));
            self.runs(top, rows.x, rows.right()).map(move |(left, right)| Rect::new(left, y, right - left, bottom - y))
        })
    }

    fn runs(&self, y: i32, from: i32, to: i32) -> impl Iterator<Item = (i32, i32)> + '_ {
        let (origin, grain) = (self.size.x, self.grain);
        let width = self.size.w.max(0) as usize;
        let (low, high) = ((from - origin).max(0) as usize, ((to - origin).max(0) as usize).min(width));
        let row = self.bits[(y - self.size.y) as usize];
        let mut word = if low < high { row & ones(low / grain, (high - 1) / grain + 1) } else { 0 };
        core::iter::from_fn(move || {
            if word == 0 {
                return None;
            }
            let start = word.trailing_zeros() as usize;
            let stop = start + (!(word >> start)).trailing_zeros() as usize;
            word &= !ones(0, stop);
            Some((origin + (start * grain).max(low) as i32, origin + (stop * grain).min(high) as i32))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pixels(region: &Region, of: Rect) -> Vec<(i32, i32)> {
        let mut all: Vec<(i32, i32)> = region
            .parts(of)
            .flat_map(|rect| (rect.y..rect.bottom()).flat_map(move |y| (rect.x..rect.right()).map(move |x| (x, y))))
            .collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "no pixel in two parts");
        all
    }

    #[test]
    fn parts_cover_added_pixels_once() {
        let size = Rect::new(0, 0, 70, 20);
        let added = [Rect::new(2, 2, 10, 10), Rect::new(6, 6, 60, 10), Rect::new(-5, 18, 9, 9), Rect::new(0, 0, 0, 9)];
        let mut region = Region::new(size);
        added.iter().for_each(|rect| region.add(*rect));
        region.find_bands();
        let expected = |of: Rect| -> Vec<(i32, i32)> {
            let mut pixels: Vec<(i32, i32)> = (of.y..of.bottom())
                .flat_map(|y| (of.x..of.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| size.contains(x, y) && added.iter().any(|rect| rect.contains(x, y)))
                .collect();
            pixels.sort_unstable();
            pixels
        };
        assert_eq!(pixels(&region, size), expected(size));
        assert_eq!(region.pixels(), expected(size).len());
        let window = Rect::new(5, 4, 62, 9);
        assert_eq!(pixels(&region, window), expected(window), "and of a box within the screen only what lies in it");
        let mut other = Region::new(size);
        other.add(Rect::new(68, 0, 9, 3));
        region.add_region(&other);
        region.find_bands();
        assert_eq!(region.pixels(), expected(size).len() + 2 * 3, "the pixels of another region are added");
        region.clear();
        region.find_bands();
        assert!(region.pixels() == 0 && region.parts(size).next().is_none());
        region.add_whole();
        assert_eq!(region.pixels(), 70 * 20);
        // Seventy columns are more than a word's bits: a bit stands for two of them.
        let mut region = Region::new(size);
        region.add(Rect::new(3, 5, 1, 1));
        region.add(Rect::new(69, 6, 1, 1));
        region.find_bands();
        assert_eq!(
            (pixels(&region, size), region.pixels()),
            (vec![(2, 5), (3, 5), (68, 6), (69, 6)], 4),
            "whole groups, and none past the screen's edge"
        );
        assert_eq!(pixels(&region, Rect::new(3, 0, 66, 20)), vec![(3, 5), (68, 6)], "a box cuts its parts out of them");
    }
}
