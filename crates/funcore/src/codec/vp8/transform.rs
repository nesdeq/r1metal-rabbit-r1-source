// sqrt(2) * cos(pi / 8) - 1 and sqrt(2) * sin(pi / 8) in 16 bits (RFC 6386, 14.4).
const COS_MINUS_ONE: i32 = 20091;
const SIN: i32 = 35468;
const PRODUCT_BITS: u32 = 16;
const DCT_SHIFT: u32 = 3;
const DCT_ROUNDING: i32 = 4;
const WHT_ROUNDING: i32 = 3;
// The forward transform's sqrt(2) * cos(pi / 8) and sqrt(2) * sin(pi / 8) in 12 bits, the inverse's own rotation.
const FORWARD_COS: i32 = 5352;
const FORWARD_SIN: i32 = 2217;
const FORWARD_BITS: u32 = 12;
// The rows are scaled by 8 for precision; the columns take that out with the half the forward transform owes, 16 in all.
const ROW_SHIFT: u32 = 3;
const COLUMN_SHIFT: u32 = 4;

// The intermediate rows and the results are 16-bit words, which a damaged frame's values wrap (RFC 6386, 14.3, 14.4).
#[inline]
const fn word(value: i32) -> i16 {
    value as i16
}

// The second-order block inverted: the DC coefficient of each luma subblock, in raster order (RFC 6386, 14.3).
pub(super) fn inverse_wht(input: &[i16; 16]) -> [i16; 16] {
    let mut middle = [0i16; 16];
    for column in 0..4 {
        let [first, second, third, fourth] = [0, 4, 8, 12].map(|row| i32::from(input[row + column]));
        let (a1, b1, c1, d1) = (first + fourth, second + third, second - third, first - fourth);
        middle[column] = word(a1 + b1);
        middle[4 + column] = word(c1 + d1);
        middle[8 + column] = word(a1 - b1);
        middle[12 + column] = word(d1 - c1);
    }
    let mut output = [0i16; 16];
    for (line, out) in middle.as_chunks::<4>().0.iter().zip(output.as_chunks_mut::<4>().0) {
        let [first, second, third, fourth] = line.map(i32::from);
        let (a1, b1, c1, d1) = (first + fourth, second + third, second - third, first - fourth);
        out[0] = word((a1 + b1 + WHT_ROUNDING) >> DCT_SHIFT);
        out[1] = word((c1 + d1 + WHT_ROUNDING) >> DCT_SHIFT);
        out[2] = word((a1 - b1 + WHT_ROUNDING) >> DCT_SHIFT);
        out[3] = word((d1 - c1 + WHT_ROUNDING) >> DCT_SHIFT);
    }
    output
}

#[inline]
const fn rotate(first: i32, second: i32) -> (i32, i32) {
    let c1 = ((first * SIN) >> PRODUCT_BITS) - (second + ((second * COS_MINUS_ONE) >> PRODUCT_BITS));
    let d1 = first + ((first * COS_MINUS_ONE) >> PRODUCT_BITS) + ((second * SIN) >> PRODUCT_BITS);
    (c1, d1)
}

// The residue of a subblock from its dequantised coefficients in raster order: columns, then rows (RFC 6386, 14.4).
pub(super) fn inverse_dct(input: &[i16; 16]) -> [i16; 16] {
    let mut middle = [0i16; 16];
    for column in 0..4 {
        let [first, second, third, fourth] = [0, 4, 8, 12].map(|row| i32::from(input[row + column]));
        let (a1, b1) = (first + third, first - third);
        let (c1, d1) = rotate(second, fourth);
        middle[column] = word(a1 + d1);
        middle[4 + column] = word(b1 + c1);
        middle[8 + column] = word(b1 - c1);
        middle[12 + column] = word(a1 - d1);
    }
    let mut output = [0i16; 16];
    for (line, out) in middle.as_chunks::<4>().0.iter().zip(output.as_chunks_mut::<4>().0) {
        let [first, second, third, fourth] = line.map(i32::from);
        let (a1, b1) = (first + third, first - third);
        let (c1, d1) = rotate(second, fourth);
        out[0] = word((a1 + d1 + DCT_ROUNDING) >> DCT_SHIFT);
        out[1] = word((b1 + c1 + DCT_ROUNDING) >> DCT_SHIFT);
        out[2] = word((b1 - c1 + DCT_ROUNDING) >> DCT_SHIFT);
        out[3] = word((a1 - d1 + DCT_ROUNDING) >> DCT_SHIFT);
    }
    output
}

// The coefficients whose inversion by `inverse_dct` gives back a residue in raster order: rows, then columns.
pub(super) fn forward_dct(input: &[i16; 16]) -> [i16; 16] {
    let rotate = |c: i32, d: i32, bits: u32| {
        let half = 1 << (bits - 1);
        ((c * FORWARD_SIN + d * FORWARD_COS + half) >> bits, (d * FORWARD_SIN - c * FORWARD_COS + half) >> bits)
    };
    let mut middle = [0i32; 16];
    for (line, out) in input.as_chunks::<4>().0.iter().zip(middle.as_chunks_mut::<4>().0) {
        let [x0, x1, x2, x3] = line.map(|value| i32::from(value) << ROW_SHIFT);
        let (a, b, c, d) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
        (out[1], out[3]) = rotate(c, d, FORWARD_BITS);
        (out[0], out[2]) = (a + b, a - b);
    }
    let mut output = [0i16; 16];
    let half = 1 << (COLUMN_SHIFT - 1);
    for column in 0..4 {
        let [x0, x1, x2, x3] = [0, 4, 8, 12].map(|row| middle[row + column]);
        let (a, b, c, d) = (x0 + x3, x1 + x2, x1 - x2, x0 - x3);
        let (odd, three) = rotate(c, d, FORWARD_BITS + COLUMN_SHIFT);
        output[column] = word((a + b + half) >> COLUMN_SHIFT);
        output[4 + column] = word(odd);
        output[8 + column] = word((a - b + half) >> COLUMN_SHIFT);
        output[12 + column] = word(three);
    }
    output
}

// The second-order coefficients of the luma subblocks' DCs in raster order, which `inverse_wht` inverts.
pub(super) fn forward_wht(input: &[i16; 16]) -> [i16; 16] {
    let hadamard = |[x0, x1, x2, x3]: [i32; 4]| {
        let (a, b, c, d) = (x0 + x1, x2 + x3, x0 - x1, x2 - x3);
        [a + b, a - b, c - d, c + d]
    };
    let mut middle = [0i32; 16];
    for (line, out) in input.as_chunks::<4>().0.iter().zip(middle.as_chunks_mut::<4>().0) {
        *out = hadamard(line.map(i32::from));
    }
    let mut output = [0i16; 16];
    for column in 0..4 {
        let values = hadamard([0, 4, 8, 12].map(|row| middle[row + column]));
        for (row, value) in values.into_iter().enumerate() {
            // The forward transform halves, rounding away from nought.
            output[row * 4 + column] = word((value + value.signum()) / 2);
        }
    }
    output
}

// Adds a residue to the 4x4 prediction at `at` of a plane, each sum held to a byte (RFC 6386, 14.5).
#[inline]
pub(super) fn add_residue(residue: &[i16; 16], plane: &mut [u8], at: usize, stride: usize) {
    for (line, values) in residue.as_chunks::<4>().0.iter().enumerate() {
        let start = at + line * stride;
        if let Some(pixels) = plane.get_mut(start..start + 4) {
            for (pixel, &value) in pixels.iter_mut().zip(values) {
                *pixel = (i32::from(*pixel) + i32::from(value)).clamp(0, 255) as u8;
            }
        }
    }
}

// Adds the residue of a subblock whose one coefficient is its DC, the same throughout, as the full inversion gives it.
#[inline]
pub(super) fn add_dc(dc: i16, plane: &mut [u8], at: usize, stride: usize) {
    let value = (i32::from(dc) + DCT_ROUNDING) >> DCT_SHIFT;
    for line in 0..4 {
        let start = at + line * stride;
        if let Some(pixels) = plane.get_mut(start..start + 4) {
            for pixel in pixels {
                *pixel = (i32::from(*pixel) + value).clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_dc_basis() {
        let mut input = [0i16; 16];
        input[0] = 80;
        assert_eq!(inverse_dct(&input), [10; 16], "a DC of 80 is 10 throughout, (80 + 4) >> 3");
        input[0] = -85;
        assert_eq!(inverse_dct(&input), [-11; 16], "the shift floors: (-85 + 4) >> 3 is -11");
        let mut plane = [100u8; 16];
        add_dc(-85, &mut plane, 0, 4);
        assert_eq!(plane, [89; 16], "the DC shortcut adds what the full inversion gives");
    }

    #[test]
    fn idct_ac_basis() {
        let mut input = [0i16; 16];
        input[1] = 100;
        // 100 * 35468 >> 16 is 54 and 100 + (100 * 20091 >> 16) is 130, each then (x + 4) >> 3.
        let row = [16, 7, -7, -16];
        assert_eq!(inverse_dct(&input)[..], row.repeat(4)[..], "the first horizontal basis function in every row");
        let mut vertical = [0i16; 16];
        vertical[4] = 100;
        let result = inverse_dct(&vertical);
        let column: [i16; 4] = core::array::from_fn(|line| result[line * 4]);
        assert_eq!(column, row, "the first vertical basis function down each column");
        assert!(
            result.as_chunks::<4>().0.iter().all(|line| line.iter().all(|&value| value == line[0])),
            "a vertical function is the same across each row"
        );
    }

    #[test]
    fn idct_words_wrap() {
        let mut input = [0i16; 16];
        input[0] = i16::MAX;
        input[8] = i16::MAX;
        // 32767 + 32767 overflows the 16-bit word of the column pass: it wraps to -2, and the rows read (-2 + 4) >> 3.
        let result = inverse_dct(&input);
        assert_eq!(result[0], 0, "a column sum that wraps as the reference's short does");
    }

    #[test]
    fn wht_basis() {
        let mut input = [0i16; 16];
        input[0] = 80;
        assert_eq!(inverse_wht(&input), [10; 16], "a DC of 80 gives each subblock (80 + 3) >> 3");
        input[0] = 0;
        input[1] = 16;
        let expected = [2, 2, -2, -2].repeat(4);
        assert_eq!(
            inverse_wht(&input)[..],
            expected[..],
            "the first horizontal sequency, (16 + 3) >> 3 and (-16 + 3) >> 3"
        );
        let mut vertical = [0i16; 16];
        vertical[12] = 16;
        let result = inverse_wht(&vertical);
        let column: [i16; 4] = core::array::from_fn(|line| result[line * 4]);
        assert_eq!(column, [2, -2, 2, -2], "the highest vertical sequency alternates");
    }

    #[test]
    fn forward_inverse_round_trip() {
        let mut state = 0x9E37_79B9u32;
        let mut worst = (0, 0);
        for _ in 0..20_000 {
            let residue: [i16; 16] = core::array::from_fn(|_| {
                state = crate::base::math::xorshift32(state);
                (state % 511) as i16 - 255
            });
            let back = inverse_dct(&forward_dct(&residue));
            let dct = residue.iter().zip(back).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0);
            let dcs: [i16; 16] = residue.map(|value| value * 8 + value % 8);
            let wht = dcs.iter().zip(inverse_wht(&forward_wht(&dcs))).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0);
            worst = (worst.0.max(dct), worst.1.max(wht));
        }
        assert_eq!(worst, (1, 1), "the inversions give back residues and DCs within one");
        let flat = forward_dct(&[-37; 16]);
        assert_eq!((flat[0], flat[1..].iter().all(|&value| value == 0)), (-296, true), "a flat block is its DC alone");
        assert_eq!(forward_wht(&[80; 16])[0], 640, "sixteen DCs of 80: half their sum");
    }

    #[test]
    fn residue_held_to_bytes() {
        let mut plane = [250u8, 3, 128, 0, 9, 9, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 0xAA];
        let residue = [10, -10, 5, 0, -9, -10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        add_residue(&residue, &mut plane, 0, 4);
        assert_eq!(plane[..8], [255, 0, 133, 0, 0, 0, 9, 9], "sums above 255 and below 0 are held at the ends");
        assert_eq!(plane[16], 0xAA, "nothing past the block is touched");
    }
}
