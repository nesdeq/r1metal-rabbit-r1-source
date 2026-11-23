//! Picture turns, resampling positions and the mixes of samples about them, rows filtered by taps, and places past a picture's edges mirrored back into it.

use super::simd::Sample;
use alloc::vec::Vec;

pub(crate) const PLACE_BITS: u32 = 6;
pub(crate) const PLACE_UNIT: u32 = 1 << PLACE_BITS;

// A place past either end of `length` samples mirrored back in without the end sample itself, as often as it takes: OpenCV's default border, `BORDER_REFLECT_101`.
pub(crate) const fn reflect_101(at: isize, length: usize) -> usize {
    let last = length as isize - 1;
    if last <= 0 {
        return 0;
    }
    let at = at.rem_euclid(2 * last);
    (if at > last { 2 * last - at } else { at }) as usize
}

// The same with the end sample repeated: SciPy's `reflect`.
pub(crate) const fn reflect(at: isize, length: usize) -> usize {
    if length == 0 {
        return 0;
    }
    let length = length as isize;
    let at = at.rem_euclid(2 * length);
    (if at >= length { 2 * length - 1 - at } else { at }) as usize
}

// A sample a filter weighs, a weight as wide as it, and a sum of their products, which wraps past its width as a wider sum cast to it does.
pub(crate) trait Weigh: Copy {
    type Weight: Copy;
    type Sum: Copy + Default;

    // `sum` and this sample weighed by `weight`.
    fn weigh(self, weight: Self::Weight, sum: Self::Sum) -> Self::Sum;
}

// A weight no wider than its sample has the compiler make one widening multiply-accumulate of a product and its sum, NEON's `umlal`.
impl Weigh for u8 {
    type Weight = u8;
    type Sum = u16;

    fn weigh(self, weight: u8, sum: u16) -> u16 {
        sum.wrapping_add(u16::from(weight) * u16::from(self))
    }
}

impl Weigh for u16 {
    type Weight = u16;
    type Sum = u32;

    fn weigh(self, weight: u16, sum: u32) -> u32 {
        sum.wrapping_add(u32::from(weight) * u32::from(self))
    }
}

impl Weigh for u32 {
    type Weight = u32;
    type Sum = u64;

    fn weigh(self, weight: u32, sum: u64) -> u64 {
        sum.wrapping_add(u64::from(weight) * u64::from(self))
    }
}

impl Weigh for f32 {
    type Weight = f32;
    type Sum = f32;

    fn weigh(self, weight: f32, sum: f32) -> f32 {
        sum + weight * self
    }
}

// Fills `out` with each place's sum of the samples of `rows` there, one row to each of `weights`, added in their order: a weight at a time over the whole of `out`, which the compiler makes a vector loop of.
pub(crate) fn sum_rows<T: Weigh>(rows: &[&[T]], weights: &[T::Weight], out: &mut [T::Sum]) {
    out.fill(T::Sum::default());
    let count = out.len();
    for (row, &weight) in rows.iter().zip(weights) {
        for (sum, sample) in out.iter_mut().zip(&row[..count]) {
            *sum = sample.weigh(weight, *sum);
        }
    }
}

// Fills `out` with sums of `taps` over `row` about each `step`th sample, the first tap half the taps before it, the samples past either end taken where `mirror` (`reflect_101` or `reflect`) puts them: the sums that reach past no end at once, those that do one by one.
pub(crate) fn filter_row<T: Weigh>(
    row: &[T],
    (taps, step): (&[T::Weight], usize),
    mirror: fn(isize, usize) -> usize,
    out: &mut [T::Sum],
) {
    let reach = taps.len() / 2;
    let first = reach.div_ceil(step).min(out.len());
    let room = (row.len() + reach).checked_sub(taps.len());
    let last = room.map_or(0, |room| room / step + 1).clamp(first, out.len());
    if first < last {
        let start = first * step - reach;
        // Every `step`th sample from each of the first `step` on, so that the samples a tap weighs lie side by side, where the compiler's vector loop reads them.
        let phases: Vec<Vec<T>> = match step {
            1 => Vec::new(),
            _ => (0..step).map(|phase| row.iter().skip(phase).step_by(step).copied().collect()).collect(),
        };
        let rows: Vec<&[T]> = (start..start + taps.len())
            .map(|at| if step == 1 { &row[at..] } else { &phases[at % step][at / step..] })
            .collect();
        sum_rows(&rows, taps, &mut out[first..last]);
    }
    for x in (0..first).chain(last..out.len()) {
        let place = |tap: usize| mirror((x * step + tap) as isize - reach as isize, row.len());
        let sums = taps.iter().enumerate();
        out[x] = sums.fold(T::Sum::default(), |sum, (tap, &weight)| row[place(tap)].weigh(weight, sum));
    }
}

// Fills `out` with sums of `taps` over the rows of a picture of `height` about its row `at`, the first tap half the taps above it, `row` giving each row by its place and `mirror` the place of one past either end.
pub(crate) fn filter_down<'a, T: Weigh + 'a>(
    row: impl Fn(usize) -> &'a [T],
    (taps, at, height): (&[T::Weight], usize, usize),
    mirror: fn(isize, usize) -> usize,
    out: &mut [T::Sum],
) {
    let reach = (taps.len() / 2) as isize;
    let rows: Vec<&[T]> = (0..taps.len()).map(|tap| row(mirror(at as isize + tap as isize - reach, height))).collect();
    sum_rows(&rows, taps, out);
}

pub(crate) fn middle(pixel: usize, pixels: usize, (length, per): (usize, usize), unit: i64) -> i64 {
    (2 * pixel as i64 + 1) * length as i64 * unit / (2 * pixels.max(1) as i64 * per as i64) - unit / 2
}

pub(crate) fn straddle(exact: i64, samples: usize, bits: u32) -> (usize, usize, u32) {
    let (unit, last) = (1i64 << bits, samples.saturating_sub(1));
    let clamped = exact.clamp(0, last as i64 * unit);
    let before = (clamped >> bits) as usize;
    (before, (before + 1).min(last), (clamped & (unit - 1)) as u32)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Place {
    pub(crate) low: u32,
    pub(crate) high: u32,
    pub(crate) fraction: u32,
}

impl Place {
    // The samples about the middle of pixel `pixel` of `pixels` laid over `length` samples in `per`ths, of which the first `valid` hold the picture.
    pub(crate) fn new(pixel: usize, pixels: usize, (length, per): (usize, usize), valid: usize) -> Place {
        let position = middle(pixel, pixels, (length, per), i64::from(PLACE_UNIT));
        let (low, high, fraction) = straddle(position, valid, PLACE_BITS);
        Place { low: low as u32, high: high as u32, fraction }
    }
}

pub(crate) fn blend(out: &mut [u16], first: &[u8], second: &[u8], fraction: u32) {
    let (first_weight, second_weight) = ((PLACE_UNIT - fraction) as u16, fraction as u16);
    for ((out, &first), &second) in out.iter_mut().zip(first).zip(second) {
        *out = u16::from(first) * first_weight + u16::from(second) * second_weight;
    }
}

#[inline(always)]
fn interpolate(low: u16, high: u16, fraction: u32) -> u8 {
    let (low, high) = (i32::from(low), i32::from(high));
    (((low << PLACE_BITS) + (high - low) * fraction as i32 + (1 << (2 * PLACE_BITS - 1))) >> (2 * PLACE_BITS)) as u8
}

pub(crate) fn pick<const SIZE: usize>(blends: &[u16], picks: &[Place], out: &mut [u8]) {
    for (out, &Place { low, high, fraction }) in out.as_chunks_mut::<SIZE>().0.iter_mut().zip(picks) {
        for (kind, out) in out.iter_mut().enumerate() {
            *out = interpolate(blends[low as usize + kind], blends[high as usize + kind], fraction);
        }
    }
}

/// Quarter turns clockwise, then a left-right mirror.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Turn {
    pub quarters: u8,
    pub(crate) is_mirrored: bool,
}

impl Turn {
    pub(crate) const fn is_sideways(self) -> bool {
        self.quarters % 2 == 1
    }

    const fn steps(self) -> [(isize, isize); 2] {
        match (self.quarters % 4, self.is_mirrored) {
            (0, false) => [(1, 0), (0, 1)],
            (1, false) => [(0, 1), (-1, 0)],
            (2, false) => [(-1, 0), (0, -1)],
            (3, false) => [(0, -1), (1, 0)],
            (0, true) => [(-1, 0), (0, 1)],
            (1, true) => [(0, 1), (1, 0)],
            (2, true) => [(1, 0), (0, -1)],
            (_, _) => [(0, -1), (-1, 0)],
        }
    }

    pub(crate) fn walk(self, width: usize, height: usize) -> (isize, isize, isize) {
        let [right, below] = self.steps();
        let (width, height) = (width as isize, height as isize);
        let start = corner(right.0, below.0, width) + corner(right.1, below.1, height) * width;
        (start, right.0 + right.1 * width, below.0 + below.1 * width)
    }

    pub(crate) fn walk_back(self, width: usize, height: usize) -> ((isize, isize), (isize, isize), (isize, isize)) {
        let [right, below] = self.steps();
        let (along, down) = ((right.0, below.0), (right.1, below.1));
        ((corner(along.0, down.0, width as isize), corner(along.1, down.1, height as isize)), along, down)
    }
}

const fn corner(one: isize, other: isize, length: isize) -> isize {
    if one < 0 || other < 0 { length - 1 } else { 0 }
}

// Writes the words of a picture `width` across as they stand once turned, in place of what `out` held; returns how many stand in a line.
pub(crate) fn turn_words(words: &[u32], width: usize, turn: Turn, out: &mut Vec<u32>) -> usize {
    let height = words.len().checked_div(width).unwrap_or(0);
    let words = &words[..width * height];
    if turn.quarters.is_multiple_of(4) && !turn.is_mirrored {
        out.clear();
        out.extend_from_slice(words);
        return width;
    }
    // The turn writes every word, so only what `out` lacked is filled first.
    out.resize(words.len(), 0);
    Sample::turn(words, width, turn, out);
    if turn.is_sideways() { height } else { width }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn mirrored_borders() {
        assert_eq!(
            [-3, -2, -1, 0, 3, 4, 5, 6, 7].map(|at| reflect_101(at, 4)),
            [3, 2, 1, 0, 3, 2, 1, 0, 1],
            "mirrored about the end samples, round and round"
        );
        assert_eq!(
            [-5, -4, -2, -1, 0, 3, 4, 5, 7, 8].map(|at| reflect(at, 4)),
            [3, 3, 1, 0, 0, 3, 3, 2, 0, 0],
            "mirrored past the end samples, each of which stands twice"
        );
        assert_eq!(
            [reflect_101(-9, 1), reflect_101(3, 0), reflect(-9, 1), reflect(3, 0)],
            [0; 4],
            "one sample or none"
        );
    }

    // Sums of `taps` over `row` about each `step`th sample, every tap's place mirrored on its own.
    fn mirrored<T: Weigh>(
        row: &[T],
        (taps, step): (&[T::Weight], usize),
        mirror: fn(isize, usize) -> usize,
        count: usize,
    ) -> Vec<T::Sum> {
        let reach = (taps.len() / 2) as isize;
        let sum = |x: usize| {
            taps.iter().enumerate().fold(T::Sum::default(), |sum, (tap, &weight)| {
                row[mirror((x * step + tap) as isize - reach, row.len())].weigh(weight, sum)
            })
        };
        (0..count).map(sum).collect()
    }

    fn check_rows<T: Weigh>(row: &[T], taps: &[T::Weight])
    where
        T::Sum: PartialEq + core::fmt::Debug,
    {
        let mirrors: [fn(isize, usize) -> usize; 2] = [reflect_101, reflect];
        for (step, mirror) in [1, 2, 4].into_iter().flat_map(|step| mirrors.map(|mirror| (step, mirror))) {
            for count in [row.len().div_ceil(step), row.len() / step] {
                let mut out = vec![T::Sum::default(); count];
                filter_row(row, (taps, step), mirror, &mut out);
                let (length, count) = (row.len(), taps.len());
                assert_eq!(
                    out,
                    mirrored(row, (taps, step), mirror, out.len()),
                    "{count} taps at step {step} of {length}"
                );
            }
        }
    }

    #[test]
    fn filtered_rows_mirrored() {
        let mut state = 1u32;
        let mut next = || {
            state = crate::base::math::xorshift32(state);
            state
        };
        for length in 1..48 {
            for count in 1..=25 {
                let samples: Vec<u32> = (0..length).map(|_| next()).collect();
                let weights: Vec<u32> = (0..count).map(|_| next()).collect();
                let bytes: Vec<u8> = samples.iter().map(|&sample| sample as u8).collect();
                check_rows(&bytes, &weights.iter().map(|&weight| weight as u8).collect::<Vec<u8>>());
                let halves: Vec<u16> = samples.iter().map(|&sample| sample as u16).collect();
                check_rows(&halves, &weights.iter().map(|&weight| weight as u16).collect::<Vec<u16>>());
                check_rows(&samples, &weights);
                let unit = |value: u32| (value >> u8::BITS) as f32 / (1u32 << (u32::BITS - u8::BITS)) as f32;
                let floats: Vec<f32> = samples.iter().map(|&sample| unit(sample)).collect();
                check_rows(&floats, &weights.iter().map(|&weight| unit(weight)).collect::<Vec<f32>>());
            }
        }
    }

    #[test]
    fn places_blends_and_picks() {
        let places: Vec<Place> = (0..3).map(|pixel| Place::new(pixel, 3, (4, 1), 4)).collect();
        assert_eq!(
            places,
            [
                Place { low: 0, high: 1, fraction: 10 },
                Place { low: 1, high: 2, fraction: 32 },
                Place { low: 2, high: 3, fraction: 53 }
            ],
            "three pixels along four samples"
        );
        assert_eq!(
            (Place::new(0, 8, (2, 1), 2), Place::new(7, 8, (2, 1), 2)),
            (Place { low: 0, high: 1, fraction: 0 }, Place { low: 1, high: 1, fraction: 0 }),
            "past the outermost samples there is nothing to go toward"
        );
        assert!((0..5).all(|pixel| Place::new(pixel, 5, (5, 1), 5)
            == Place { low: pixel as u32, high: (pixel as u32 + 1).min(4), fraction: 0 }));
        let fifths: Vec<Place> = (0..4).map(|pixel| Place::new(pixel, 4, (10, 4), 2)).collect();
        assert_eq!(
            fifths,
            [
                Place { low: 0, high: 1, fraction: 0 },
                Place { low: 0, high: 1, fraction: 28 },
                Place { low: 1, high: 1, fraction: 0 },
                Place { low: 1, high: 1, fraction: 0 }
            ],
            "four pixels along two and a half samples of which the first two hold the picture"
        );
        let mut blends = [0u16; 2];
        blend(&mut blends, &[0, 64], &[128, 192], 32);
        assert_eq!(blends, [64 * 64, 128 * 64], "half way between two lines");
        let picks = [
            Place { low: 0, high: 1, fraction: 16 },
            Place { low: 1, high: 2, fraction: 32 },
            Place { low: 2, high: 2, fraction: 0 },
        ];
        let mut out = [0u8; 3];
        pick::<1>(&[0, 4096, 8192], &picks, &mut out);
        assert_eq!(out, [16, 96, 128], "a quarter of the way, half way, and on the last sample");
        let pairs = [
            Place { low: 2, high: 0, fraction: 16 },
            Place { low: 4, high: 2, fraction: 32 },
            Place { low: 4, high: 4, fraction: 0 },
        ];
        let mut out = [0u8; 6];
        pick::<2>(&[0, 6400, 4096, 3200, 8192, 0], &pairs, &mut out);
        assert_eq!(out, [48, 63, 96, 25, 128, 0], "each kind of a pair between its own samples");
    }

    #[test]
    fn turned_words() {
        let picture: [u32; 6] = [1, 2, 3, 4, 5, 6];
        let mut out = vec![9u32; 2];
        let mut turned = |quarters: u8, is_mirrored: bool, width: usize| {
            let across = turn_words(&picture, width, Turn { quarters, is_mirrored }, &mut out);
            (across, out.clone())
        };
        assert_eq!(turned(0, false, 3), (3, picture.to_vec()));
        assert_eq!(turned(1, false, 3), (2, vec![4, 1, 5, 2, 6, 3]), "the top left corner goes to the top right");
        assert_eq!(turned(2, false, 3), (3, vec![6, 5, 4, 3, 2, 1]));
        assert_eq!(turned(3, false, 3), (2, vec![3, 6, 2, 5, 1, 4]), "the top left corner goes to the bottom left");
        assert_eq!(turned(5, false, 3), (2, vec![4, 1, 5, 2, 6, 3]), "turns go round");
        assert_eq!(turned(0, true, 3), (3, vec![3, 2, 1, 6, 5, 4]), "mirrored left to right");
        assert_eq!(turned(1, true, 3), (2, vec![1, 4, 2, 5, 3, 6]), "turned, then mirrored: about its diagonal");
        assert_eq!(turned(1, false, 0), (0, vec![]), "a picture of no width has no words");
        assert_eq!(turned(0, false, 4), (4, vec![1, 2, 3, 4]), "words short of a line are left out");
    }

    #[test]
    fn walk_covers_and_returns() {
        let (across, down) = (5usize, 3usize);
        for quarters in 0..4 {
            for is_mirrored in [false, true] {
                let turn = Turn { quarters, is_mirrored };
                let (width, height) = if turn.is_sideways() { (down, across) } else { (across, down) };
                let ((start, right, below), (origin, along, back_down)) =
                    (turn.walk(width, height), turn.walk_back(across, down));
                let mut landed = vec![false; width * height];
                for y in 0..down as isize {
                    for x in 0..across as isize {
                        let at = start + x * right + y * below;
                        let (u, v) = (at % width as isize, at / width as isize);
                        let back = (origin.0 + u * along.0 + v * back_down.0, origin.1 + u * along.1 + v * back_down.1);
                        assert_eq!(back, (x, y), "{turn:?}");
                        landed[at as usize] = true;
                    }
                }
                assert!(landed.iter().all(|&landed| landed), "{turn:?} fills the picture");
            }
        }
        assert_eq!(
            Turn { quarters: 1, is_mirrored: false }.walk(3, 5),
            (2, 3, -1),
            "the top left goes to the top right"
        );
        assert_eq!(
            (middle(0, 3, (4, 1), 64), middle(2, 3, (4, 1), 64), middle(0, 4, (2, 1), 64)),
            (10, 181, -16),
            "a pixel's middle among the samples, before the first of them where it lies before its middle"
        );
        assert_eq!(
            (straddle(80, 4, 6), straddle(-5, 4, 6), straddle(1000, 4, 6), straddle(9, 0, 2)),
            ((1, 2, 16), (0, 1, 0), (3, 3, 0), (0, 0, 0)),
            "a quarter of the way from the second sample to the third, and a place past either end held to it"
        );
    }
}
