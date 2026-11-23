// A burst's noise: how a raw sample's variance grows with its light, from the differences between the reference and a frame laid on it.

use crate::hw::camera::align::Field;
use crate::hw::camera::align::Image;
use crate::hw::camera::frame::BLACK;
use alloc::vec::Vec;

// The variance of rounding to whole codes, a uniform error of one code.
const ROUNDING: f32 = 1.0 / 12.0;

// The variance of a raw sample `light` codes above black: `per_code` times the light, plus `floor`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Noise {
    per_code: f32,
    floor: f32,
}

impl Default for Noise {
    // A burst of one frame, or none to measure by: rounding alone.
    fn default() -> Noise {
        Noise { per_code: 0.0, floor: ROUNDING }
    }
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[values.len() / 2]
}

impl Noise {
    pub(super) fn variance(&self, light: f32) -> f32 {
        self.per_code * light.max(0.0) + self.floor
    }

    pub(super) const fn per_code(&self) -> f32 {
        self.per_code
    }

    pub(super) const fn floor(&self) -> f32 {
        self.floor
    }

    // The noise of a burst from its reference's grey and another frame's laid on it by `field`, tile by tile of the field's grid.
    pub(super) fn measure(reference: &Image, other: &Image, field: &Field) -> Noise {
        let (width, tile) = (reference.width(), field.tile());
        let mut tiles: Vec<(f32, f32)> = Vec::with_capacity(field.rows() * field.columns());
        for row in 0..field.rows() {
            for column in 0..field.columns() {
                let (down, across) = field.offset(row, column);
                let (x, y) = (column * tile / 2, row * tile / 2);
                let (other_x, other_y) = (x as i64 + i64::from(across), y as i64 + i64::from(down));
                if other_x < 0
                    || other_y < 0
                    || other_x as usize + tile > width
                    || other_y as usize + tile > other.height()
                {
                    continue;
                }
                let (mut light, mut sum, mut squares) = (0i64, 0i64, 0i64);
                for line in 0..tile {
                    let own = &reference.samples()[(y + line) * width + x..][..tile];
                    let theirs = &other.samples()[(other_y as usize + line) * width + other_x as usize..][..tile];
                    for (a, b) in own.iter().zip(theirs) {
                        let step = i64::from(*a) - i64::from(*b);
                        (light, sum, squares) = (light + i64::from(*a), sum + step, squares + step * step);
                    }
                }
                let count = (tile * tile) as f64;
                let mean_step = sum as f64 / count;
                let variance = squares as f64 / count - mean_step * mean_step;
                // A grey sample is the mean of four raw ones, and a step the difference of two greys: a raw sample's variance is twice the step's.
                tiles.push(((light as f64 / count) as f32 - BLACK as f32, (2.0 * variance) as f32));
            }
        }
        Noise::fit(&mut tiles)
    }

    // A line through the medians of the darker half of the tiles and of the brighter half; medians, so that tiles a moving thing or a misplaced edge spoils do not count.
    fn fit(tiles: &mut [(f32, f32)]) -> Noise {
        if tiles.len() < 2 {
            return Noise::default();
        }
        tiles.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let (darker, brighter) = tiles.split_at_mut(tiles.len() / 2);
        let middle = |half: &mut [(f32, f32)]| {
            let mut lights: Vec<f32> = half.iter().map(|tile| tile.0).collect();
            let mut variances: Vec<f32> = half.iter().map(|tile| tile.1).collect();
            (median(&mut lights), median(&mut variances))
        };
        let ((dark_light, dark_variance), (bright_light, bright_variance)) = (middle(darker), middle(brighter));
        let per_code = if bright_light > dark_light {
            ((bright_variance - dark_variance) / (bright_light - dark_light)).max(0.0)
        } else {
            0.0
        };
        Noise { per_code, floor: (dark_variance - per_code * dark_light).max(ROUNDING) }
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;

    // Noise of `per_code` times the light plus `floor`.
    pub(crate) const fn with(per_code: f32, floor: f32) -> Noise {
        Noise { per_code, floor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;
    use crate::hw::camera::align::fake::still;
    use alloc::vec;

    // A grey picture whose raw samples carry noise of variance `per_code` times their light plus `floor`, as the mean of four.
    fn noisy(width: usize, height: usize, per_code: f32, floor: f32, seed: u32) -> Image {
        let mut state = seed;
        let mut gaussian = || {
            let sum: f32 = (0..12)
                .map(|_| {
                    state = xorshift32(state);
                    state as f32 / u32::MAX as f32
                })
                .sum();
            sum - 6.0
        };
        let samples = (0..width * height)
            .map(|at| {
                let light = 20.0 + 600.0 * (at % width) as f32 / width as f32;
                let deviation = crate::base::math::sqrt((per_code * light + floor) / 4.0);
                (BLACK as f32 + light + deviation * gaussian()).max(0.0) as u16
            })
            .collect();
        Image::new(width, height, samples)
    }

    #[test]
    fn noise_line_from_two_frames() {
        let (per_code, floor) = (0.8, 9.0);
        let (first, second) = (noisy(512, 256, per_code, floor, 7), noisy(512, 256, per_code, floor, 99));
        let still = still(&first, crate::base::fft::SIDE);
        let noise = Noise::measure(&first, &second, &still);
        for light in [50.0, 300.0, 600.0] {
            let truth = per_code * light + floor;
            assert!(
                (noise.variance(light) / truth - 1.0).abs() < 0.1,
                "{noise:?}: at {light} codes {} for {truth}",
                noise.variance(light)
            );
        }
        let same = Noise::measure(&first, &first, &still);
        assert_eq!(
            same,
            Noise { per_code: 0.0, floor: ROUNDING },
            "one frame laid on itself has no noise but rounding"
        );
        let mut few = vec![(10.0, 4.0)];
        assert_eq!(Noise::fit(&mut few), Noise { per_code: 0.0, floor: ROUNDING });
    }
}
