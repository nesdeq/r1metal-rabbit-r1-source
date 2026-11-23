// A burst merged tile by tile in the frequency domain, each frame weighed by how far it differs from the reference, then smoothed where noise lies (IPOL's HDR+).

use crate::base::fft::Fft;
use crate::base::fft::Rows;
use crate::base::fft::SIDE;
use crate::base::fft::Tile;
use crate::base::math::sine_cosine;
use crate::base::math::sqrt;
use crate::hw::camera::align::Field;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::WHITE;
use crate::hw::camera::mosaic::Planes;
use crate::hw::camera::noise::Noise;
use core::ops::Range;

const STEP: usize = SIDE / 2;
// IPOL's defaults for how far a frame that differs is left out and how far the merge is smoothed (`runHdrplus.py`).
const TEMPORAL: f32 = 75.0;
const SPATIAL: f32 = 0.1;
// A tile's transform against its samples' variance, as IPOL's merge scales it: the side squared over 16.
const SCALE: f32 = (SIDE * SIDE) as f32 / 16.0;
// Fraction bits of a merged sample against the sensor's codes.
pub(super) const FINE_BITS: u32 = 4;

// Tile rows `2 k + parity` for k in `ks` of one pair of planes, and the rows of the merged planes they add to.
pub(super) struct Strip<'a> {
    pair: usize,
    parity: usize,
    ks: Range<usize>,
    out: [&'a mut [f32]; 2],
}

// What every tile's merge looks up: the transform, the window, and each frequency's distance from nought.
pub(super) struct Merger {
    fft: Fft,
    window: [f32; SIDE],
    reach: Rows,
}

impl Default for Merger {
    fn default() -> Merger {
        let window = core::array::from_fn(|at| (0.5 - 0.5 * sine_cosine((at as f64 + 0.5) / SIDE as f64).1) as f32);
        let frequency = |at: usize| at.min(SIDE - at) as f32;
        let reach = core::array::from_fn(|u| {
            core::array::from_fn(|v| sqrt(frequency(u) * frequency(u) + frequency(v) * frequency(v)))
        });
        Merger { fft: Fft::default(), window, reach }
    }
}

fn load(plane: &[u16], stride: usize, (y, x): (usize, usize)) -> Rows {
    core::array::from_fn(|row| {
        let line = &plane[(y + row) * stride + x..][..SIDE];
        core::array::from_fn(|column| f32::from(line[column]))
    })
}

fn mean(rows: &Rows) -> f32 {
    rows.iter().flatten().sum::<f32>() / (SIDE * SIDE) as f32
}

// Adds the alternate's tile, moved toward the reference's as far as they differ beyond the noise.
fn add_weighed(sum: &mut Tile, reference: &Tile, alternate: &Tile, noise: f32) {
    for u in 0..SIDE {
        for v in 0..SIDE {
            let (re, im) = (reference.re[u][v] - alternate.re[u][v], reference.im[u][v] - alternate.im[u][v]);
            let distance = re * re + im * im;
            let share = distance / (distance + noise);
            sum.re[u][v] += alternate.re[u][v] + share * re;
            sum.im[u][v] += alternate.im[u][v] + share * im;
        }
    }
}

impl Merger {
    fn smooth(&self, sum: &mut Tile, frames: f32, variance: f32) {
        let noise = SCALE * SPATIAL * variance / frames;
        for u in 0..SIDE {
            for v in 0..SIDE {
                let (re, im) = (sum.re[u][v] / frames, sum.im[u][v] / frames);
                let power = re * re + im * im;
                let keep = if power > 0.0 { power / (power + self.reach[u][v] * noise) } else { 0.0 };
                (sum.re[u][v], sum.im[u][v]) = (re * keep, im * keep);
            }
        }
    }

    // Merges a strip's tiles of every frame in `frames`, each laid on the reference by its field, into the strip's rows.
    pub(super) fn merge(
        &self,
        strip: &mut Strip<'_>,
        frames: &[Planes],
        fields: &[Option<Field>],
        reference: usize,
        noise: Noise,
    ) {
        let planes = &frames[reference];
        let (stride, last) = (planes.stride(), planes.stride() - SIDE);
        let columns = stride / STEP - 1;
        let pair = [2 * strip.pair, 2 * strip.pair + 1];
        let count = frames.len() as f32;
        let temporal = SCALE * 2.0 * TEMPORAL;
        for k in strip.ks.clone() {
            let row = 2 * k + strip.parity;
            let first_row = (2 * strip.ks.start + strip.parity) * STEP;
            for column in 0..columns {
                let origin = (row * STEP, column * STEP);
                let (first, second) =
                    (load(planes.plane(pair[0]), stride, origin), load(planes.plane(pair[1]), stride, origin));
                let variances = [mean(&first), mean(&second)].map(|level| noise.variance(level - BLACK as f32));
                let mut tile = Tile::pair(first, second);
                self.fft.forward(&mut tile);
                let (own_first, own_second) = tile.split();
                let (mut sum_first, mut sum_second) = (own_first, own_second);
                for (other, field) in frames.iter().zip(fields) {
                    // The reference has none.
                    let Some(field) = field else { continue };
                    let (down, across) = field.offset(row, column);
                    let place = |at: usize, by: i32| (at as i64 + i64::from(by)).clamp(0, last as i64) as usize;
                    let moved = (place(origin.0, down), place(origin.1, across));
                    let mut tile = Tile::pair(
                        load(other.plane(pair[0]), stride, moved),
                        load(other.plane(pair[1]), stride, moved),
                    );
                    self.fft.forward(&mut tile);
                    let (other_first, other_second) = tile.split();
                    add_weighed(&mut sum_first, &own_first, &other_first, temporal * variances[0]);
                    add_weighed(&mut sum_second, &own_second, &other_second, temporal * variances[1]);
                }
                self.smooth(&mut sum_first, count, variances[0]);
                self.smooth(&mut sum_second, count, variances[1]);
                let mut merged = Tile::join(&sum_first, &sum_second);
                self.fft.inverse(&mut merged);
                for (out, rows) in strip.out.iter_mut().zip([&merged.re, &merged.im]) {
                    for (line, samples) in rows.iter().enumerate() {
                        let start = (origin.0 + line - first_row) * stride + origin.1;
                        for ((sum, sample), weight) in
                            out[start..start + SIDE].iter_mut().zip(samples).zip(&self.window)
                        {
                            *sum += sample * weight * self.window[line];
                        }
                    }
                }
            }
        }
    }
}

// Writes merged sums as samples of `FINE_BITS` more bits than the sensor's, held to its range.
pub(super) fn to_fine(sums: &[f32], samples: &mut [u16]) {
    let (scale, top) = ((1 << FINE_BITS) as f32, (WHITE << FINE_BITS) as f32);
    for (sample, sum) in samples.iter_mut().zip(sums) {
        *sample = (sum * scale + 0.5).clamp(0.0, top) as u16;
    }
}

// The strips of one parity of tile rows for a pair of planes, about `count` of them of as many tile rows each, over the sums of those two planes.
pub(super) fn strips<'a>(
    sums: [&'a mut [f32]; 2],
    stride: usize,
    (pair, parity): (usize, usize),
    count: usize,
) -> impl Iterator<Item = Strip<'a>> {
    let tile_rows = stride / STEP - 1;
    let ks = (tile_rows + 1 - parity) / 2;
    let per_strip = ks.div_ceil(count.max(1)).max(1);
    let rows = per_strip * SIDE * stride;
    let [first, second] = sums.map(|sums| sums[parity * STEP * stride..].chunks_mut(rows));
    first.zip(second).enumerate().map(move |(index, (first, second))| {
        let start = index * per_strip;
        Strip { pair, parity, ks: start..(start + per_strip).min(ks), out: [first, second] }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;
    use crate::hw::camera::align::Image;
    use crate::hw::camera::align::fake::still;
    use crate::hw::camera::mosaic::PAD;
    use crate::hw::camera::noise::fake::with;
    use alloc::vec;
    use alloc::vec::Vec;

    const SIDE_SAMPLES: usize = 64;

    fn burst(frames: usize, deviation: f32, moved: Option<(usize, u16)>) -> Vec<Planes> {
        let mut state = 5u32;
        (0..frames)
            .map(|frame| {
                let mut planes = Planes::new(SIDE_SAMPLES);
                let stride = planes.stride();
                for mut band in planes.bands(stride) {
                    for (plane, samples) in band.rows_mut().iter_mut().enumerate() {
                        for (at, sample) in samples.iter_mut().enumerate() {
                            let (x, y) = (at % stride, at / stride);
                            let mut noise = 0.0;
                            for _ in 0..12 {
                                state = xorshift32(state);
                                noise += state as f32 / u32::MAX as f32;
                            }
                            let light = 200.0 + 2.0 * x as f32 + y as f32 + 30.0 * plane as f32;
                            let blot = match moved {
                                Some((at_frame, lift))
                                    if frame == at_frame && (20..36).contains(&x) && (20..36).contains(&y) =>
                                {
                                    f32::from(lift)
                                }
                                _ => 0.0,
                            };
                            *sample = (light + blot + deviation * (noise - 6.0)) as u16;
                        }
                    }
                }
                planes
            })
            .collect()
    }

    fn merged(frames: &[Planes], noise: Noise) -> [Vec<f32>; 4] {
        let stride = frames[0].stride();
        let grey = Image::new(stride, stride, vec![0; stride * stride]);
        let fields: Vec<Option<Field>> = (0..frames.len()).map(|at| (at != 0).then(|| still(&grey, SIDE))).collect();
        let merger = Merger::default();
        let mut sums: [Vec<f32>; 4] = core::array::from_fn(|_| vec![0.0; stride * stride]);
        for parity in 0..2 {
            for pair in 0..2 {
                let [first, second, third, fourth] = sums.each_mut();
                let out = if pair == 0 {
                    [first.as_mut_slice(), second.as_mut_slice()]
                } else {
                    [third.as_mut_slice(), fourth.as_mut_slice()]
                };
                for mut strip in strips(out, stride, (pair, parity), 3) {
                    merger.merge(&mut strip, frames, &fields, 0, noise);
                }
            }
        }
        sums
    }

    #[test]
    fn one_frame_comes_back() {
        let frames = burst(1, 0.0, None);
        let sums = merged(&frames, with(0.0, 0.0));
        let stride = frames[0].stride();
        for (plane, sums) in sums.iter().enumerate() {
            for y in PAD..PAD + SIDE_SAMPLES {
                for x in PAD..PAD + SIDE_SAMPLES {
                    let (sum, sample) = (sums[y * stride + x], f32::from(frames[0].plane(plane)[y * stride + x]));
                    assert!((sum - sample).abs() < 0.05, "plane {plane} at {x} {y}: {sum} for {sample}");
                }
            }
        }
    }

    #[test]
    fn merge_noise_and_motion() {
        let deviation = 8.0;
        let noise = with(0.0, deviation * deviation);
        let variance = |frames: &[Planes], sums: Option<&[Vec<f32>; 4]>| {
            let stride = frames[0].stride();
            let mut steps = Vec::new();
            for y in PAD + 2..PAD + 18 {
                for x in PAD + 40..PAD + SIDE_SAMPLES - 2 {
                    let at = y * stride + x;
                    let value = sums.map_or(f32::from(frames[0].plane(0)[at]), |sums| sums[0][at]);
                    steps.push(value - (200.0 + 2.0 * x as f32 + y as f32));
                }
            }
            let mean = steps.iter().sum::<f32>() / steps.len() as f32;
            steps.iter().map(|step| (step - mean) * (step - mean)).sum::<f32>() / steps.len() as f32
        };
        let frames = burst(8, deviation, Some((3, 300)));
        let sums = merged(&frames, noise);
        let (before, after) = (variance(&frames, None), variance(&frames, Some(&sums)));
        // Each frame keeps a sixth of the reference where they differ by noise alone, so eight make less than an eightfold cut.
        assert!(after * 5.0 < before, "eight frames merged: variance {after} from {before}");
        let stride = frames[0].stride();
        let at = (PAD + 28) * stride + PAD + 28;
        assert!(
            (sums[0][at] - f32::from(frames[0].plane(0)[at])).abs() < 40.0,
            "where one frame holds a thing the others do not, the merge keeps to the reference: {} for {}",
            sums[0][at],
            frames[0].plane(0)[at]
        );
        let mut fine = [0u16; 3];
        to_fine(&[-3.0, 100.25, 5000.0], &mut fine);
        assert_eq!(
            fine,
            [0, 1604, (WHITE << FINE_BITS) as u16],
            "sums in sixteenths of a code, held to the sensor's range"
        );
    }
}
