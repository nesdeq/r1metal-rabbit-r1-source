//! Photos: the sensor's modes for them, and a burst's merged square developed at its own size into a JPEG encoder's luma and chroma planes.

use crate::base::crew::Part;
use crate::base::math::sqrt;
use crate::base::sampling::Turn;
use crate::base::sampling::reflect_101;
use crate::base::ycbcr::jfif_chroma;
use crate::base::ycbcr::jfif_luma;
use crate::hw::camera::BLUE;
use crate::hw::camera::FRACTION;
use crate::hw::camera::GREEN;
use crate::hw::camera::LIGHT_STEPS;
use crate::hw::camera::RED;
use crate::hw::camera::UNITY;
use crate::hw::camera::burst::Merged;
use crate::hw::camera::exposure::TARGET;
use crate::hw::camera::frame::WHITE;
use crate::hw::camera::fusion::GainMap;
use crate::hw::camera::merge::FINE_BITS;
use crate::hw::camera::mode::Mode;
use crate::hw::camera::mosaic::Planes;
use crate::hw::camera::raw::Square;
use crate::hw::camera::shading::Axis;
use crate::hw::camera::shading::Correction;
use crate::hw::camera::shading::Lens;
use crate::hw::camera::shading::Span;
use crate::hw::camera::shading::lift_code;
use crate::hw::camera::tone::Recipe;
use crate::hw::camera::tone::tone_curve;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

// The sensor's mode for the photo viewfinder and the key's window.
pub(crate) const VIEW_MODE: Mode = Mode::Preview;
/// The sensor's mode for a photo.
pub const MODE: Mode = Mode::Capture;
/// Pixels along a side of a photo: the capture frame's middle square at the sensor's own size.
pub const SIDE: usize = Square::middle_of(MODE.frame()).side as usize;
/// The error text where `Developer::new` makes none.
pub const NO_DEVELOPER: &str = "no developer for this burst";
const LIGHT_TOP: i32 = LIGHT_STEPS as i32 - 1;
const REACH: i32 = 2;
const RAW_LINES: usize = 2 * REACH as usize + 1;
const COLOUR_BITS: u32 = 8;
// ISO 12232's noise-based speeds: a signal-to-noise ratio of 40 at mid-grey makes an excellent picture, of 10 an acceptable one.
const SNR_EXCELLENT: f32 = 40.0;
const SNR_ACCEPTABLE: f32 = 10.0;

fn walk(view: Square, turn: Turn) -> ((i32, i32), (i32, i32), (i32, i32)) {
    let side = view.side as usize;
    let ((x, y), u_step, v_step) = turn.walk_back(side, side);
    let to_i32 = |(x, y): (isize, isize)| (x as i32, y as i32);
    ((x as i32, y as i32), to_i32(u_step), to_i32(v_step))
}

#[derive(Clone, Copy)]
struct Turned<'a> {
    planes: &'a Planes,
    side: i32,
    origin: (i32, i32),
    u_step: (i32, i32),
    v_step: (i32, i32),
}

impl Turned<'_> {
    fn place(&self, u: i32, v: i32) -> (usize, usize) {
        let mirror = |at: i32| reflect_101(at as isize, self.side as usize);
        let x = self.origin.0 + u * self.u_step.0 + v * self.v_step.0;
        let y = self.origin.1 + u * self.u_step.1 + v * self.v_step.1;
        (mirror(x), mirror(y))
    }

    fn colour(&self, u: i32, v: i32) -> usize {
        match self.place(u, v) {
            (x, y) if (x + y).is_multiple_of(2) => GREEN,
            (_, y) if y.is_multiple_of(2) => BLUE,
            _ => RED,
        }
    }

    fn unpack(&self, v: i32, line: &mut [u16]) {
        for (index, sample) in line.iter_mut().enumerate() {
            let (x, y) = self.place(index as i32 - REACH, v);
            *sample = self.planes.at(x, y);
        }
    }
}

// How far the colour matrix goes, in 1024ths: all of the way where the merge's mid-grey is clean, none where it is barely acceptable.
fn strength(merged: &Merged, recipe: &Recipe, under: i32) -> i32 {
    let grey =
        TARGET as f32 * UNITY as f32 / recipe.gains[GREEN].max(1) as f32 * UNITY as f32 / under.max(UNITY) as f32;
    let deviation = sqrt(merged.noise.variance(grey) / merged.frames.max(1) as f32);
    let ratio = if deviation > 0.0 { grey / deviation } else { SNR_EXCELLENT };
    (((ratio - SNR_ACCEPTABLE) / (SNR_EXCELLENT - SNR_ACCEPTABLE)).clamp(0.0, 1.0) * UNITY as f32) as i32
}

fn toward(full: i32, plain: i32, share: i32) -> i32 {
    plain + (((full - plain) * share) >> FRACTION)
}

fn correction(lens: Lens, view: Square, turn: Turn) -> Correction {
    let (origin, u_step, v_step) = walk(view, turn);
    let area = lens.area;
    let (axis, first, is_backward) = if u_step.0 != 0 {
        (Axis::Horizontal, area.left + area.scale * view.left, u_step.0 < 0)
    } else {
        (Axis::Vertical, area.top + area.scale * view.top, u_step.1 < 0)
    };
    let span = Span::evenly(axis, first, area.scale * view.side, view.side as usize, is_backward);
    let lines = (0..view.side as i32)
        .map(|v| match axis {
            Axis::Horizontal => area.top + area.scale * (view.top + (origin.1 + v * v_step.1) as u32),
            Axis::Vertical => area.left + area.scale * (view.left + (origin.0 + v * v_step.0) as u32),
        })
        .collect();
    Correction { shading: lens.shading, axis, span, lines }
}

/// Develops a burst's merged square into a photo of the same size.
pub struct Developer {
    side: usize,
    turn: Turn,
    correction: Option<Correction>,
    // A lens's correction where the merge is clean, half of it where it is not.
    lens_share: i32,
    light: [Vec<u16>; 3],
    matrix: Option<[[i32; 3]; 3]>,
    curve: [u8; LIGHT_STEPS],
    gains: Option<GainMap>,
}

impl Developer {
    /// A developer of `merged`, turned by `turn`, by a recipe, taken `under` (in 1024ths) below the light the recipe asks for, making up for a lens's shading over the square of the frame it names, its tone map made in parts `run` may run at once; none where the square is empty.
    pub fn new(
        merged: &Merged,
        turn: Turn,
        recipe: Recipe,
        under: i32,
        lens: Option<(Lens, Square)>,
        mut run: impl FnMut(Vec<Part<'_>>),
    ) -> Option<Developer> {
        let side = 2 * merged.planes.side();
        if side == 0 {
            return None;
        }
        let share = strength(merged, &recipe, under);
        let matrix = recipe.mix_matrix().map(|matrix| {
            core::array::from_fn(|row| {
                core::array::from_fn(|column| toward(matrix[row][column], if row == column { UNITY } else { 0 }, share))
            })
        });
        Some(Developer {
            side,
            turn,
            correction: lens.map(|(lens, view)| correction(lens, view, turn)),
            lens_share: (UNITY + share) / 2,
            light: recipe.fine_light_tables(LIGHT_TOP, FINE_BITS),
            matrix,
            curve: tone_curve(),
            gains: GainMap::new(merged, &recipe, under, lens, &mut run),
        })
    }

    fn fill_in(
        &self,
        turned: &Turned<'_>,
        v: i32,
        rows: [&[u16]; RAW_LINES],
        gains: &[Vec<u16>; 3],
        line: &mut [[u16; 3]],
    ) {
        let [far_up, up, row, below, far_below] = rows;
        let colours = [(turned.colour(0, v), turned.colour(1, v)), (turned.colour(1, v), turned.colour(0, v))];
        let top = (WHITE << FINE_BITS) as i32;
        for (u, pixel) in line.iter_mut().enumerate() {
            let at = u + REACH as usize;
            let (own, neighbour) = colours[u % 2];
            let centre = i32::from(row[at]);
            let sides = i32::from(row[at - 1]) + i32::from(row[at + 1]);
            let ends = i32::from(up[at]) + i32::from(below[at]);
            let corners =
                i32::from(up[at - 1]) + i32::from(up[at + 1]) + i32::from(below[at - 1]) + i32::from(below[at + 1]);
            let far_sides = i32::from(row[at - 2]) + i32::from(row[at + 2]);
            let far_ends = i32::from(far_up[at]) + i32::from(far_below[at]);
            let mut codes = [0i32; 3];
            if own == GREEN {
                let row_code = (10 * centre + 8 * sides - 2 * corners - 2 * far_sides + far_ends) / 16;
                let column_code = (10 * centre + 8 * ends - 2 * corners - 2 * far_ends + far_sides) / 16;
                codes[GREEN] = centre;
                codes[neighbour] = row_code;
                codes[RED + BLUE - neighbour] = column_code;
            } else {
                codes[own] = centre;
                codes[GREEN] = (4 * centre + 2 * (sides + ends) - far_sides - far_ends) / 8;
                codes[RED + BLUE - own] = (12 * centre + 4 * corners - 3 * (far_sides + far_ends)) / 16;
            }
            *pixel = core::array::from_fn(|colour| {
                let code = lift_code(codes[colour].clamp(0, top) as u32, gains[colour][u], FINE_BITS);
                self.light[colour][code as usize]
            });
        }
    }

    /// Develops the photo's lines `lines` (even ends) of the merge into `luma` and `chroma`.
    pub fn develop(&self, merged: &Merged, lines: Range<usize>, luma: &mut [u8], chroma: &mut [u8]) {
        let side = self.side;
        assert!(
            lines.start <= lines.end
                && lines.end <= side
                && lines.start.is_multiple_of(2)
                && lines.end.is_multiple_of(2),
            "lines {lines:?} of a photo of {side}"
        );
        assert!(luma.len() >= lines.len() * side && chroma.len() >= lines.len() / 2 * side, "planes too short");
        assert_eq!(2 * merged.planes.side(), side, "a merge of the developer's size");
        let square = Square { left: 0, top: 0, side: side as u32 };
        let (origin, u_step, v_step) = walk(square, self.turn);
        let turned = Turned { planes: &merged.planes, side: side as i32, origin, u_step, v_step };
        let mut raw_lines = vec![vec![0u16; side + 2 * REACH as usize]; RAW_LINES];
        let mut raw_numbers: [Option<i32>; RAW_LINES] = [None; RAW_LINES];
        let mut colour_line = vec![[0u16; 3]; side];
        let mut colours = vec![[0u8; 3]; 2 * side];
        // The gains of one line, for red, green and blue: one everywhere unless a lens's shading is made up for.
        let mut gains: [Vec<u16>; 3] = core::array::from_fn(|_| vec![UNITY as u16; side]);
        for (pair, first) in lines.step_by(2).enumerate() {
            for line in 0..2 {
                let v = (first + line) as i32;
                for number in v - REACH..=v + REACH {
                    let slot = number.rem_euclid(RAW_LINES as i32) as usize;
                    if raw_numbers[slot] != Some(number) {
                        turned.unpack(number, &mut raw_lines[slot]);
                        raw_numbers[slot] = Some(number);
                    }
                }
                let rows: [&[u16]; RAW_LINES] = core::array::from_fn(|index| {
                    &raw_lines[(v - REACH + index as i32).rem_euclid(RAW_LINES as i32) as usize][..]
                });
                if let Some(correction) = &self.correction {
                    let profile = correction.shading.profile(correction.axis, correction.lines[v as usize]);
                    for (gains, points) in gains.iter_mut().zip(&profile) {
                        let points = points.map(|point| toward(point, UNITY, self.lens_share));
                        correction.span.fill(&points, 0, gains);
                    }
                }
                self.fill_in(&turned, v, rows, &gains, &mut colour_line);
                for (u, colour) in colours[line * side..(line + 1) * side].iter_mut().enumerate() {
                    let mut lights = colour_line[u].map(i32::from);
                    if let Some(map) = &self.gains {
                        let (x, y) = turned.place(u as i32, v);
                        let gain = map.gain(x, y);
                        lights = lights.map(|light| ((light * gain) >> FRACTION).min(LIGHT_TOP));
                    }
                    let lights = match &self.matrix {
                        Some(matrix) => matrix.map(|row| {
                            ((row[0] * lights[0] + row[1] * lights[1] + row[2] * lights[2]) >> FRACTION)
                                .clamp(0, LIGHT_TOP)
                        }),
                        None => lights,
                    };
                    *colour = lights.map(|light| self.curve[light as usize]);
                }
            }
            for (line, row) in colours.chunks(side).enumerate() {
                let out = &mut luma[(2 * pair + line) * side..(2 * pair + line + 1) * side];
                for (byte, colour) in out.iter_mut().zip(row) {
                    *byte = jfif_luma::<COLOUR_BITS>(colour.map(i32::from));
                }
            }
            let (upper, lower) = colours.split_at(side);
            let out = &mut chroma[pair * side..(pair + 1) * side];
            for (x, bytes) in out.chunks_mut(2).enumerate() {
                let quad = [upper[2 * x], upper[2 * x + 1], lower[2 * x], lower[2 * x + 1]];
                bytes.copy_from_slice(&jfif_chroma::<COLOUR_BITS>(quad.map(|pixel| pixel.map(i32::from))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::crew::in_order;
    use crate::hw::camera::burst::fake::merged;
    use crate::hw::camera::colour::DAYLIGHT;
    use crate::hw::camera::colour::gain;
    use crate::hw::camera::colour::matrix;
    use crate::hw::camera::frame::BLACK;
    use crate::hw::camera::frame::Frame;
    use crate::hw::camera::frame::fake::flat;
    use crate::hw::camera::frame::fake::packed;
    use crate::hw::camera::shading::SensorArea;
    use crate::hw::camera::shading::fake;

    const UPRIGHT: Turn = Turn { quarters: 0, is_mirrored: false };
    const FRAME: Frame = Frame::whole(64, 40);

    fn develop_whole(developer: &Developer, merge: &Merged) -> (Vec<u8>, Vec<u8>) {
        let side = developer.side;
        let (mut luma, mut chroma) = (vec![0u8; side * side], vec![0u8; side * side / 2]);
        developer.develop(merge, 0..side, &mut luma, &mut chroma);
        (luma, chroma)
    }

    fn photo(turn: Turn, recipe: Recipe, raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let merge = merged(raw, FRAME);
        develop_whole(&Developer::new(&merge, turn, recipe, UNITY, None, in_order).unwrap(), &merge)
    }

    fn flat_photo(recipe: Recipe, colours: [u32; 3]) -> (Vec<u8>, Vec<u8>) {
        photo(UPRIGHT, recipe, &flat(FRAME, colours))
    }

    #[test]
    fn native_side() {
        assert_eq!(SIDE, 2448, "the capture frame's middle square, pixel for pixel");
        let merge = merged(&flat(FRAME, [BLACK; 3]), FRAME);
        assert_eq!(
            Developer::new(&merge, UPRIGHT, Recipe::PLAIN, UNITY, None, in_order).map(|developer| developer.side),
            Some(40)
        );
    }

    #[test]
    fn grey_frame() {
        let (luma, chroma) = flat_photo(Recipe::PLAIN, [BLACK + 173; 3]);
        assert!(
            luma.iter().all(|&byte| byte == 118),
            "18 % grey, as far as {:?}",
            luma.iter().min_by_key(|&&byte| byte.abs_diff(118))
        );
        assert!(chroma.iter().all(|&byte| byte == 128));
        let ((white, _), (black, _)) = (flat_photo(Recipe::PLAIN, [WHITE; 3]), flat_photo(Recipe::PLAIN, [BLACK; 3]));
        assert!(white.iter().all(|&byte| byte == 255) && black.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn jpeg_colours() {
        let cases = [
            ([WHITE, BLACK, BLACK], [76, 85, 255]),
            ([BLACK, WHITE, BLACK], [149, 43, 21]),
            ([BLACK, BLACK, WHITE], [29, 255, 107]),
        ];
        for (raw, [y, cb, cr]) in cases {
            let (luma, chroma) = flat_photo(Recipe::PLAIN, raw);
            assert!(luma.iter().all(|&byte| byte.abs_diff(y) <= 1), "luma of {raw:?}: {}", luma[0]);
            assert!(
                chroma.chunks(2).all(|pair| pair[0].abs_diff(cb) <= 1 && pair[1].abs_diff(cr) <= 1),
                "chroma of {raw:?}: {:?}",
                &chroma[..2]
            );
        }
    }

    #[test]
    fn balance_and_matrix() {
        let warm = Recipe { gains: [2 * UNITY, UNITY, UNITY / 2], ..Recipe::PLAIN };
        let (luma, chroma) = flat_photo(warm, [BLACK + 200, BLACK + 400, BLACK + 800]);
        assert!(chroma.iter().all(|&byte| byte.abs_diff(128) <= 1), "{:?}", &chroma[..2]);
        assert!(luma.iter().all(|&byte| byte == luma[0]));
        let swapped = Recipe { matrix: [[0, 0, UNITY], [0, UNITY, 0], [UNITY, 0, 0]], ..Recipe::PLAIN };
        let (_, chroma) = flat_photo(swapped, [WHITE, BLACK, BLACK]);
        assert!(
            chroma.chunks(2).all(|pair| pair[0].abs_diff(255) <= 1 && pair[1].abs_diff(107) <= 1),
            "red read as blue: {:?}",
            &chroma[..2]
        );
    }

    #[test]
    fn veil_and_tuned_matrix() {
        let (luma, chroma) = flat_photo(Recipe { veil: UNITY / 16, ..Recipe::PLAIN }, [BLACK + 59; 3]);
        assert!(
            luma.iter().all(|&byte| byte == 0) && chroma.iter().all(|&byte| byte == 128),
            "what lies under the veil is black"
        );
        let (plain, _) = flat_photo(Recipe::PLAIN, [BLACK + 59; 3]);
        assert!(plain.iter().all(|&byte| byte > 40), "and grey without it: {}", plain[0]);
        let daylight = [gain(DAYLIGHT.red), UNITY, gain(DAYLIGHT.blue)];
        let tuned = Recipe { matrix: matrix(daylight), ..Recipe::PLAIN };
        let (luma, chroma) = flat_photo(tuned, [BLACK + 173; 3]);
        assert!(
            luma.iter().all(|&byte| byte.abs_diff(118) <= 1) && chroma.iter().all(|&byte| byte.abs_diff(128) <= 1),
            "grey stays grey: {} {:?}",
            luma[0],
            &chroma[..2]
        );
        let reddish = [BLACK + 400, BLACK + 250, BLACK + 200];
        let ((_, dull), (_, vivid)) = (flat_photo(Recipe::PLAIN, reddish), flat_photo(tuned, reddish));
        assert!(vivid[1] > dull[1] + 15, "a reddish thing is redder: chroma {} against {}", vivid[1], dull[1]);
    }

    #[test]
    fn noise_dulls_colour() {
        let daylight = [gain(DAYLIGHT.red), UNITY, gain(DAYLIGHT.blue)];
        let tuned = Recipe { matrix: matrix(daylight), ..Recipe::PLAIN };
        let mut merge = merged(&flat(FRAME, [BLACK; 3]), FRAME);
        assert_eq!(strength(&merge, &tuned, UNITY), UNITY, "a merge without noise takes all of the matrix");
        merge.noise = crate::hw::camera::noise::fake::with(0.0, (TARGET * TARGET) as f32 / 400.0);
        assert_eq!(
            strength(&merge, &tuned, UNITY),
            341,
            "mid-grey 20 times its noise: a third of the way past acceptable"
        );
        merge.frames = 4;
        assert_eq!(strength(&merge, &tuned, UNITY), UNITY, "four frames halve the noise: excellent");
        merge.frames = 1;
        merge.noise = crate::hw::camera::noise::fake::with(0.0, (TARGET * TARGET) as f32 / 25.0);
        let developer = Developer::new(&merge, UPRIGHT, tuned, UNITY, None, in_order).unwrap();
        assert_eq!(
            (developer.matrix, developer.lens_share),
            (Some([[UNITY, 0, 0], [0, UNITY, 0], [0, 0, UNITY]]), UNITY / 2)
        );
    }

    fn quarter_means(turn: Turn) -> [u32; 4] {
        let view = Square::middle_of(FRAME);
        let bytes = packed(FRAME, |x, y| {
            match (x.wrapping_sub(view.left) < view.side / 2, x.wrapping_sub(view.left) < view.side, y < view.side / 2)
            {
                (true, _, true) => WHITE,
                (false, true, true) => BLACK + 173,
                _ => BLACK,
            }
        });
        let (luma, _) = photo(turn, Recipe::PLAIN, &bytes);
        let luma = &luma;
        let mean = |left: usize, top: usize| {
            (top + 5..top + 15)
                .flat_map(|y| (left + 5..left + 15).map(move |x| u32::from(luma[y * 40 + x])))
                .sum::<u32>()
                / 100
        };
        [mean(0, 0), mean(20, 0), mean(0, 20), mean(20, 20)]
    }

    #[test]
    fn photo_turns() {
        const BRIGHT: u32 = 255;
        const HALF: u32 = 118;
        let means = |quarters: u8, is_mirrored: bool| quarter_means(Turn { quarters, is_mirrored });
        assert_eq!(means(0, false), [BRIGHT, HALF, 0, 0], "top left, top right, bottom left, bottom right");
        assert_eq!(means(1, false), [0, BRIGHT, 0, HALF], "a quarter turn clockwise");
        assert_eq!(means(2, false), [0, 0, HALF, BRIGHT]);
        assert_eq!(means(3, false), [HALF, 0, BRIGHT, 0]);
        assert_eq!(means(0, true), [HALF, BRIGHT, 0, 0], "mirrored left to right");
        assert_eq!(means(1, true), [BRIGHT, 0, HALF, 0]);
        assert_eq!(means(2, true), [0, 0, BRIGHT, HALF]);
        assert_eq!(means(3, true), [0, HALF, 0, BRIGHT]);
    }

    #[test]
    fn lens_lift() {
        let area = SensorArea { left: 0, top: 0, scale: 51 };
        let view = Square::middle_of(FRAME);
        let shot = |lens: Lens, turn: Turn, colours: [u32; 3]| {
            let merge = merged(&flat(FRAME, colours), FRAME);
            let lens = Some((Lens { area, ..lens }, view));
            let developer = Developer::new(&merge, turn, Recipe::PLAIN, UNITY, lens, in_order).unwrap();
            develop_whole(&developer, &merge)
        };
        assert_eq!(
            shot(fake::lens(|_, _, _| 2048), UPRIGHT, [BLACK + 100, BLACK + 150, BLACK + 200]),
            flat_photo(Recipe::PLAIN, [BLACK + 200, BLACK + 300, BLACK + 400]),
            "a lens that halves all light everywhere: every pixel is doubled"
        );
        let rising = || fake::lens(|_, _, column| 1024 + 64 * column as u16);
        let (luma, chroma) = shot(rising(), UPRIGHT, [BLACK + 200; 3]);
        let at = |luma: &[u8], x: usize, y: usize| luma[y * 40 + x];
        assert!(at(&luma, 35, 20) > at(&luma, 5, 20) + 20, "gains that rise along the sensor's rows");
        assert!(at(&luma, 20, 5).abs_diff(at(&luma, 20, 35)) <= 1 && chroma.iter().all(|&byte| byte == 128));
        let (turned, _) = shot(rising(), Turn { quarters: 1, is_mirrored: false }, [BLACK + 200; 3]);
        assert!(
            at(&turned, 20, 35) > at(&turned, 20, 5) + 20 && at(&turned, 5, 20).abs_diff(at(&turned, 35, 20)) <= 1,
            "a quarter turn clockwise: the sensor's rows run up the photo, and the gains rise down it"
        );
        let (mirrored, _) = shot(rising(), Turn { quarters: 0, is_mirrored: true }, [BLACK + 200; 3]);
        assert_eq!(at(&mirrored, 5, 20), at(&luma, 34, 20), "mirrored: the photo's left is the sensor's right");
        let (red, tint) = shot(fake::lens(|colour, _, _| [2048, 1024, 1024][colour]), UPRIGHT, [BLACK + 200; 3]);
        assert!(tint[1] > 150 && red[0] > at(&luma, 0, 0), "red alone lifted: the photo turns red");
    }

    #[test]
    fn parts_equal_whole() {
        let bytes = packed(FRAME, |x, y| (37 * x + 101 * y + x * y) % 1024);
        let merge = merged(&bytes, FRAME);
        let area = SensorArea { left: 0, top: 0, scale: 51 };
        let uneven = Lens { area, ..fake::lens(|colour, row, column| (1024 + 40 * (colour + row + column)) as u16) };
        let below = (4 * UNITY, Some((uneven, Square::middle_of(FRAME))));
        for (quarters, (under, lens)) in (0..4).flat_map(|quarters| [(quarters, (UNITY, None)), (quarters, below)]) {
            let recipe = Recipe { gains: [1500, UNITY, 1900], veil: 20, matrix: matrix([1500, UNITY, 1900]) };
            let turn = Turn { quarters, is_mirrored: quarters % 2 == 1 };
            let developer = Developer::new(&merge, turn, recipe, under, lens, in_order).unwrap();
            assert_eq!(developer.gains.is_some(), under > UNITY, "a tone map for a burst taken below its light alone");
            let (luma, chroma) = develop_whole(&developer, &merge);
            let (mut luma_parts, mut chroma_parts) = (vec![0u8; 1600], vec![0u8; 800]);
            for (index, (luma_part, chroma_part)) in
                luma_parts.chunks_mut(12 * 40).zip(chroma_parts.chunks_mut(6 * 40)).enumerate()
            {
                developer.develop(&merge, 12 * index..12 * index + luma_part.len() / 40, luma_part, chroma_part);
            }
            assert!(luma_parts == luma && chroma_parts == chroma, "{quarters} quarter turns, taken {under} below");
            assert!(luma.iter().any(|&byte| byte != luma[0]));
        }
    }

    #[test]
    fn shadows_lifted_below_light() {
        let bytes = packed(FRAME, |x, _| if x < 32 { BLACK + 30 } else { WHITE - 40 });
        let merge = merged(&bytes, FRAME);
        let shot = |under: i32| {
            develop_whole(&Developer::new(&merge, UPRIGHT, Recipe::PLAIN, under, None, in_order).unwrap(), &merge).0
        };
        let (plain, lifted) = (shot(UNITY), shot(4 * UNITY));
        let at = |luma: &[u8], x: usize| luma[20 * 40 + x];
        assert!(
            at(&lifted, 5) > at(&plain, 5),
            "the dark half of a burst taken below its light is lifted: {} for {}",
            at(&lifted, 5),
            at(&plain, 5)
        );
        assert!(
            at(&lifted, 35) >= at(&plain, 35),
            "and its bright half no darker: {} for {}",
            at(&lifted, 35),
            at(&plain, 35)
        );
    }

    #[test]
    fn sharp_edge() {
        let bytes = packed(FRAME, |x, _| if x < 32 { BLACK + 20 } else { WHITE - 20 });
        let (luma, _) = photo(UPRIGHT, Recipe::PLAIN, &bytes);
        let row = &luma[20 * 40..21 * 40];
        let (dark, bright) = (row[5], row[35]);
        assert!(dark < 50 && bright > 245, "{dark} and {bright}");
        assert!(
            row[..18].iter().all(|&byte| byte == dark) && row[22..].iter().all(|&byte| byte == bright),
            "flat again two pixels from the edge, as far as the demosaic reaches: {row:?}"
        );
    }
}
