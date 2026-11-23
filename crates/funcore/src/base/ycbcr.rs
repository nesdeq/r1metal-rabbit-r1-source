use super::pixel::PixelFormat;
use super::simd;

pub(crate) const WEIGHTS_709: [i64; 3] = [2126, 7152, 722];
pub(crate) const WEIGHTS_601: [i64; 3] = [2990, 5870, 1140];
pub(crate) const WHOLE: i64 = 10_000;
pub(crate) const LUMA_FLOOR: i32 = 16;
pub(crate) const LUMA_SPAN: i64 = 219;
pub(crate) const CHROMA_MIDDLE: i32 = 128;
pub(crate) const CHROMA_SPAN: i64 = 224;
pub(crate) const FULL: i64 = 255;
pub(super) const COLOUR_BITS: u32 = 16;
const COLOUR_ONE: i64 = 1 << COLOUR_BITS;

pub(crate) const fn div_round(value: i64, divisor: i64) -> i64 {
    (2 * value + if value < 0 { -divisor } else { divisor }) / (2 * divisor)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Matrix {
    pub(crate) luma: [i32; 3],
    pub(crate) blue: [i32; 3],
    pub(crate) red: [i32; 3],
}

impl Matrix {
    pub(crate) const fn new(weights: [i64; 3], unit: i64, is_full_range: bool) -> Matrix {
        let (luma_span, chroma_span) = if is_full_range { (FULL, FULL) } else { (LUMA_SPAN, CHROMA_SPAN) };
        let (red, blue) = (weights[0], weights[2]);
        let mut matrix = Matrix { luma: [0; 3], blue: [0; 3], red: [0; 3] };
        let mut colour = 0;
        while colour < 3 {
            let weight = weights[colour];
            let (own_blue, own_red) = (if colour == 2 { WHOLE } else { 0 }, if colour == 0 { WHOLE } else { 0 });
            matrix.luma[colour] = div_round(weight * unit * luma_span, WHOLE * FULL) as i32;
            matrix.blue[colour] = div_round((own_blue - weight) * unit * chroma_span, 2 * (WHOLE - blue) * FULL) as i32;
            matrix.red[colour] = div_round((own_red - weight) * unit * chroma_span, 2 * (WHOLE - red) * FULL) as i32;
            colour += 1;
        }
        matrix
    }
}

#[inline(always)]
pub(crate) fn jfif_luma<const BITS: u32>(colour: [i32; 3]) -> u8 {
    ((weigh(const { Matrix::new(WEIGHTS_601, 1 << BITS, true) }.luma, colour) + (1 << (BITS - 1))) >> BITS) as u8
}

#[inline(always)]
pub(crate) fn jfif_chroma<const BITS: u32>(four: [[i32; 3]; 4]) -> [u8; 2] {
    let matrix = const { Matrix::new(WEIGHTS_601, 1 << BITS, true) };
    let sum = core::array::from_fn(|colour| four.iter().map(|pixel| pixel[colour]).sum());
    let tint = |weights| {
        (CHROMA_MIDDLE + ((weigh(weights, sum) + (1 << (BITS + 1))) >> (BITS + 2))).clamp(0, FULL as i32) as u8
    };
    [tint(matrix.blue), tint(matrix.red)]
}

#[inline(always)]
fn weigh(weights: [i32; 3], [red, green, blue]: [i32; 3]) -> i32 {
    weights[0] * red + weights[1] * green + weights[2] * blue
}

pub(crate) struct Colours {
    pub(super) floor: i32,
    pub(super) luma: i32,
    pub(super) red: i32,
    pub(super) green_by_blue: i32,
    pub(super) green_by_red: i32,
    pub(super) blue: i32,
}

// Where a row's blue and red samples lie: one of each for every pixel, in rows of their own or side by side, or side by side for every two pixels.
#[derive(Clone, Copy)]
pub(crate) enum Chroma<'a> {
    Apart(&'a [u8], &'a [u8]),
    Beside(&'a [u8]),
    Shared(&'a [u8]),
}

impl Colours {
    pub(crate) const fn new(weights: [i64; 3], is_full_range: bool) -> Colours {
        let [red, green, blue] = weights;
        let (floor, luma_span, chroma_span) =
            if is_full_range { (0, FULL, FULL) } else { (LUMA_FLOOR, LUMA_SPAN, CHROMA_SPAN) };
        let scale = FULL * COLOUR_ONE;
        Colours {
            floor,
            luma: div_round(scale, luma_span) as i32,
            red: div_round(2 * scale * (WHOLE - red), WHOLE * chroma_span) as i32,
            green_by_blue: div_round(2 * scale * blue * (WHOLE - blue), green * WHOLE * chroma_span) as i32,
            green_by_red: div_round(2 * scale * red * (WHOLE - red), green * WHOLE * chroma_span) as i32,
            blue: div_round(2 * scale * (WHOLE - blue), WHOLE * chroma_span) as i32,
        }
    }

    #[inline(always)]
    pub(crate) fn word(&self, luma: i32, blue: i32, red: i32, format: PixelFormat) -> u32 {
        let code = |sum: i32| (sum >> COLOUR_BITS).clamp(0, FULL as i32) as u32;
        let light = (luma - self.floor) * self.luma + (COLOUR_ONE / 2) as i32;
        let (blue, red) = (blue - CHROMA_MIDDLE, red - CHROMA_MIDDLE);
        code(light + red * self.red) << format.r_shift
            | code(light - blue * self.green_by_blue - red * self.green_by_red) << format.g_shift
            | code(light + blue * self.blue) << format.b_shift
    }

    // The words of as many pixels as `out` holds, each of its light and its blue and red where `chroma` lays them.
    pub(crate) fn row(&self, luma: &[u8], chroma: Chroma<'_>, format: PixelFormat, out: &mut [u32]) {
        simd::words(self, luma, chroma, format, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const RGB: PixelFormat = PixelFormat::XRGB8888;

    #[test]
    fn word_converts_by_matrix_and_range() {
        let near = |colours: &Colours, [luma, blue, red]: [i32; 3], expected: [u8; 3]| {
            let colour = RGB.unpack(colours.word(luma, blue, red, RGB));
            let apart =
                [colour.r().abs_diff(expected[0]), colour.g().abs_diff(expected[1]), colour.b().abs_diff(expected[2])];
            assert!(apart.iter().all(|&apart| apart <= 2), "{luma} {blue} {red} gives {colour:?}, not {expected:?}");
        };
        let high = Colours::new(WEIGHTS_709, false);
        for (samples, colour) in [
            ([16, 128, 128], [0; 3]),
            ([235, 128, 128], [255; 3]),
            ([126, 128, 128], [128; 3]),
            ([63, 102, 240], [255, 0, 0]),
            ([173, 42, 26], [0, 255, 0]),
            ([32, 240, 118], [0, 0, 255]),
            ([219, 16, 138], [255, 255, 0]),
        ] {
            near(&high, samples, colour);
        }
        near(&high, [0, 128, 128], [0; 3]);
        near(&high, [255, 128, 128], [255; 3]);
        near(&high, [255, 255, 0], [49, 255, 255]);
        near(&high, [0, 0, 255], [209, 0, 0]);
        let standard = Colours::new(WEIGHTS_601, false);
        for (samples, colour) in [
            ([81, 90, 240], [255, 0, 0]),
            ([145, 54, 34], [0, 255, 0]),
            ([41, 240, 110], [0, 0, 255]),
            ([235, 128, 128], [255; 3]),
        ] {
            near(&standard, samples, colour);
        }
        let whole = Colours::new(WEIGHTS_601, true);
        for (samples, colour) in [
            ([0, 128, 128], [0; 3]),
            ([255, 128, 128], [255; 3]),
            ([128, 128, 128], [128; 3]),
            ([76, 85, 255], [255, 0, 0]),
            ([29, 255, 107], [0, 0, 255]),
        ] {
            near(&whole, samples, colour);
        }
        let other = PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 };
        assert_eq!(
            other.unpack(high.word(63, 102, 240, other)),
            RGB.unpack(high.word(63, 102, 240, RGB)),
            "in the pixel format asked for"
        );
    }

    #[test]
    fn jfif_rec_601_matrix() {
        assert_eq!(
            Matrix::new(WEIGHTS_601, 256, true),
            Matrix { luma: [77, 150, 29], blue: [-43, -85, 128], red: [128, -107, -21] },
            "in 256ths"
        );
        let whole = Colours::new(WEIGHTS_601, true);
        assert_eq!(
            (whole.luma, whole.red, whole.green_by_blue, whole.green_by_red, whole.blue),
            (65_536, 91_881, 22_553, 46_802, 116_130),
            "1, 1.402, 0.344136, 0.714136 and 1.772 in 65536ths"
        );
    }

    #[test]
    fn row_matches_word() {
        let lights: [u8; 256] = core::array::from_fn(|light| light as u8);
        let mut row = [0u32; 256];
        for colours in
            [Colours::new(WEIGHTS_601, true), Colours::new(WEIGHTS_601, false), Colours::new(WEIGHTS_709, false)]
        {
            for blue in 0..=255 {
                for red in 0..=255 {
                    colours.row(&lights, Chroma::Apart(&[blue; 256], &[red; 256]), RGB, &mut row);
                    let word = |light: u8| colours.word(light.into(), blue.into(), red.into(), RGB);
                    assert!(
                        lights.iter().zip(&row).all(|(&light, &made)| made == word(light)),
                        "every light with blue {blue} and red {red}, at a floor of {}",
                        colours.floor
                    );
                }
            }
        }
    }

    #[test]
    fn colour_runs() {
        let high = Colours::new(WEIGHTS_709, false);
        let (luma, chroma): (Vec<u8>, Vec<u8>) = (
            (0..=255).collect(),
            (0..=255).flat_map(|code| [(3 * code % 256) as u8, (7 * code % 256) as u8]).collect(),
        );
        let mut many = vec![0u32; 256];
        high.row(&luma, Chroma::Beside(&chroma), RGB, &mut many);
        let word = |at: usize| high.word(luma[at].into(), chroma[2 * at].into(), chroma[2 * at + 1].into(), RGB);
        assert!((0..256).all(|at| many[at] == word(at)));
    }
}
