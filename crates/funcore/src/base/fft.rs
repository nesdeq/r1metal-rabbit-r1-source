// Discrete Fourier transforms: of 16 by 16 tiles, each step over whole rows of 16 at once, of a line of points of a length whose factors are 2, 3 and 5, each stage's butterflies over whole rows of points at once, and of real samples through a line of half as many.

use super::math::sine_cosine;
use alloc::vec;
use alloc::vec::Vec;

// Samples along a side of a tile.
pub(crate) const SIDE: usize = 16;
// The radices a line is split by, in the order they are tried.
const RADICES: [usize; 4] = [4, 2, 3, 5];
const MAX_RADIX: usize = 5;

// A complex number: its real and its imaginary part.
pub(crate) type Complex = [f32; 2];

// The point turned by a turn, or by its conjugate where `sign` is -1.
pub(crate) fn rotate(point: Complex, turn: Complex, sign: f32) -> Complex {
    let im = sign * turn[1];
    [point[0] * turn[0] - point[1] * im, point[0] * im + point[1] * turn[0]]
}

// The radices of a line's stages, the first splitting the whole line.
fn radices(len: usize) -> Vec<usize> {
    let mut radices = Vec::new();
    let mut left = len;
    while left > 1 {
        let radix = RADICES.into_iter().find(|radix| left.is_multiple_of(*radix));
        let radix = radix.expect("a length whose factors are 2, 3 and 5");
        radices.push(radix);
        left /= radix;
    }
    radices
}

// e^(-2 pi i k / len) for every k below the length.
fn turns(len: usize) -> Vec<Complex> {
    (0..len)
        .map(|k| {
            let (sine, cosine) = sine_cosine(-(k as f64) / len as f64);
            [cosine as f32, sine as f32]
        })
        .collect()
}

// A stage of a line's transform: butterflies of `radix` parts `span` points apart, the roots of its radix, and the turns of the parts, part q's of every butterfly in a row, row q - 1.
struct Stage {
    radix: usize,
    span: usize,
    roots: [Complex; MAX_RADIX],
    turns: Vec<Complex>,
}

// The transform of a line of points of one length, unscaled both ways, its points taken apart by decimation in time.
pub(crate) struct Plan {
    // The stages in the order they run, from butterflies of single points on.
    stages: Vec<Stage>,
    // Where each input point stands before the first butterflies.
    places: Vec<usize>,
}

impl Plan {
    pub(crate) fn new(len: usize) -> Plan {
        let (radices, turns) = (radices(len), turns(len));
        let mut places = vec![0; len];
        place(&mut places, &radices, 0, 0, 1);
        let (mut groups, mut span) = (len, 1);
        let mut stages = Vec::new();
        for &radix in radices.iter().rev() {
            groups /= radix;
            let roots = core::array::from_fn(|q| turns[q % radix * (len / radix)]);
            let parts = (1..radix).flat_map(|q| (0..span).map(move |j| q * j * groups));
            stages.push(Stage { radix, span, roots, turns: parts.map(|at| turns[at]).collect() });
            span *= radix;
        }
        Plan { stages, places }
    }

    // X[k] = sum of x[n] e^(-2 pi i k n / len).
    pub(crate) fn forward(&self, from: &[Complex], to: &mut [Complex]) {
        self.transform(from, to, 1.0);
    }

    // x[n] = sum of X[k] e^(2 pi i k n / len), with no division by the length.
    pub(crate) fn inverse(&self, from: &[Complex], to: &mut [Complex]) {
        self.transform(from, to, -1.0);
    }

    fn transform(&self, from: &[Complex], to: &mut [Complex], sign: f32) {
        for (&point, &place) in from.iter().zip(&self.places) {
            to[place] = point;
        }
        for stage in &self.stages {
            for group in to.chunks_exact_mut(stage.radix * stage.span) {
                match stage.radix {
                    2 => halves(group, stage, sign),
                    4 => quarters(group, stage, sign),
                    3 => odd::<3>(group, stage, sign),
                    _ => odd::<MAX_RADIX>(group, stage, sign),
                }
            }
        }
    }
}

// The butterflies of radix 2 over a group's two rows; the roots of two are 1 and -1, sums with no product.
fn halves(group: &mut [Complex], stage: &Stage, sign: f32) {
    let (low, high) = group.split_at_mut(stage.span);
    for ((low, high), &turn) in low.iter_mut().zip(high.iter_mut()).zip(&stage.turns) {
        let (a, b) = (*low, rotate(*high, turn, sign));
        *low = [a[0] + b[0], a[1] + b[1]];
        *high = [a[0] - b[0], a[1] - b[1]];
    }
}

// The butterflies of radix 4 over a group's four rows; the roots of four are 1, -1 and a quarter turn, sums and swaps with no product.
fn quarters(group: &mut [Complex], stage: &Stage, sign: f32) {
    let span = stage.span;
    let (low, high) = group.split_at_mut(2 * span);
    let ((first, second), (third, fourth)) = (low.split_at_mut(span), high.split_at_mut(span));
    let (second_turns, later_turns) = stage.turns.split_at(span);
    let (third_turns, fourth_turns) = later_turns.split_at(span);
    let points = first.iter_mut().zip(second.iter_mut()).zip(third.iter_mut()).zip(fourth.iter_mut());
    let turns = second_turns.iter().zip(third_turns).zip(fourth_turns);
    for ((((first, second), third), fourth), ((&b_turn, &c_turn), &d_turn)) in points.zip(turns) {
        let (a, b, c, d) =
            (*first, rotate(*second, b_turn, sign), rotate(*third, c_turn, sign), rotate(*fourth, d_turn, sign));
        let (sum, difference) = ([a[0] + c[0], a[1] + c[1]], [a[0] - c[0], a[1] - c[1]]);
        let (across, apart) = ([b[0] + d[0], b[1] + d[1]], [b[0] - d[0], b[1] - d[1]]);
        let turned = [sign * apart[1], -sign * apart[0]];
        *first = [sum[0] + across[0], sum[1] + across[1]];
        *second = [difference[0] + turned[0], difference[1] + turned[1]];
        *third = [sum[0] - across[0], sum[1] - across[1]];
        *fourth = [difference[0] - turned[0], difference[1] - turned[1]];
    }
}

// The butterflies of an odd radix over a group's rows: each part turned by the roots its output takes.
fn odd<const RADIX: usize>(group: &mut [Complex], stage: &Stage, sign: f32) {
    let span = stage.span;
    for at in 0..span {
        let parts: [Complex; RADIX] = core::array::from_fn(|q| match q {
            0 => group[at],
            _ => rotate(group[q * span + at], stage.turns[(q - 1) * span + at], sign),
        });
        for r in 0..RADIX {
            let mut sum = parts[0];
            for (q, part) in parts.iter().enumerate().skip(1) {
                let turned = rotate(*part, stage.roots[r * q % RADIX], sign);
                sum = [sum[0] + turned[0], sum[1] + turned[1]];
            }
            group[r * span + at] = sum;
        }
    }
}

// Where each input point of a line stands once its points are taken apart by `radices`, the first place given to `input` among those at `stride` from it.
fn place(places: &mut [usize], radices: &[usize], first: usize, input: usize, stride: usize) {
    let Some((&radix, rest)) = radices.split_first() else {
        places[input] = first;
        return;
    };
    let span = rest.iter().product::<usize>();
    for j in 0..radix {
        place(places, rest, first + j * span, input + j * stride, stride * radix);
    }
}

// The transform of an even count of real samples, bins nought to half the count: its even and odd samples as one point each of a line of half as many, whose transform the turns of each bin over the count part and join.
pub(crate) struct Real {
    plan: Plan,
    // e^(2 pi i k / count) for every bin.
    turns: Vec<Complex>,
    points: Vec<Complex>,
    transformed: Vec<Complex>,
}

impl Real {
    pub(crate) fn new(count: usize) -> Real {
        let points = count / 2;
        let turns = (0..=points)
            .map(|bin| {
                let (sine, cosine) = sine_cosine(bin as f64 / count as f64);
                [cosine as f32, sine as f32]
            })
            .collect();
        Real { plan: Plan::new(points), turns, points: vec![[0.0; 2]; points], transformed: vec![[0.0; 2]; points] }
    }

    pub(crate) fn forward(&mut self, samples: &[f32], bins: &mut [Complex]) {
        let points = self.points.len();
        for (point, pair) in self.points.iter_mut().zip(samples.as_chunks::<2>().0) {
            *point = *pair;
        }
        self.plan.forward(&self.points, &mut self.transformed);
        for (bin, out) in bins.iter_mut().enumerate().take(points + 1) {
            let (value, mirror) = (self.transformed[bin % points], self.transformed[(points - bin) % points]);
            let even = [(value[0] + mirror[0]) / 2.0, (value[1] - mirror[1]) / 2.0];
            let odd = [(value[1] + mirror[1]) / 2.0, (mirror[0] - value[0]) / 2.0];
            let turned = rotate(odd, self.turns[bin], -1.0);
            *out = [even[0] + turned[0], even[1] + turned[1]];
        }
    }

    // The samples whose transform `bins` is.
    pub(crate) fn inverse(&mut self, bins: &[Complex], samples: &mut [f32]) {
        let points = self.points.len();
        for (bin, point) in self.points.iter_mut().enumerate() {
            let (value, mirror) = (bins[bin], bins[points - bin]);
            let even = [(value[0] + mirror[0]) / 2.0, (value[1] - mirror[1]) / 2.0];
            let odd = rotate([(value[0] - mirror[0]) / 2.0, (value[1] + mirror[1]) / 2.0], self.turns[bin], 1.0);
            *point = [even[0] - odd[1], even[1] + odd[0]];
        }
        self.plan.inverse(&self.points, &mut self.transformed);
        let scale = 1.0 / points as f32;
        for (pair, value) in samples.as_chunks_mut::<2>().0.iter_mut().zip(&self.transformed) {
            *pair = [value[0] * scale, value[1] * scale];
        }
    }
}

const REVERSED: [usize; SIDE] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

pub(crate) type Rows = [[f32; SIDE]; SIDE];

// A tile of complex numbers: its real and its imaginary parts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Tile {
    pub(crate) re: Rows,
    pub(crate) im: Rows,
}

impl Tile {
    const ZERO: Tile = Tile { re: [[0.0; SIDE]; SIDE], im: [[0.0; SIDE]; SIDE] };

    // Two real tiles as one complex one, whose transform `split` takes apart.
    pub(crate) const fn pair(re: Rows, im: Rows) -> Tile {
        Tile { re, im }
    }

    // The transforms of the two real tiles a transformed pair was made of, by the symmetry of a real tile's transform.
    pub(crate) fn split(&self) -> (Tile, Tile) {
        let (mut first, mut second) = (Tile::ZERO, Tile::ZERO);
        for u in 0..SIDE {
            let mirrored_u = (SIDE - u) % SIDE;
            for v in 0..SIDE {
                let mirrored_v = (SIDE - v) % SIDE;
                let (re, im) = (self.re[u][v], self.im[u][v]);
                let (mirrored_re, mirrored_im) = (self.re[mirrored_u][mirrored_v], self.im[mirrored_u][mirrored_v]);
                first.re[u][v] = (re + mirrored_re) / 2.0;
                first.im[u][v] = (im - mirrored_im) / 2.0;
                second.re[u][v] = (im + mirrored_im) / 2.0;
                second.im[u][v] = (mirrored_re - re) / 2.0;
            }
        }
        (first, second)
    }

    // The pair of two transforms of real tiles, whose inverse holds the first in its real part and the second in its imaginary part.
    pub(crate) fn join(first: &Tile, second: &Tile) -> Tile {
        let mut joined = Tile::ZERO;
        for u in 0..SIDE {
            for v in 0..SIDE {
                joined.re[u][v] = first.re[u][v] - second.im[u][v];
                joined.im[u][v] = first.im[u][v] + second.re[u][v];
            }
        }
        joined
    }

    fn transpose(&mut self) {
        for rows in [&mut self.re, &mut self.im] {
            let copy = *rows;
            for (u, row) in rows.iter_mut().enumerate() {
                for (v, value) in row.iter_mut().enumerate() {
                    *value = copy[v][u];
                }
            }
        }
    }
}

// The transform's twiddle factors.
pub(crate) struct Fft {
    // The cosine and sine of each sixteenth of a turn up to half of one.
    turns: [Complex; SIDE / 2],
}

impl Default for Fft {
    fn default() -> Fft {
        Fft {
            turns: core::array::from_fn(|step| {
                let (sine, cosine) = sine_cosine(step as f64 / SIDE as f64);
                [cosine as f32, sine as f32]
            }),
        }
    }
}

impl Fft {
    // The transform along the tile's first index, for every place along its second at once.
    fn down(&self, tile: &mut Tile, sign: f32) {
        for (place, &reversed) in REVERSED.iter().enumerate() {
            if reversed > place {
                tile.re.swap(place, reversed);
                tile.im.swap(place, reversed);
            }
        }
        let mut half = 1;
        while half < SIDE {
            let stride = SIDE / (2 * half);
            for start in (0..SIDE).step_by(2 * half) {
                for offset in 0..half {
                    let turn = self.turns[offset * stride];
                    let (low, high) = (start + offset, start + offset + half);
                    let (re_low, re_high) = tile.re.split_at_mut(high);
                    let (im_low, im_high) = tile.im.split_at_mut(high);
                    let (re_a, im_a, re_b, im_b) =
                        (&mut re_low[low], &mut im_low[low], &mut re_high[0], &mut im_high[0]);
                    for lane in 0..SIDE {
                        let [turned_re, turned_im] = rotate([re_b[lane], im_b[lane]], turn, -sign);
                        re_b[lane] = re_a[lane] - turned_re;
                        im_b[lane] = im_a[lane] - turned_im;
                        re_a[lane] += turned_re;
                        im_a[lane] += turned_im;
                    }
                }
            }
            half *= 2;
        }
    }

    // Transforms a tile in place; the transform comes out with its two indices swapped.
    pub(crate) fn forward(&self, tile: &mut Tile) {
        self.down(tile, 1.0);
        tile.transpose();
        self.down(tile, 1.0);
    }

    // Undoes `forward`, its swap of the indices too.
    pub(crate) fn inverse(&self, tile: &mut Tile) {
        self.down(tile, -1.0);
        tile.transpose();
        self.down(tile, -1.0);
        let scale = 1.0 / (SIDE * SIDE) as f32;
        for rows in [&mut tile.re, &mut tile.im] {
            rows.iter_mut().flatten().for_each(|value| *value *= scale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seed: usize) -> Rows {
        core::array::from_fn(|u| core::array::from_fn(|v| ((u * 37 + v * 11 + seed * 7) % 23) as f32 - 9.0))
    }

    fn definition(re: &Rows, im: &Rows) -> Tile {
        let mut out = Tile::ZERO;
        for a in 0..SIDE {
            for b in 0..SIDE {
                let (mut sum_re, mut sum_im) = (0.0f64, 0.0f64);
                for u in 0..SIDE {
                    for v in 0..SIDE {
                        let (sine, cosine) = sine_cosine(-(((a * u + b * v) % SIDE) as f64) / SIDE as f64);
                        let (x, y) = (f64::from(re[u][v]), f64::from(im[u][v]));
                        sum_re += x * cosine - y * sine;
                        sum_im += x * sine + y * cosine;
                    }
                }
                out.re[a][b] = sum_re as f32;
                out.im[a][b] = sum_im as f32;
            }
        }
        out
    }

    fn near(first: &Tile, second: &Tile, by: f32) -> bool {
        let rows = |tile: &Tile| tile.re.into_iter().chain(tile.im).flatten().collect::<alloc::vec::Vec<_>>();
        rows(first).iter().zip(rows(second)).all(|(a, b)| (a - b).abs() <= by)
    }

    #[test]
    fn transform_matches_definition() {
        let fft = Fft::default();
        let (re, im) = (sample(1), sample(2));
        let mut tile = Tile { re, im };
        fft.forward(&mut tile);
        let mut expected = definition(&re, &im);
        expected.transpose();
        assert!(near(&tile, &expected, 1.0e-3), "the sum the transform is defined by, its indices swapped");
        fft.inverse(&mut tile);
        assert!(near(&tile, &Tile { re, im }, 1.0e-4), "and back");
    }

    #[test]
    fn real_pairs() {
        let fft = Fft::default();
        let (first, second) = (sample(3), sample(4));
        let mut pair = Tile::pair(first, second);
        fft.forward(&mut pair);
        let (mut alone_first, mut alone_second) =
            (Tile::pair(first, [[0.0; SIDE]; SIDE]), Tile::pair(second, [[0.0; SIDE]; SIDE]));
        fft.forward(&mut alone_first);
        fft.forward(&mut alone_second);
        let (split_first, split_second) = pair.split();
        assert!(
            near(&split_first, &alone_first, 1.0e-3) && near(&split_second, &alone_second, 1.0e-3),
            "two real tiles transformed as one come apart into their own transforms"
        );
        let mut joined = Tile::join(&split_first, &split_second);
        fft.inverse(&mut joined);
        assert!(near(&joined, &Tile::pair(first, second), 1.0e-4), "and joined they come back as the pair");
    }

    // The sum a line's transform is defined by, in f64.
    fn line_definition(points: &[Complex], sign: f64) -> alloc::vec::Vec<[f64; 2]> {
        let len = points.len();
        (0..len)
            .map(|k| {
                points.iter().enumerate().fold([0.0, 0.0], |[re, im], (n, point)| {
                    let (sine, cosine) = sine_cosine(sign * ((k * n) % len) as f64 / len as f64);
                    let (x, y) = (f64::from(point[0]), f64::from(point[1]));
                    [re + x * cosine - y * sine, im + x * sine + y * cosine]
                })
            })
            .collect()
    }

    #[test]
    fn line_matches_definition() {
        for len in [1, 2, 3, 4, 5, 8, 15, 60, 64, 120, 240, 480, 512] {
            let plan = Plan::new(len);
            let points: alloc::vec::Vec<Complex> =
                (0..len).map(|n| [((n * 37 + 11) % 23) as f32 - 11.0, ((n * 17 + 5) % 19) as f32 - 9.0]).collect();
            let mut transformed = vec![[0.0; 2]; len];
            plan.forward(&points, &mut transformed);
            for (k, (actual, expected)) in transformed.iter().zip(line_definition(&points, -1.0)).enumerate() {
                let error = (f64::from(actual[0]) - expected[0]).abs().max((f64::from(actual[1]) - expected[1]).abs());
                assert!(error < 1.0e-3, "length {len} point {k}: {actual:?} for {expected:?}");
            }
            let mut back = vec![[0.0; 2]; len];
            plan.inverse(&transformed, &mut back);
            for (n, (actual, point)) in back.iter().zip(&points).enumerate() {
                let error = (actual[0] / len as f32 - point[0]).abs().max((actual[1] / len as f32 - point[1]).abs());
                assert!(
                    error < 1.0e-4,
                    "length {len} point {n}: the inverse, unscaled, gives the line times its length"
                );
            }
        }
        assert_eq!(radices(480), [4, 4, 2, 3, 5], "fours first, then two, then the odd factors");
        let stages: alloc::vec::Vec<(usize, usize)> =
            Plan::new(480).stages.iter().map(|stage| (stage.radix, stage.span)).collect();
        assert_eq!(stages, [(5, 1), (3, 5), (2, 15), (4, 30), (4, 120)], "the stages run from the last split");
    }

    // The transform of a line butterfly by butterfly in place, each part's turn taken from the turns of the line as it goes.
    fn in_place(points: &[Complex], sign: f32) -> alloc::vec::Vec<Complex> {
        let (len, radices, turns) = (points.len(), radices(points.len()), turns(points.len()));
        let mut places = vec![0; len];
        place(&mut places, &radices, 0, 0, 1);
        let mut line = vec![[0.0f32; 2]; len];
        for (&point, &place) in points.iter().zip(&places) {
            line[place] = point;
        }
        let (mut groups, mut span) = (len, 1);
        for &radix in radices.iter().rev() {
            groups /= radix;
            let roots: [Complex; MAX_RADIX] = core::array::from_fn(|q| turns[q % radix * (len / radix)]);
            for group in line.chunks_exact_mut(radix * span) {
                for j in 0..span {
                    let mut parts = [group[j]; MAX_RADIX];
                    for (q, part) in parts.iter_mut().enumerate().take(radix).skip(1) {
                        *part = rotate(group[j + q * span], turns[q * j * groups], sign);
                    }
                    let add = |one: Complex, other: Complex| [one[0] + other[0], one[1] + other[1]];
                    let less = |one: Complex, other: Complex| [one[0] - other[0], one[1] - other[1]];
                    match radix {
                        2 => (group[j], group[j + span]) = (add(parts[0], parts[1]), less(parts[0], parts[1])),
                        4 => {
                            let [a, b, c, d, _] = parts;
                            let (sum, difference, across, apart) = (add(a, c), less(a, c), add(b, d), less(b, d));
                            let turned = [sign * apart[1], -sign * apart[0]];
                            (group[j], group[j + span]) = (add(sum, across), add(difference, turned));
                            (group[j + 2 * span], group[j + 3 * span]) = (less(sum, across), less(difference, turned));
                        }
                        _ => {
                            for r in 0..radix {
                                let turned = (1..radix).map(|q| rotate(parts[q], roots[r * q % radix], sign));
                                group[j + r * span] = turned.fold(parts[0], add);
                            }
                        }
                    }
                }
            }
            span *= radix;
        }
        line
    }

    #[test]
    fn stages_match_in_place() {
        let mut cases = 0;
        for len in (1..=1024usize).filter(|&len| [2, 3, 5].iter().fold(len, |left, &radix| strip(left, radix)) == 1) {
            let plan = Plan::new(len);
            for seed in 0..3 {
                let mut state = (len * 7 + seed) as u32 | 1;
                let mut points: alloc::vec::Vec<Complex> = (0..len)
                    .map(|_| {
                        state = crate::base::math::xorshift32(state);
                        [(state >> 16) as f32 - 32_768.0, (state & 0xFFFF) as f32 / 65_536.0 - 0.5]
                    })
                    .collect();
                if seed == 2 {
                    points.iter_mut().step_by(3).for_each(|point| *point = [-0.0, 0.0]);
                }
                for sign in [1.0, -1.0] {
                    let mut staged = vec![[0.0f32; 2]; len];
                    plan.transform(&points, &mut staged, sign);
                    let bits = |line: &[Complex]| {
                        line.iter().flatten().map(|value| value.to_bits()).collect::<alloc::vec::Vec<u32>>()
                    };
                    assert_eq!(bits(&staged), bits(&in_place(&points, sign)), "length {len}, seed {seed}, sign {sign}");
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 87 * 6, "every length to 1024 whose factors are 2, 3 and 5, both ways, bit for bit");
    }

    // A count with every factor `radix` taken out.
    fn strip(mut count: usize, radix: usize) -> usize {
        while count.is_multiple_of(radix) {
            count /= radix;
        }
        count
    }

    #[test]
    fn real_matches_definition() {
        for count in [2usize, 4, 64, 480, 512] {
            let samples: alloc::vec::Vec<f32> = (0..count).map(|at| ((at * 37 + 11) % 23) as f32 - 9.0).collect();
            let points: alloc::vec::Vec<Complex> = samples.iter().map(|&sample| [sample, 0.0]).collect();
            let (mut real, mut bins) = (Real::new(count), vec![[0.0f32; 2]; count / 2 + 1]);
            real.forward(&samples, &mut bins);
            for (bin, (actual, expected)) in bins.iter().zip(line_definition(&points, -1.0)).enumerate() {
                let error = (f64::from(actual[0]) - expected[0]).abs().max((f64::from(actual[1]) - expected[1]).abs());
                assert!(error < 1.0e-2, "{count} samples, bin {bin}: {actual:?} for the line's {expected:?}");
            }
            let mut back = vec![0.0f32; count];
            real.inverse(&bins, &mut back);
            assert!(
                back.iter().zip(&samples).all(|(&actual, &expected)| (actual - expected).abs() < 1.0e-4),
                "{count} samples: and back, scaled"
            );
        }
    }
}
