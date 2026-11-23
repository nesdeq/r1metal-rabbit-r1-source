//! From the sensor's raw codes to the picture's: a recipe's gains, veil and matrix, and the sRGB curve with more contrast about grey.

use crate::base::pixel::PixelFormat;
use crate::hw::camera::BLUE;
use crate::hw::camera::FRACTION;
use crate::hw::camera::GREEN;
use crate::hw::camera::RED;
use crate::hw::camera::UNITY;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::RAW_CODES;
use crate::hw::camera::frame::WHITE;
use alloc::vec::Vec;

pub(super) const RUN: usize = 256;

pub(super) struct Scratch {
    light: [[i32; RUN]; 3],
    mix: [[u16; RUN]; 3],
}

impl Scratch {
    pub(super) const fn new() -> Scratch {
        Scratch { light: [[0; RUN]; 3], mix: [[0; RUN]; 3] }
    }
}

const CURVE_STEPS: usize = 1024;
const TOP: i32 = CURVE_STEPS as i32 - 1;
const LAST_CODE: usize = u8::MAX as usize;
const SRGB_SLOPE: f64 = 12.92;
const SRGB_KNEE: f64 = 0.04045;
const SRGB_OFFSET: f64 = 0.055;
const SRGB_SCALE: f64 = 1.055;
const SRGB_EXPONENT: f64 = 2.4;
const ROOT_STEPS: u32 = 48;
const CONTRAST_PIVOT: f64 = 118.0 / 255.0;
const CONTRAST_SLOPE: f64 = 1.25;
const SPREAD_BITS: u32 = 16;
const VEIL_LIMIT: i32 = UNITY / 2;

fn inverse(curve: impl Fn(f64) -> f64, target: f64) -> f64 {
    let (mut low, mut high) = (0.0f64, 1.0f64);
    for _ in 0..ROOT_STEPS {
        let middle = (low + high) / 2.0;
        if curve(middle) < target { low = middle } else { high = middle }
    }
    (low + high) / 2.0
}

pub(super) fn srgb_to_light(code: f64) -> f64 {
    if code <= SRGB_KNEE {
        return code / SRGB_SLOPE;
    }
    crate::base::math::power_f64((code + SRGB_OFFSET) / SRGB_SCALE, SRGB_EXPONENT)
}

pub(super) fn light_to_srgb(light: f64) -> f64 {
    if light <= SRGB_KNEE / SRGB_SLOPE {
        return light * SRGB_SLOPE;
    }
    SRGB_SCALE * crate::base::math::power_f64(light, 1.0 / SRGB_EXPONENT) - SRGB_OFFSET
}

fn add_contrast(share: f64) -> f64 {
    let bend = (CONTRAST_SLOPE - 1.0) / (CONTRAST_PIVOT * (1.0 - CONTRAST_PIVOT));
    share + bend * share * (share - CONTRAST_PIVOT) * (1.0 - share)
}

fn remove_contrast(share: f64) -> f64 {
    inverse(add_contrast, share)
}

pub(super) fn tone_curve<const STEPS: usize>() -> [u8; STEPS] {
    let mut curve = [0u8; STEPS];
    let mut step = 0;
    for code in 1..=LAST_CODE {
        let start = srgb_to_light(remove_contrast((code as f64 - 0.5) / LAST_CODE as f64)) * (STEPS - 1) as f64;
        while step < STEPS && (step as f64) < start {
            curve[step] = (code - 1) as u8;
            step += 1;
        }
    }
    curve[step..].fill(LAST_CODE as u8);
    curve
}

const PLAIN: [[i32; 3]; 3] = [[UNITY, 0, 0], [0, UNITY, 0], [0, 0, UNITY]];

/// What a frame is developed with: its gains, the stray light taken off, and the matrix into the picture's colours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Recipe {
    // Of red, green and blue, in 1024ths.
    pub(crate) gains: [i32; 3],
    pub(super) veil: i32,
    // The mix of red, green and blue for each colour of the picture, in 1024ths.
    pub(crate) matrix: [[i32; 3]; 3],
}

#[cfg(test)]
impl Recipe {
    pub(super) const PLAIN: Recipe = Recipe { gains: [UNITY; 3], veil: 0, matrix: PLAIN };
}

impl Recipe {
    fn light_tables(&self, top: i32) -> [[u16; RAW_CODES]; 3] {
        self.gains.map(|gain| {
            let curve = LightCurve::new(gain, self.veil, top, 0);
            core::array::from_fn(|raw| curve.light(raw))
        })
    }

    // The tables of codes with `bits` more bits than the sensor's, a merge's.
    pub(super) fn fine_light_tables(&self, top: i32, bits: u32) -> [Vec<u16>; 3] {
        self.gains.map(|gain| {
            let curve = LightCurve::new(gain, self.veil, top, bits);
            (0..=(WHITE << bits) as usize).map(|code| curve.light(code)).collect()
        })
    }

    pub(super) fn mix_matrix(&self) -> Option<[[i32; 3]; 3]> {
        (self.matrix != PLAIN).then_some(self.matrix)
    }
}

#[derive(Clone, Copy)]
struct LightCurve {
    stretch: i64,
    veil: i64,
    spread: i64,
    top: i64,
    bits: u32,
}

impl LightCurve {
    fn new(gain: i32, veil: i32, top: i32, bits: u32) -> LightCurve {
        let top = i64::from(top);
        let veil = (i64::from(veil.clamp(0, VEIL_LIMIT)) * top) >> FRACTION;
        LightCurve {
            stretch: i64::from(gain) * top / i64::from(WHITE - BLACK),
            veil,
            spread: (top << SPREAD_BITS) / (top - veil),
            top,
            bits,
        }
    }

    #[inline]
    fn light(&self, code: usize) -> u16 {
        let above = (code as i64 - (i64::from(BLACK) << self.bits)).max(0);
        let light = ((above * self.stretch) >> (FRACTION + self.bits)).min(self.top);
        (((light - self.veil).max(0) * self.spread) >> SPREAD_BITS).min(self.top) as u16
    }
}

/// What turns the sensor's colours into the screen's: a recipe's gains, veil and matrix, and the picture's curve.
pub struct Tone {
    pub(super) recipe: Recipe,
    curve: [u8; CURVE_STEPS],
    light: [[u16; RAW_CODES]; 3],
    pub(super) codes: [[u8; RAW_CODES]; 3],
}

impl Tone {
    pub fn new(recipe: Recipe) -> Tone {
        let mut tone = Tone { recipe, curve: tone_curve(), light: [[0; RAW_CODES]; 3], codes: [[0; RAW_CODES]; 3] };
        tone.set(recipe);
        tone
    }

    /// Takes a recipe where it is another than the one it follows; true where it was.
    pub fn update(&mut self, recipe: Recipe) -> bool {
        let is_new = recipe != self.recipe;
        if is_new {
            self.set(recipe);
        }
        is_new
    }

    fn set(&mut self, recipe: Recipe) {
        self.recipe = recipe;
        self.light = recipe.light_tables(TOP);
        for (table, light) in self.codes.iter_mut().zip(&self.light) {
            for (code, &light) in table.iter_mut().zip(light) {
                *code = self.curve[usize::from(light)];
            }
        }
    }

    #[cfg(test)]
    pub(super) fn colour(&self, raw: [u32; 3]) -> [u8; 3] {
        let [red, green, blue] = raw.map(|code| code.min(WHITE) as usize);
        let Some(matrix) = self.recipe.mix_matrix() else {
            return [self.codes[RED][red], self.codes[GREEN][green], self.codes[BLUE][blue]];
        };
        let (red, green, blue) =
            (i32::from(self.light[RED][red]), i32::from(self.light[GREEN][green]), i32::from(self.light[BLUE][blue]));
        let mix = |row: &[i32; 3]| ((row[0] * red + row[1] * green + row[2] * blue) >> FRACTION).clamp(0, TOP) as usize;
        [self.curve[mix(&matrix[0])], self.curve[mix(&matrix[1])], self.curve[mix(&matrix[2])]]
    }

    pub(super) fn words(
        &self,
        [reds, greens, blues]: [&[u16]; 3],
        format: PixelFormat,
        words: &mut [u32],
        scratch: &mut Scratch,
    ) {
        let count = words.len();
        let (reds, greens, blues) = (&reds[..count], &greens[..count], &blues[..count]);
        let (r_shift, g_shift, b_shift) = (format.r_shift, format.g_shift, format.b_shift);
        // A place in a table is cut to the table's length by its bits, so that no step asks whether the table reaches that far.
        let (code, step) =
            (|raw: u16| usize::from(raw) & (RAW_CODES - 1), |mix: u16| usize::from(mix) & (CURVE_STEPS - 1));
        let Some(matrix) = self.recipe.mix_matrix() else {
            for (((word, &red), &green), &blue) in words.iter_mut().zip(reds).zip(greens).zip(blues) {
                *word = u32::from(self.codes[RED][code(red)]) << r_shift
                    | u32::from(self.codes[GREEN][code(green)]) << g_shift
                    | u32::from(self.codes[BLUE][code(blue)]) << b_shift;
            }
            return;
        };
        let [red, green, blue] = scratch.light.each_mut().map(|light| &mut light[..count]);
        for ((light, table), codes) in
            [&mut *red, &mut *green, &mut *blue].into_iter().zip(&self.light).zip([reds, greens, blues])
        {
            for (light, &raw) in light.iter_mut().zip(codes) {
                *light = i32::from(table[code(raw)]);
            }
        }
        // The mixing is a plain run over the light of all quads, which the compiler turns into vector code.
        let [to_red, to_green, to_blue] = scratch.mix.each_mut().map(|mix| &mut mix[..count]);
        for (row, mix) in matrix.iter().zip([&mut *to_red, &mut *to_green, &mut *to_blue]) {
            for (((mix, &red), &green), &blue) in mix.iter_mut().zip(&*red).zip(&*green).zip(&*blue) {
                *mix = ((row[0] * red + row[1] * green + row[2] * blue) >> FRACTION).clamp(0, TOP) as u16;
            }
        }
        for (((word, &red), &green), &blue) in words.iter_mut().zip(&*to_red).zip(&*to_green).zip(&*to_blue) {
            *word = u32::from(self.curve[step(red)]) << r_shift
                | u32::from(self.curve[step(green)]) << g_shift
                | u32::from(self.curve[step(blue)]) << b_shift;
        }
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;

    pub(crate) fn plain() -> Tone {
        Tone::new(Recipe::PLAIN)
    }

    pub(crate) fn balanced(gains: [i32; 3]) -> Tone {
        Tone::new(Recipe { gains, ..Recipe::PLAIN })
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{balanced, plain};
    use super::*;
    use crate::hw::camera::colour::DAYLIGHT;
    use crate::hw::camera::colour::gain;
    use crate::hw::camera::colour::matrix;

    fn with_matrix(matrix: [[i32; 3]; 3]) -> Tone {
        Tone::new(Recipe { matrix, ..Recipe::PLAIN })
    }

    #[test]
    fn srgb_curve() {
        let curve = tone_curve::<CURVE_STEPS>();
        assert_eq!((curve[0], curve[1023]), (0, 255));
        assert_eq!(curve[184], 118, "18 % grey is where sRGB has it");
        assert_eq!(curve[512], 201, "half of full light: sRGB has 188");
        assert_eq!(curve[3], 6, "near black: sRGB has 10");
        assert_eq!(
            (curve[100], curve[767]),
            (81, 236),
            "darker under grey and brighter over it than sRGB's 89 and 225"
        );
        assert!(curve.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            (add_contrast(CONTRAST_PIVOT) - CONTRAST_PIVOT).abs() < 1e-12
                && add_contrast(0.0) == 0.0
                && (add_contrast(1.0) - 1.0).abs() < 1e-12
        );
        for share in [0.02, 0.3, CONTRAST_PIVOT, 0.8, 0.99] {
            assert!((add_contrast(remove_contrast(share)) - share).abs() < 1e-9, "share {share}");
        }
        for code in [0.0, 0.02, 0.04045, 0.3, 0.5, 1.0] {
            assert!((light_to_srgb(srgb_to_light(code)) - code).abs() < 1e-9, "sRGB's two ways at {code}");
        }
    }

    #[test]
    fn veil_removal() {
        let plain = LightCurve::new(UNITY, 0, TOP, 0);
        assert_eq!(
            (
                plain.light(0),
                plain.light(BLACK as usize),
                plain.light(BLACK as usize + 173),
                plain.light(WHITE as usize)
            ),
            (0, 0, 184, 1022)
        );
        let veiled = LightCurve::new(UNITY, UNITY / 16, TOP, 0);
        assert_eq!(veiled.light(BLACK as usize + 59), 0, "what lies under the veil is black");
        assert_eq!(veiled.light(WHITE as usize), 1021, "the sensor's limit stays at the top");
        assert_eq!(
            veiled.light(BLACK as usize + 480),
            477,
            "half of the range: 511 less the veil's 63, spread by 1023 over 960"
        );
        let lifted = LightCurve::new(2 * UNITY, UNITY / 16, TOP, 0);
        assert_eq!(
            lifted.light(BLACK as usize + 240),
            veiled.light(BLACK as usize + 480),
            "the veil comes off after the gain"
        );
        assert_eq!(
            LightCurve::new(UNITY, 4 * UNITY, TOP, 0).light(WHITE as usize),
            1021,
            "no veil takes more than half of the range"
        );
        let fine = LightCurve::new(UNITY, 0, TOP, 4);
        assert_eq!(
            (fine.light((BLACK as usize + 173) << 4), fine.light(((BLACK as usize + 173) << 4) + 8)),
            (plain.light(BLACK as usize + 173), 185),
            "a code in sixteenths, as a merge has it, lies where its whole code lies, and half a code more half a step on"
        );
        let tone = Tone::new(Recipe { veil: UNITY / 16, ..Recipe::PLAIN });
        assert_eq!(tone.colour([BLACK + 59, BLACK + 480, WHITE]), [0, tone.curve[477], 255]);
    }

    #[test]
    fn black_and_white_codes() {
        let tone = plain();
        assert_eq!(tone.colour([BLACK; 3]), [0; 3]);
        assert_eq!(tone.colour([0; 3]), [0; 3], "codes under black count as black");
        assert_eq!(tone.colour([WHITE; 3]), [255; 3]);
        assert_eq!(tone.colour([BLACK + 173, BLACK, WHITE]), [118, 0, 255]);
    }

    #[test]
    fn balance_and_matrix() {
        let warm = balanced([2 * UNITY, UNITY, UNITY / 2]);
        let grey = warm.colour([BLACK + 200, BLACK + 400, BLACK + 800]);
        assert!(grey[0].abs_diff(grey[1]) <= 1 && grey[1].abs_diff(grey[2]) <= 1, "{grey:?}");
        assert_eq!(warm.colour([WHITE, WHITE, WHITE]), [255, 255, 201], "a colour past the top stays at the top");
        let swapped = with_matrix([[0, 0, UNITY], [0, UNITY, 0], [UNITY, 0, 0]]);
        assert_eq!(swapped.colour([WHITE, BLACK, BLACK]), [0, 0, 255]);
        let vivid = with_matrix([[2 * UNITY, -UNITY, 0], [0, UNITY, 0], [0, -UNITY, 2 * UNITY]]);
        let green = plain().colour([BLACK, BLACK + 400, BLACK])[1];
        assert_eq!(
            vivid.colour([BLACK + 100, BLACK + 400, BLACK + 900]),
            [0, green, 255],
            "what the mix takes below black or past the top is cut off"
        );
        let mut tone = plain();
        assert!(!tone.update(Recipe::PLAIN), "the recipe it follows is no other");
        assert!(tone.update(Recipe { gains: [2 * UNITY, UNITY, UNITY / 2], ..Recipe::PLAIN }));
        assert_eq!(tone.colour([BLACK + 200, BLACK + 400, BLACK + 800]), grey, "a tone takes another recipe");
    }

    #[test]
    fn run_words() {
        let format = PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 };
        let daylight = [gain(DAYLIGHT.red), UNITY, gain(DAYLIGHT.blue)];
        let strong = [[4 * UNITY, -3 * UNITY, 0], [0, UNITY, 0], [0, -3 * UNITY, 4 * UNITY]];
        let codes: [[u16; RUN]; 3] = core::array::from_fn(|colour| {
            core::array::from_fn(|quad| match quad {
                0 => 0,
                1 => BLACK as u16,
                2 => WHITE as u16,
                _ => ((37 + 300 * colour) * quad % RAW_CODES) as u16,
            })
        });
        let veiled = Tone::new(Recipe { gains: daylight, veil: UNITY / 16, matrix: matrix(daylight) });
        for (tone, name) in
            [(plain(), "plain"), (balanced(daylight), "balanced"), (with_matrix(strong), "mixed"), (veiled, "veiled")]
        {
            for count in [RUN, 37, 1, 0] {
                let mut words = [0u32; RUN];
                let codes = codes.each_ref().map(|codes| &codes[..count]);
                tone.words(codes, format, &mut words[..count], &mut Scratch::new());
                for quad in 0..count {
                    let [r, g, b] = tone.colour(codes.map(|codes| u32::from(codes[quad])));
                    let word = u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16;
                    assert_eq!(words[quad], word, "quad {quad} of {count} by the {name} tone");
                }
                assert_eq!(words[count..], [0u32; RUN][count..], "and no word past the run");
            }
        }
    }

    #[test]
    fn sensor_matrix() {
        let daylight = [gain(DAYLIGHT.red), UNITY, gain(DAYLIGHT.blue)];
        let tone = with_matrix(matrix(daylight));
        for code in [BLACK + 20, BLACK + 173, BLACK + 600, WHITE] {
            let grey = tone.colour([code; 3]);
            assert!(grey[0].abs_diff(grey[1]) <= 1 && grey[1].abs_diff(grey[2]) <= 1, "raw {code}: {grey:?}");
            assert!(grey[1].abs_diff(plain().colour([code; 3])[1]) <= 1);
        }
        let spread = |colour: [u8; 3]| colour.iter().max().unwrap() - colour.iter().min().unwrap();
        let reddish = [BLACK + 400, BLACK + 250, BLACK + 200];
        assert_eq!(
            (tone.colour(reddish), plain().colour(reddish)),
            ([208, 134, 115], [185, 145, 128]),
            "a reddish thing is redder"
        );
        assert!(spread(tone.colour(reddish)) > spread(plain().colour(reddish)) + 30);
    }
}
