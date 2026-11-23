// The local tone map of a burst taken below its light to keep its highlights: the burst fused with itself made as bright as asked, by how well each is exposed (Mertens' exposure fusion, as IPOL's HDR+ finishes), and the gain that makes it so at each quad.

use crate::base::crew::Part;
use crate::base::math::log2_f64;
use crate::base::math::power_f64;
use crate::base::sampling::Weigh;
use crate::base::sampling::filter_down;
use crate::base::sampling::filter_row;
use crate::base::sampling::reflect_101;
use crate::base::sampling::sum_rows;
use crate::gfx::bands;
use crate::hw::PARTS;
use crate::hw::camera::BLUE;
use crate::hw::camera::GREEN;
use crate::hw::camera::LIGHT_STEPS;
use crate::hw::camera::RED;
use crate::hw::camera::UNITY;
use crate::hw::camera::burst::Merged;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::WHITE;
use crate::hw::camera::merge::FINE_BITS;
use crate::hw::camera::mosaic::PAD;
use crate::hw::camera::raw::Square;
use crate::hw::camera::shading::Axis;
use crate::hw::camera::shading::Lens;
use crate::hw::camera::shading::Span;
use crate::hw::camera::tone::Recipe;
use crate::hw::camera::tone::light_to_srgb;
use crate::hw::camera::tone::srgb_to_light;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

// OpenCV's well-exposedness: a Gaussian about mid-scale with a deviation of a fifth of the scale.
const EXPOSED_MIDDLE: f64 = 0.5;
const EXPOSED_DEVIATION: f64 = 0.2;
// OpenCV's `MergeMertens` adds this to every weight, so that no sum of weights is nought.
const WEIGHT_FLOOR: f32 = 1.0e-12;
// OpenCV's `pyrDown` and `pyrUp` kernel, the samples it reaches on either side, and the halving from a level to the next.
const KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
const REACH: usize = KERNEL.len() / 2;
const STEP: usize = 2;

// The samples of a level `samples` long that `pyrUp`'s place `at` of the level `length` long above it takes, by the weights of their taps in the taps' order: the taps whose mirrored place falls on a sample and not on a zero between two.
fn up_taps(at: usize, length: usize, samples: usize) -> impl Iterator<Item = (usize, f32)> {
    KERNEL.iter().enumerate().filter_map(move |(tap, &weight)| {
        let place = reflect_101((at + tap) as isize - REACH as isize, length);
        place.is_multiple_of(STEP).then(|| ((place / STEP).min(samples - 1), weight))
    })
}

#[derive(Clone)]
struct Layer {
    width: usize,
    height: usize,
    values: Vec<f32>,
}

// What runs the parts of a pass, which may run them at once.
type Run<'r> = dyn FnMut(Vec<Part<'_>>) + 'r;

// Has `run` fill each band of rows of `out`, a row `width` long, by `work` from the band's first row, as a part of its own, a band for each of the crew's threads.
fn pass<T: Send>(out: &mut [T], width: usize, run: &mut Run<'_>, work: &(dyn Fn(usize, &mut [T]) + Sync)) {
    let parts = bands(out, width, PARTS).map(|(rows, values)| Box::new(move || work(rows.start, values)) as Part<'_>);
    run(parts.collect());
}

impl Layer {
    fn new(width: usize, height: usize) -> Layer {
        Layer { width, height, values: vec![0.0; width * height] }
    }

    // The rows of the level from `first` on.
    fn rows(&self, first: usize) -> impl Iterator<Item = &[f32]> {
        self.values.chunks_exact(self.width.max(1)).skip(first)
    }

    // OpenCV's `pyrDown`: the kernel both ways, then every other sample.
    fn down(&self, run: &mut Run<'_>) -> Layer {
        let (width, height) = (self.width.div_ceil(STEP), self.height.div_ceil(STEP));
        let mut across = Layer::new(width, self.height);
        pass(&mut across.values, width, run, &|first, out| {
            for (row, out) in self.rows(first).zip(out.chunks_exact_mut(width.max(1))) {
                filter_row(row, (&KERNEL, STEP), reflect_101, out);
            }
        });
        let mut down = Layer::new(width, height);
        pass(&mut down.values, width, run, &|first, out| {
            for (y, out) in (first..).zip(out.chunks_exact_mut(width.max(1))) {
                let row = |at: usize| &across.values[at * width..][..width];
                filter_down(row, (&KERNEL, STEP * y, self.height), reflect_101, out);
            }
        });
        down
    }

    // OpenCV's `pyrUp` to `width` by `height`: zeros between the samples, then the kernel at four times its weight.
    fn up(&self, width: usize, height: usize, run: &mut Run<'_>) -> Layer {
        let mut across = Layer::new(width, self.height);
        // `reflect_101` keeps a place's parity, so a place takes the taps of its own parity alone, the others falling on zeros: inside, where no tap is mirrored or held to the last sample, an even place takes the even taps over the samples about its half, an odd one the odd taps.
        let phases: [Vec<f32>; STEP] =
            core::array::from_fn(|parity| KERNEL.iter().skip(parity).step_by(STEP).copied().collect());
        let inside = width.checked_sub(REACH + 1).map_or(0, |room| room / STEP + 1);
        let pairs = REACH / STEP..inside.min(self.width.saturating_sub(REACH / STEP)).max(REACH / STEP);
        let edges = (0..(STEP * pairs.start).min(width)).chain((STEP * pairs.end).min(width)..width);
        pass(&mut across.values, width, run, &|first, out| {
            let mut sums: [Vec<f32>; STEP] = core::array::from_fn(|_| vec![0.0; pairs.len()]);
            for (row, out) in self.rows(first).zip(out.chunks_exact_mut(width.max(1))) {
                if !pairs.is_empty() {
                    for (parity, (taps, sums)) in phases.iter().zip(&mut sums).enumerate() {
                        let first = pairs.start - (REACH - parity) / STEP;
                        let samples: Vec<&[f32]> = (first..first + taps.len()).map(|at| &row[at..]).collect();
                        sum_rows(&samples, taps, sums);
                    }
                    let outs = out[STEP * pairs.start..STEP * pairs.end].as_chunks_mut::<STEP>().0;
                    for (pair, outs) in outs.iter_mut().enumerate() {
                        outs.iter_mut().zip(&sums).for_each(|(out, sums)| *out = 2.0 * sums[pair]);
                    }
                }
                for x in edges.clone() {
                    let taps = up_taps(x, width, self.width);
                    out[x] = 2.0 * taps.fold(0.0, |sum, (at, weight)| row[at].weigh(weight, sum));
                }
            }
        });
        let mut up = Layer::new(width, height);
        pass(&mut up.values, width, run, &|first, out| {
            for (y, out) in (first..).zip(out.chunks_exact_mut(width.max(1))) {
                let (rows, taps): (Vec<&[f32]>, Vec<f32>) = up_taps(y, height, self.height)
                    .map(|(at, weight)| (&across.values[at * width..][..width], weight))
                    .unzip();
                sum_rows(&rows, &taps, out);
                out.iter_mut().for_each(|value| *value *= 2.0);
            }
        });
        up
    }

    fn combine(&mut self, other: &Layer, with: impl Fn(f32, f32) -> f32 + Sync, run: &mut Run<'_>) {
        let width = self.width;
        pass(&mut self.values, width, run, &|first, values| {
            let others = &other.values[first * width..];
            values.iter_mut().zip(others).for_each(|(value, other)| *value = with(*value, *other));
        });
    }
}

fn gaussian(layer: Layer, count: usize, run: &mut Run<'_>) -> Vec<Layer> {
    let mut pyramid = vec![layer];
    for _ in 0..count {
        let next = pyramid.last().expect("a level").down(run);
        pyramid.push(next);
    }
    pyramid
}

fn laplacian(layer: Layer, count: usize, run: &mut Run<'_>) -> Vec<Layer> {
    let mut pyramid = gaussian(layer, count, run);
    for level in 0..count {
        let (width, height) = (pyramid[level].width, pyramid[level].height);
        let up = pyramid[level + 1].up(width, height, run);
        pyramid[level].combine(&up, |value, up| value - up, run);
    }
    pyramid
}

// A curve in as many steps as the developer's light has.
struct Table(Vec<f32>);

impl Table {
    fn new(curve: impl Fn(f64) -> f64) -> Table {
        Table((0..=LIGHT_STEPS).map(|step| curve(step as f64 / LIGHT_STEPS as f64) as f32).collect())
    }

    fn at(&self, value: f32) -> f32 {
        let place = value.clamp(0.0, 1.0) * LIGHT_STEPS as f32;
        let index = (place as usize).min(LIGHT_STEPS - 1);
        let share = place - index as f32;
        self.0[index] * (1.0 - share) + self.0[index + 1] * share
    }
}

// Exposure fusion of two exposures by their well-exposedness alone, as IPOL has OpenCV's `MergeMertens` do it.
fn fuse(exposures: [Layer; 2], run: &mut Run<'_>) -> Layer {
    let (width, height) = (exposures[0].width, exposures[0].height);
    // As many levels below the picture as OpenCV makes: the whole halvings of its shorter side.
    let count = log2_f64(width.min(height).max(1) as f64) as usize;
    let exposed = Table::new(|value| {
        let off = (value - EXPOSED_MIDDLE) / EXPOSED_DEVIATION;
        power_f64(core::f64::consts::E, -off * off / 2.0)
    });
    // Each exposure's well-exposedness over the two's.
    let mut weights = [Layer::new(width, height), Layer::new(width, height)];
    let [first, second] = &mut weights;
    let (exposed, [short, long]) = (&exposed, &exposures);
    let rows = bands(&mut first.values, width, PARTS).zip(bands(&mut second.values, width, PARTS));
    let parts = rows.map(|((rows, firsts), (_, seconds))| {
        let samples = short.values[rows.start * width..].iter().zip(&long.values[rows.start * width..]);
        Box::new(move || {
            for ((first, second), (&short, &long)) in firsts.iter_mut().zip(seconds.iter_mut()).zip(samples) {
                let (own, other) = (exposed.at(short) + WEIGHT_FLOOR, exposed.at(long) + WEIGHT_FLOOR);
                let total = own + other;
                (*first, *second) = (own / total, other / total);
            }
        }) as Part<'_>
    });
    run(parts.collect());
    let mut sum: Option<Vec<Layer>> = None;
    for (exposure, weight) in exposures.into_iter().zip(weights) {
        let mut images = laplacian(exposure, count, run);
        for (image, share) in images.iter_mut().zip(&gaussian(weight, count, run)) {
            image.combine(share, |value, share| value * share, run);
        }
        match &mut sum {
            None => sum = Some(images),
            Some(sum) => sum.iter_mut().zip(&images).for_each(|(level, add)| level.combine(add, |a, b| a + b, run)),
        }
    }
    let mut pyramid = sum.expect("two exposures");
    for level in (1..pyramid.len()).rev() {
        let (width, height) = (pyramid[level - 1].width, pyramid[level - 1].height);
        let up = pyramid[level].up(width, height, run);
        pyramid[level - 1].combine(&up, |value, up| value + up, run);
    }
    pyramid.swap_remove(0)
}

// The gain of the local tone map at each quad of a merge, in 1024ths.
pub(super) struct GainMap {
    side: usize,
    gains: Vec<u16>,
}

impl GainMap {
    // The map of `merged`, taken `under` (in 1024ths) below the light `recipe` asks for, its lens made up for over `view`, its work in parts `run` may run at once; none where it was not taken below.
    pub(super) fn new(
        merged: &Merged,
        recipe: &Recipe,
        under: i32,
        lens: Option<(Lens, Square)>,
        run: &mut Run<'_>,
    ) -> Option<GainMap> {
        if under <= UNITY {
            return None;
        }
        let planes = &merged.planes;
        let (side, stride) = (planes.side(), planes.stride());
        let range = ((WHITE - BLACK) << FINE_BITS) as f32;
        let lift = under as f32 / UNITY as f32;
        let span = lens.map(|(lens, view)| {
            let area = lens.area;
            (
                lens,
                view,
                Span::evenly(Axis::Horizontal, area.left + area.scale * view.left, area.scale * view.side, side, false),
            )
        });
        let (mut short, mut long) = (Layer::new(side, side), Layer::new(side, side));
        // The short exposure's light again, which the gains take it out of.
        let mut shorts = vec![0.0; side * side];
        let (compress, expand) = (Table::new(light_to_srgb), Table::new(srgb_to_light));
        let (span, compress, expand) = (&span, &compress, &expand);
        let rows = bands(&mut short.values, side, PARTS).zip(bands(&mut long.values, side, PARTS));
        let parts =
            rows.zip(bands(&mut shorts, side, PARTS)).map(|(((rows, short_rows), (_, long_rows)), (_, light_rows))| {
                let first = rows.start;
                Box::new(move || {
                    let mut shades: [Vec<u16>; 3] = core::array::from_fn(|_| vec![UNITY as u16; side]);
                    let rows = short_rows.chunks_exact_mut(side).zip(long_rows.chunks_exact_mut(side));
                    for (row, ((shorts, longs), lights)) in (first..).zip(rows.zip(light_rows.chunks_exact_mut(side))) {
                        if let Some((lens, view, span)) = span {
                            let line = lens.area.top + lens.area.scale * (view.top + 2 * row as u32 + 1);
                            for (gains, points) in shades.iter_mut().zip(&lens.shading.profile(Axis::Horizontal, line))
                            {
                                span.fill(points, 0, gains);
                            }
                        }
                        let at = (row + PAD) * stride + PAD;
                        for (column, ((short, long), own)) in shorts.iter_mut().zip(longs).zip(lights).enumerate() {
                            let sample = |plane: usize| {
                                f32::from(planes.plane(plane)[at + column]) - (BLACK << FINE_BITS) as f32
                            };
                            let raw = [sample(2), (sample(0) + sample(3)) / 2.0, sample(1)];
                            let light: [f32; 3] = core::array::from_fn(|colour| {
                                let gain = recipe.gains[colour] as f32 * f32::from(shades[colour][column])
                                    / (UNITY * UNITY) as f32;
                                (raw[colour].max(0.0) / range * gain).min(1.0)
                            });
                            let brighter = light.map(|light| (light * lift).min(1.0));
                            *short = compress.at((light[RED] + light[GREEN] + light[BLUE]) / 3.0);
                            *long = compress.at((brighter[RED] + brighter[GREEN] + brighter[BLUE]) / 3.0);
                            *own = expand.at(*short);
                        }
                    }
                }) as Part<'_>
            });
        run(parts.collect());
        let fused = fuse([short, long], run);
        let mut gains = vec![0; side * side];
        let (fused, shorts) = (&fused.values, &shorts);
        pass(&mut gains, side, run, &|first, gains| {
            for ((gain, &fused), &short) in gains.iter_mut().zip(&fused[first * side..]).zip(&shorts[first * side..]) {
                // As IPOL scales: a quad of no light is left as it is.
                let lift = if short > 0.0 { expand.at(fused) / short } else { 1.0 };
                *gain = (lift * UNITY as f32 + 0.5).clamp(0.0, f32::from(u16::MAX)) as u16;
            }
        });
        Some(GainMap { side, gains })
    }

    // The gain at the square's pixel at `x` and `y`, between the middles of the quads about it.
    #[inline]
    pub(super) fn gain(&self, x: usize, y: usize) -> i32 {
        let last = self.side - 1;
        let place = |at: usize| {
            let middle = (at as f32 - 0.5) / 2.0;
            let low = (middle.max(0.0) as usize).min(last);
            (low, (low + 1).min(last), (middle - low as f32).clamp(0.0, 1.0))
        };
        let ((left, right, across), (upper, lower, down)) = (place(x), place(y));
        let at = |row: usize, column: usize| f32::from(self.gains[row * self.side + column]);
        let top = at(upper, left) * (1.0 - across) + at(upper, right) * across;
        let bottom = at(lower, left) * (1.0 - across) + at(lower, right) * across;
        (top * (1.0 - down) + bottom * down + 0.5) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::crew::in_order;
    use crate::hw::camera::burst::fake::merged;
    use crate::hw::camera::frame::Frame;
    use crate::hw::camera::frame::fake::packed;
    use crate::hw::camera::shading::SensorArea;
    use crate::hw::camera::shading::fake;
    use crate::hw::camera::tone::Recipe;

    #[test]
    fn pyramid_steps() {
        let flat = Layer { width: 9, height: 6, values: vec![3.0; 54] };
        let down = flat.down(&mut in_order);
        assert_eq!((down.width, down.height), (5, 3));
        assert!(down.values.iter().all(|&value| (value - 3.0).abs() < 1e-5), "the kernel keeps a flat level flat");
        let up = down.up(9, 6, &mut in_order);
        assert!(up.values.iter().all(|&value| (value - 3.0).abs() < 1e-5), "and so does the way back");
        let laplacian = laplacian(flat, 2, &mut in_order);
        assert!(laplacian[0].values.iter().all(|value| value.abs() < 1e-5), "a flat level has no detail");
    }

    #[test]
    fn up_as_zeros_between() {
        let mut state = 5u32;
        let sizes: [(usize, usize); 12] =
            [(1, 1), (2, 3), (3, 2), (4, 5), (5, 4), (6, 6), (7, 9), (9, 6), (12, 11), (19, 20), (77, 39), (153, 154)];
        for (width, height) in sizes {
            let (small_width, small_height) = (width.div_ceil(STEP), height.div_ceil(STEP));
            let values = (0..small_width * small_height)
                .map(|_| {
                    state = crate::base::math::xorshift32(state);
                    state as f32 / u32::MAX as f32
                })
                .collect();
            let small = Layer { width: small_width, height: small_height, values };
            // Each place of each pass by the taps that fall on a sample, as `pyrUp` has it.
            let sum = |taps: &mut dyn Iterator<Item = (usize, f32)>, sample: &dyn Fn(usize) -> f32| {
                2.0 * taps.fold(0.0, |sum, (at, weight)| sum + weight * sample(at))
            };
            let across: Vec<f32> = (0..small_height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| sum(&mut up_taps(x, width, small_width), &|at| small.values[y * small_width + at]))
                .collect();
            let direct: Vec<u32> = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| sum(&mut up_taps(y, height, small_height), &|at| across[at * width + x]).to_bits())
                .collect();
            let up: Vec<u32> =
                small.up(width, height, &mut in_order).values.iter().map(|value| value.to_bits()).collect();
            assert_eq!(up, direct, "{width}x{height} from {small_width}x{small_height}, bit for bit");
        }
    }

    #[test]
    fn shadows_lifted_highlights_kept() {
        let frame = Frame::whole(96, 64);
        let raw = packed(frame, |x, _| if x < 48 { BLACK + 30 } else { WHITE - 40 });
        let merge = merged(&raw, frame);
        assert!(
            GainMap::new(&merge, &Recipe::PLAIN, UNITY, None, &mut in_order).is_none(),
            "a burst taken at its light keeps its tones"
        );
        let map = GainMap::new(&merge, &Recipe::PLAIN, 4 * UNITY, None, &mut in_order).unwrap();
        let (dark, bright) = (map.gain(20, 30), map.gain(60, 30));
        assert!(dark > 2 * UNITY, "the dark half lifted toward the brighter exposure: {dark}");
        assert!(bright < dark && bright <= 3 * UNITY / 2, "the bright half kept near its own light: {bright}");
    }

    #[test]
    fn lens_lift() {
        let frame = Frame::whole(96, 64);
        let scene = |twice: u32| packed(frame, move |x, _| BLACK + twice * if x < 48 { 30 } else { 300 });
        let halving = Lens { area: SensorArea { left: 0, top: 0, scale: 34 }, ..fake::lens(|_, _, _| 2048) };
        let lens = Some((halving, Square::middle_of(frame)));
        let dimmed = GainMap::new(&merged(&scene(1), frame), &Recipe::PLAIN, 4 * UNITY, lens, &mut in_order).unwrap();
        let plain = GainMap::new(&merged(&scene(2), frame), &Recipe::PLAIN, 4 * UNITY, None, &mut in_order).unwrap();
        assert!(dimmed.gains == plain.gains, "a lens that halves all light: the map of the scene at twice its codes");
        assert!(plain.gains.iter().any(|&gain| i32::from(gain) > UNITY), "and it lifts");
    }

    #[test]
    fn parts_on_threads_match_in_order() {
        extern crate std;
        let frame = Frame::whole(150, 110);
        let raw = packed(frame, |x, y| BLACK + (37 * x + 101 * y + x * y) % 900);
        let (merge, area) = (merged(&raw, frame), SensorArea { left: 0, top: 0, scale: 34 });
        let lens = Lens { area, ..fake::lens(|colour, row, column| (1024 + 40 * (colour + row + column)) as u16) };
        let made = |run: &mut Run<'_>| {
            GainMap::new(&merge, &Recipe::PLAIN, 4 * UNITY, Some((lens, Square::middle_of(frame))), run).unwrap().gains
        };
        // Each pass's parts on threads of their own, started last first, as a crew may end them in any order.
        let threaded = made(&mut |parts: Vec<Part<'_>>| {
            std::thread::scope(|scope| {
                parts.into_iter().rev().for_each(|part| {
                    scope.spawn(part);
                });
            });
        });
        assert!(threaded == made(&mut in_order), "the map made in parts on threads is the map made in order");
        assert!(threaded.iter().any(|&gain| gain != threaded[0]), "a map with something in it");
    }
}
