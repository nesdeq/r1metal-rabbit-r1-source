use super::FRAME;
use crate::base::fft::{Complex, Plan, rotate};
use crate::base::math::{bessel, sine_cosine, sqrt};
use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

// The short windows of a block, and the lines of each (4.6.11).
pub(super) const WINDOWS: usize = 8;
pub(super) const SHORT: usize = FRAME / WINDOWS;
// Where the first short window of a block lies among its 2 * FRAME samples.
const FIRST_SHORT: usize = (FRAME - SHORT) / 2;
// The shapes of a window, as the bit of ics_info has them, and the alpha of the Kaiser-Bessel window of a long window and of a short one (4.6.11.3).
pub(super) const SINE: usize = 0;
const KAISER_BESSEL: usize = 1;
const LONG_ALPHA: f64 = 4.0;
const SHORT_ALPHA: f64 = 6.0;
const SEQUENCE_MASK: u32 = 3;

// The rising half of both shapes of a long window and of a short one, and the turns of their cosine transforms.
static LONG_WINDOWS: [[f32; FRAME]; 2] = windows(LONG_ALPHA);
static SHORT_WINDOWS: [[f32; SHORT]; 2] = windows(SHORT_ALPHA);
static LONG_TWIDDLES: Twiddles<{ FRAME / 2 }> = Twiddles::new();
static SHORT_TWIDDLES: Twiddles<{ SHORT / 2 }> = Twiddles::new();

// The window_sequence of ics_info (4.6.11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Sequence {
    Long,
    Start,
    Short,
    Stop,
}

impl Sequence {
    pub(super) const fn of(bits: u32) -> Sequence {
        match bits & SEQUENCE_MASK {
            0 => Sequence::Long,
            1 => Sequence::Start,
            2 => Sequence::Short,
            _ => Sequence::Stop,
        }
    }
}

const fn from_turns(turns: f64) -> Complex {
    let (sine, cosine) = sine_cosine(turns);
    [cosine as f32, sine as f32]
}

// The turns of a cosine transform over 2 * `POINTS` values: of each pair of values before its Fourier transform, and of each point after.
struct Twiddles<const POINTS: usize> {
    pre: [Complex; POINTS],
    post: [Complex; POINTS],
}

impl<const POINTS: usize> Twiddles<POINTS> {
    const fn new() -> Twiddles<POINTS> {
        let frame = (2 * POINTS) as f64;
        let mut twiddles = Twiddles { pre: [[0.0; 2]; POINTS], post: [[0.0; 2]; POINTS] };
        let mut point = 0;
        while point < POINTS {
            twiddles.pre[point] = from_turns(point as f64 / (2.0 * frame));
            twiddles.post[point] = from_turns((4 * point + 1) as f64 / (8.0 * frame));
            point += 1;
        }
        twiddles
    }
}

// The cosine transform of the fourth kind over `size` values, one Fourier transform of half as many points.
struct Dct {
    size: usize,
    pre_twiddles: &'static [Complex],
    post_twiddles: &'static [Complex],
    plan: Plan,
    folded: Vec<Complex>,
    points: Vec<Complex>,
}

impl Dct {
    fn new<const POINTS: usize>(twiddles: &'static Twiddles<POINTS>) -> Dct {
        Dct {
            size: 2 * POINTS,
            pre_twiddles: &twiddles.pre,
            post_twiddles: &twiddles.post,
            plan: Plan::new(POINTS),
            folded: vec![[0.0; 2]; POINTS],
            points: vec![[0.0; 2]; POINTS],
        }
    }

    fn transform(&mut self, from: &[f32], to: &mut [f32]) {
        let size = self.size;
        for (point, &by) in self.pre_twiddles.iter().enumerate() {
            self.folded[point] = rotate([from[2 * point], from[size - 1 - 2 * point]], by, -1.0);
        }
        self.plan.forward(&self.folded, &mut self.points);
        for (point, &by) in self.post_twiddles.iter().enumerate() {
            let product = rotate(self.points[point], by, -1.0);
            to[2 * point] = product[0];
            to[size - 1 - 2 * point] = -product[1];
        }
    }
}

// The rising half of the sine and the Kaiser-Bessel window of a window of 2 * `HALF` samples (4.6.11.3); the kernel's last weight goes into its total alone.
const fn windows<const HALF: usize>(alpha: f64) -> [[f32; HALF]; 2] {
    let (mut shapes, mut kernel) = ([[0.0; HALF]; 2], [0.0; HALF]);
    let (mut total, mut at) = (0.0, 0);
    while at <= HALF {
        let across = 2.0 * at as f64 / HALF as f64 - 1.0;
        let weight = bessel(PI * alpha * sqrt((1.0 - across * across) as f32) as f64);
        total += weight;
        if at < HALF {
            kernel[at] = weight;
            shapes[SINE][at] = sine_cosine((2 * at + 1) as f64 / (8 * HALF) as f64).0 as f32;
        }
        at += 1;
    }
    let (mut sum, mut at) = (0.0, 0);
    while at < HALF {
        sum += kernel[at];
        shapes[KAISER_BESSEL][at] = sqrt((sum / total) as f32);
        at += 1;
    }
    shapes
}

// The samples of the inverse transform of `lines` before they are windowed, the cosine transform's values folded out to twice as many.
fn unfold(lines: &[f32], scale: f32, out: &mut [f32]) {
    let (size, half) = (lines.len(), lines.len() / 2);
    for at in 0..half {
        out[at] = lines[half + at] * scale;
        out[half + at] = -lines[size - 1 - at] * scale;
        out[size + at] = -lines[half - 1 - at] * scale;
        out[size + half + at] = -lines[at] * scale;
    }
}

pub(super) struct Transform {
    long: Dct,
    short: Dct,
    folded: Vec<f32>,
    block: Vec<f32>,
    window: Vec<f32>,
    part: Vec<f32>,
}

impl Transform {
    pub(super) fn new() -> Transform {
        Transform {
            long: Dct::new(&LONG_TWIDDLES),
            short: Dct::new(&SHORT_TWIDDLES),
            folded: vec![0.0; FRAME],
            block: vec![0.0; 2 * FRAME],
            window: vec![0.0; 2 * FRAME],
            part: vec![0.0; 2 * SHORT],
        }
    }

    // The spectrum of the frame before `current` and `current` under the long sine window.
    pub(super) fn forward(&mut self, previous: &[f32], current: &[f32], spectrum: &mut [f32]) {
        let window = &LONG_WINDOWS[SINE];
        for at in 0..FRAME / 2 {
            self.folded[at] = -current[FRAME / 2 - 1 - at] * window[FRAME / 2 + at]
                - current[FRAME / 2 + at] * window[FRAME / 2 - 1 - at];
            self.folded[FRAME / 2 + at] = previous[at] * window[at] - previous[FRAME - 1 - at] * window[FRAME - 1 - at];
        }
        self.long.transform(&self.folded, spectrum);
        spectrum.iter_mut().for_each(|line| *line *= 2.0);
    }

    // The window of a block of one long window: its left half has the shape the block before had, its right half the shape of this one.
    fn lay(&mut self, sequence: Sequence, before: usize, now: usize) {
        let ends = FIRST_SHORT + SHORT;
        for at in 0..FRAME {
            self.window[at] = match sequence {
                Sequence::Stop if at < FIRST_SHORT => 0.0,
                Sequence::Stop if at < ends => SHORT_WINDOWS[before][at - FIRST_SHORT],
                Sequence::Stop => 1.0,
                _ => LONG_WINDOWS[before][at],
            };
            self.window[FRAME + at] = match sequence {
                Sequence::Start if at < FIRST_SHORT => 1.0,
                Sequence::Start if at < ends => SHORT_WINDOWS[now][ends - 1 - at],
                Sequence::Start => 0.0,
                _ => LONG_WINDOWS[now][FRAME - 1 - at],
            };
        }
    }

    // The samples a block's spectrum completes go to `out`, those the next block completes to `overlap`; the lines of a block of short windows follow one another, and `shapes` are those of the block before and of this one.
    pub(super) fn inverse(
        &mut self,
        spectrum: &[f32],
        sequence: Sequence,
        shapes: [usize; 2],
        overlap: &mut [f32],
        out: &mut [f32],
    ) {
        let [before, now] = shapes;
        if sequence == Sequence::Short {
            self.block.fill(0.0);
            for (number, lines) in spectrum.as_chunks::<SHORT>().0.iter().enumerate() {
                self.short.transform(lines, &mut self.folded[..SHORT]);
                unfold(&self.folded[..SHORT], 1.0 / SHORT as f32, &mut self.part);
                let (rising, falling) = (&SHORT_WINDOWS[if number == 0 { before } else { now }], &SHORT_WINDOWS[now]);
                for at in 0..SHORT {
                    self.part[at] *= rising[at];
                    self.part[SHORT + at] *= falling[SHORT - 1 - at];
                }
                let begin = FIRST_SHORT + number * SHORT;
                for (sample, &part) in self.block[begin..begin + 2 * SHORT].iter_mut().zip(&self.part) {
                    *sample += part;
                }
            }
        } else {
            self.long.transform(spectrum, &mut self.folded);
            unfold(&self.folded, 1.0 / FRAME as f32, &mut self.block);
            self.lay(sequence, before, now);
            for (sample, &weight) in self.block.iter_mut().zip(&self.window) {
                *sample *= weight;
            }
        }
        for at in 0..FRAME {
            out[at] = overlap[at] + self.block[at];
            overlap[at] = self.block[FRAME + at];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::{bessel_by_integral, iterated, next_random};

    // Four times the worst error measured for lines and samples of up to 32768, 0.0006 and 0.0117, which is an f32's own.
    const BLOCK_ERROR: f64 = 0.0025;
    const CHAIN_ERROR: f64 = 0.05;

    fn noise(count: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..count).map(|_| (next_random(&mut state) >> 16) as f32 - 32768.0).collect()
    }

    #[test]
    fn fast_transform_matches_definition() {
        let samples = noise(2 * FRAME, 7);
        let mut spectrum = vec![0.0; FRAME];
        Transform::new().forward(&samples[..FRAME], &samples[FRAME..], &mut spectrum);
        let length = (2 * FRAME) as f64;
        for line in (0..FRAME).step_by(37).chain([1, 2, FRAME - 2, FRAME - 1]) {
            let sum: f64 = samples
                .iter()
                .enumerate()
                .map(|(at, &sample)| {
                    let window = sine_cosine((2 * at + 1) as f64 / (2.0 * 2.0 * length)).0;
                    let phase = (at as f64 + (FRAME + 1) as f64 / 2.0) * (line as f64 + 0.5) / length;
                    f64::from(sample) * window * sine_cosine(phase).1
                })
                .sum();
            assert!(
                (f64::from(spectrum[line]) - 2.0 * sum).abs() < 16.0,
                "line {line}: {} for {}",
                spectrum[line],
                2.0 * sum
            );
        }
    }

    #[test]
    fn overlap_add_reconstructs() {
        let samples = noise(5 * FRAME, 3);
        let mut transform = Transform::new();
        let (mut spectrum, mut overlap, mut out) = (vec![0.0; FRAME], vec![0.0; FRAME], vec![0.0; FRAME]);
        let silence = vec![0.0; FRAME];
        for (number, current) in samples.as_chunks::<FRAME>().0.iter().enumerate() {
            let previous = if number == 0 { &silence[..] } else { &samples[(number - 1) * FRAME..number * FRAME] };
            transform.forward(previous, current, &mut spectrum);
            transform.inverse(&spectrum, Sequence::Long, [SINE; 2], &mut overlap, &mut out);
            for (at, (&actual, &expected)) in out.iter().zip(previous).enumerate() {
                assert!((actual - expected).abs() < 0.5, "frame {number} sample {at}: {actual} for {expected}");
            }
        }
    }

    // The half of a long window and of a short one, each with its alpha and the rising half of both its shapes.
    fn both_windows() -> [(usize, f64, [&'static [f32]; 2]); 2] {
        [
            (FRAME, LONG_ALPHA, LONG_WINDOWS.each_ref().map(|shape| &shape[..])),
            (SHORT, SHORT_ALPHA, SHORT_WINDOWS.each_ref().map(|shape| &shape[..])),
        ]
    }

    // Both shapes of a window of 2 * `half` samples as iterators make them.
    fn iterated_windows(half: usize, alpha: f64) -> [Vec<f32>; 2] {
        let sine = (0..half).map(|at| iterated::sine_cosine((2 * at + 1) as f64 / (8 * half) as f64).0 as f32);
        let kernel: Vec<f64> = (0..=half)
            .map(|at| {
                let across = 2.0 * at as f64 / half as f64 - 1.0;
                bessel(PI * alpha * f64::from(iterated::sqrt((1.0 - across * across) as f32)))
            })
            .collect();
        let total: f64 = kernel.iter().sum();
        let mut sum = 0.0;
        let kaiser = kernel[..half].iter().map(|&weight| {
            sum += weight;
            iterated::sqrt((sum / total) as f32)
        });
        let mut shapes = [Vec::new(), Vec::new()];
        (shapes[SINE], shapes[KAISER_BESSEL]) = (sine.collect(), kaiser.collect());
        shapes
    }

    // The turns of a cosine transform over `size` values, before and after its Fourier transform, as iterators make them.
    fn iterated_twiddles(size: usize) -> [Vec<Complex>; 2] {
        let (frame, points) = (size as f64, size / 2);
        let from_turns = |turns: f64| {
            let (sine, cosine) = iterated::sine_cosine(turns);
            [cosine as f32, sine as f32]
        };
        [
            (0..points).map(|point| from_turns(point as f64 / (2.0 * frame))).collect(),
            (0..points).map(|point| from_turns((4 * point + 1) as f64 / (8.0 * frame))).collect(),
        ]
    }

    #[test]
    fn tables_bit_exact() {
        let bits = |values: &[f32]| values.iter().map(|value| value.to_bits()).collect::<Vec<u32>>();
        for (half, alpha, shapes) in both_windows() {
            for (shape, (made, iterated)) in shapes.iter().zip(iterated_windows(half, alpha)).enumerate() {
                assert_eq!(bits(made), bits(&iterated), "shape {shape} of the window of {} samples", 2 * half);
            }
        }
        let twiddles = [
            (FRAME, [&LONG_TWIDDLES.pre[..], &LONG_TWIDDLES.post[..]]),
            (SHORT, [&SHORT_TWIDDLES.pre[..], &SHORT_TWIDDLES.post[..]]),
        ];
        for (size, turns) in twiddles {
            for (side, (made, iterated)) in turns.iter().zip(iterated_twiddles(size)).enumerate() {
                assert_eq!(bits(made.as_flattened()), bits(iterated.as_flattened()), "turns {side} over {size} values");
            }
        }
    }

    #[test]
    fn window_power_and_kaiser_kernel() {
        for (half, alpha, shapes) in both_windows() {
            for (shape, window) in shapes.iter().enumerate() {
                assert_eq!(window.len(), half);
                assert!(window.windows(2).all(|pair| pair[0] <= pair[1]), "rising, shape {shape}");
                for at in 0..half {
                    let (one, other) = (f64::from(window[at]), f64::from(window[half - 1 - at]));
                    assert!((one * one + other * other - 1.0).abs() < 1.0e-6, "shape {shape} at {at}: power is kept");
                }
            }
            let kernel: Vec<f64> = (0..=half)
                .map(|at| {
                    let across = 2.0 * at as f64 / half as f64 - 1.0;
                    bessel_by_integral(PI * alpha * (1.0 - across * across).sqrt())
                })
                .collect();
            let total: f64 = kernel.iter().sum();
            let mut sum = 0.0;
            for at in 0..half {
                sum += kernel[at];
                let expected = (sum / total).sqrt();
                assert!((f64::from(shapes[KAISER_BESSEL][at]) - expected).abs() < 2.0e-6, "alpha {alpha} at {at}");
            }
            let sine = |at: usize| (PI * (at as f64 + 0.5) / (2 * half) as f64).sin();
            for at in (0..half).step_by(7) {
                assert!((f64::from(shapes[SINE][at]) - sine(at)).abs() < 1.0e-6, "the sine window at {at}");
            }
        }
    }

    // The rising half of both shapes of the long window and of the short, in f64.
    fn rises() -> [[Vec<f64>; 2]; 2] {
        let build = |half: usize, alpha: f64| {
            let sine = (0..half).map(|at| (PI * (at as f64 + 0.5) / (2 * half) as f64).sin()).collect();
            let kernel: Vec<f64> = (0..=half)
                .map(|at| {
                    let across = 2.0 * at as f64 / half as f64 - 1.0;
                    bessel_by_integral(PI * alpha * (1.0 - across * across).sqrt())
                })
                .collect();
            let (total, mut sum): (f64, f64) = (kernel.iter().sum(), 0.0);
            let kaiser = kernel[..half]
                .iter()
                .map(|&weight| {
                    sum += weight;
                    (sum / total).sqrt()
                })
                .collect();
            [sine, kaiser]
        };
        [build(FRAME, LONG_ALPHA), build(SHORT, SHORT_ALPHA)]
    }

    // The window of a block of one long window, sample by sample as 4.6.11 lays it out.
    fn long_window(rises: &[[Vec<f64>; 2]; 2], sequence: Sequence, before: usize, now: usize) -> Vec<f64> {
        let [long, short] = rises;
        (0..2 * FRAME)
            .map(|n| match (sequence, n) {
                (Sequence::Stop, 0..448) => 0.0,
                (Sequence::Stop, 448..576) => short[before][n - 448],
                (Sequence::Stop, 576..1024) => 1.0,
                (_, 0..1024) => long[before][n],
                (Sequence::Start, 1024..1472) => 1.0,
                (Sequence::Start, 1472..1600) => short[now][1599 - n],
                (Sequence::Start, _) => 0.0,
                _ => long[now][2047 - n],
            })
            .collect()
    }

    // The window of the `number`th of the eight short windows of a block: its first has the shape the block before had on its left, the others this block's.
    fn short_window(rises: &[[Vec<f64>; 2]; 2], number: usize, before: usize, now: usize) -> Vec<f64> {
        let short = &rises[1];
        let left = if number == 0 { before } else { now };
        (0..2 * SHORT).map(|n| if n < SHORT { short[left][n] } else { short[now][2 * SHORT - 1 - n] }).collect()
    }

    // The angle of the standard's sums, with the shift n0 of the lapped transform.
    fn angle(size: usize, at: usize, line: usize) -> f64 {
        2.0 * PI / size as f64 * (at as f64 + (size / 2 + 1) as f64 / 2.0) * (line as f64 + 0.5)
    }

    fn lines_of(samples: &[f64]) -> Vec<f64> {
        let size = samples.len();
        (0..size / 2)
            .map(|line| {
                2.0 * samples.iter().enumerate().map(|(at, &sample)| sample * angle(size, at, line).cos()).sum::<f64>()
            })
            .collect()
    }

    fn samples_of(lines: &[f64]) -> Vec<f64> {
        let size = 2 * lines.len();
        (0..size)
            .map(|at| {
                2.0 / size as f64
                    * lines.iter().enumerate().map(|(line, &value)| value * angle(size, at, line).cos()).sum::<f64>()
            })
            .collect()
    }

    // The samples a block's spectrum completes and those it leaves for the next block, from the standard's sums and windows.
    fn block_of(
        rises: &[[Vec<f64>; 2]; 2],
        spectrum: &[f32],
        sequence: Sequence,
        [before, now]: [usize; 2],
    ) -> Vec<f64> {
        let lines: Vec<f64> = spectrum.iter().map(|&line| f64::from(line)).collect();
        if sequence == Sequence::Short {
            let mut block = vec![0.0; 2 * FRAME];
            for number in 0..WINDOWS {
                let window = short_window(rises, number, before, now);
                let samples = samples_of(&lines[number * SHORT..(number + 1) * SHORT]);
                for (at, (&sample, &weight)) in samples.iter().zip(&window).enumerate() {
                    block[FIRST_SHORT + number * SHORT + at] += sample * weight;
                }
            }
            block
        } else {
            let window = long_window(rises, sequence, before, now);
            samples_of(&lines).iter().zip(&window).map(|(&sample, &weight)| sample * weight).collect()
        }
    }

    #[test]
    fn blocks_match_standard_sums() {
        let (rises, mut transform) = (rises(), Transform::new());
        let spectrum = noise(FRAME, 11);
        for sequence in [Sequence::Long, Sequence::Start, Sequence::Short, Sequence::Stop] {
            for shapes in [[SINE, SINE], [SINE, KAISER_BESSEL], [KAISER_BESSEL, SINE], [KAISER_BESSEL, KAISER_BESSEL]] {
                let expected = block_of(&rises, &spectrum, sequence, shapes);
                let (mut overlap, mut out) = (vec![0.0; FRAME], vec![0.0; FRAME]);
                transform.inverse(&spectrum, sequence, shapes, &mut overlap, &mut out);
                for (at, (&actual, &expected)) in out.iter().chain(&overlap).zip(&expected).enumerate() {
                    assert!(
                        (f64::from(actual) - expected).abs() < BLOCK_ERROR,
                        "{sequence:?} {shapes:?} sample {at}: {actual} for {expected}"
                    );
                }
            }
        }
    }

    // Every kind of block follows every kind it may, with the shapes changing between them.
    const CHAIN: [(Sequence, usize); 13] = [
        (Sequence::Long, SINE),
        (Sequence::Long, KAISER_BESSEL),
        (Sequence::Start, KAISER_BESSEL),
        (Sequence::Short, SINE),
        (Sequence::Short, KAISER_BESSEL),
        (Sequence::Short, KAISER_BESSEL),
        (Sequence::Stop, SINE),
        (Sequence::Long, KAISER_BESSEL),
        (Sequence::Start, SINE),
        (Sequence::Short, SINE),
        (Sequence::Stop, KAISER_BESSEL),
        (Sequence::Long, SINE),
        (Sequence::Long, SINE),
    ];

    #[test]
    fn chained_blocks_overlap() {
        let (rises, mut transform) = (rises(), Transform::new());
        let samples: Vec<f64> = noise((CHAIN.len() + 1) * FRAME, 5).iter().map(|&sample| f64::from(sample)).collect();
        let (mut overlap, mut out) = (vec![0.0; FRAME], vec![0.0; FRAME]);
        for (number, &(sequence, now)) in CHAIN.iter().enumerate() {
            let before = if number == 0 { SINE } else { CHAIN[number - 1].1 };
            let block = &samples[number * FRAME..number * FRAME + 2 * FRAME];
            let spectrum: Vec<f32> = if sequence == Sequence::Short {
                (0..WINDOWS)
                    .flat_map(|window| {
                        let at = FIRST_SHORT + window * SHORT;
                        let weights = short_window(&rises, window, before, now);
                        let under: Vec<f64> = (0..2 * SHORT).map(|n| block[at + n] * weights[n]).collect();
                        lines_of(&under)
                    })
                    .map(|line| line as f32)
                    .collect()
            } else {
                let weights = long_window(&rises, sequence, before, now);
                let under: Vec<f64> = block.iter().zip(&weights).map(|(&sample, &weight)| sample * weight).collect();
                lines_of(&under).iter().map(|&line| line as f32).collect()
            };
            transform.inverse(&spectrum, sequence, [before, now], &mut overlap, &mut out);
            if number > 0 {
                for (at, &actual) in out.iter().enumerate() {
                    let expected = samples[number * FRAME + at];
                    assert!(
                        (f64::from(actual) - expected).abs() < CHAIN_ERROR,
                        "block {number} ({sequence:?}) sample {at}: {actual} for {expected}"
                    );
                }
            }
        }
    }
}
