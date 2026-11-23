use super::tables::{ADST_DCT, DCT_ADST, DCT_DCT, MOST_SIDE};

// cos64_lookup: round(16384 * cos(k * pi / 64)) (VP9 8.7.1.1).
#[rustfmt::skip]
const COS: [i64; 33] = [
    16384, 16364, 16305, 16207, 16069, 15893, 15679, 15426, 15137, 14811, 14449, 14053, 13623, 13160, 12665, 12140,
    11585, 11003, 10394, 9760, 9102, 8423, 7723, 7005, 6270, 5520, 4756, 3981, 3196, 2404, 1606, 804, 0,
];
// The four products of the 4 point ADST (VP9 8.7.1.6).
const SINPI_1_9: i64 = 5283;
const SINPI_2_9: i64 = 9929;
const SINPI_3_9: i64 = 13377;
const SINPI_4_9: i64 = 15212;
// A butterfly's product keeps 14 bits of fraction (VP9 8.7.1.1).
const PRODUCT_BITS: u32 = 14;
// The lossless transform takes its rows by two bits less (VP9 8.7.2).
const WHT_ROW_SHIFT: u32 = 2;
// The base 2 logarithms of the 4x4 block and of the largest shift of a column's output (VP9 8.7.2).
const LOG2_4X4: u32 = 2;
const MOST_SHIFT: u32 = 6;

#[inline(always)]
const fn round(value: i64) -> i64 {
    (value + (1 << (PRODUCT_BITS - 1))) >> PRODUCT_BITS
}

fn idct4(io: &mut [i64]) {
    let [a, b, c, d] = [io[0], io[1], io[2], io[3]];
    let step0 = round((a + c) * COS[16]);
    let step1 = round((a - c) * COS[16]);
    let step2 = round(b * COS[24] - d * COS[8]);
    let step3 = round(b * COS[8] + d * COS[24]);
    io[0] = step0 + step3;
    io[1] = step1 + step2;
    io[2] = step1 - step2;
    io[3] = step0 - step3;
}

// A transform of twice the points is the smaller one over its even inputs, steps of its own over the odd ones, and a last butterfly of the two halves: the lanes of each keep their numbers.
fn idct8(io: &mut [i64]) {
    let input: [i64; 8] = core::array::from_fn(|index| io[index]);
    let mut even: [i64; 4] = core::array::from_fn(|index| input[2 * index]);
    idct4(&mut even);
    let mut step1 = [0i64; 8];
    step1[4] = round(input[1] * COS[28] - input[7] * COS[4]);
    step1[7] = round(input[1] * COS[4] + input[7] * COS[28]);
    step1[5] = round(input[5] * COS[12] - input[3] * COS[20]);
    step1[6] = round(input[5] * COS[20] + input[3] * COS[12]);
    let mut step2 = step1;
    step2[4] = step1[4] + step1[5];
    step2[5] = step1[4] - step1[5];
    step2[6] = -step1[6] + step1[7];
    step2[7] = step1[6] + step1[7];
    step1[..4].copy_from_slice(&even);
    step1[4] = step2[4];
    step1[5] = round((step2[6] - step2[5]) * COS[16]);
    step1[6] = round((step2[5] + step2[6]) * COS[16]);
    step1[7] = step2[7];
    for index in 0..4 {
        io[index] = step1[index] + step1[7 - index];
        io[7 - index] = step1[index] - step1[7 - index];
    }
}

fn idct16(io: &mut [i64]) {
    let input: [i64; 16] = core::array::from_fn(|index| io[index]);
    let mut even: [i64; 8] = core::array::from_fn(|index| input[2 * index]);
    idct8(&mut even);
    let mut step1 = [0i64; 16];
    for (place, from) in [1, 9, 5, 13, 3, 11, 7, 15].into_iter().enumerate() {
        step1[8 + place] = input[from];
    }
    let mut step2 = step1;
    step2[8] = round(step1[8] * COS[30] - step1[15] * COS[2]);
    step2[15] = round(step1[8] * COS[2] + step1[15] * COS[30]);
    step2[9] = round(step1[9] * COS[14] - step1[14] * COS[18]);
    step2[14] = round(step1[9] * COS[18] + step1[14] * COS[14]);
    step2[10] = round(step1[10] * COS[22] - step1[13] * COS[10]);
    step2[13] = round(step1[10] * COS[10] + step1[13] * COS[22]);
    step2[11] = round(step1[11] * COS[6] - step1[12] * COS[26]);
    step2[12] = round(step1[11] * COS[26] + step1[12] * COS[6]);

    step1[8] = step2[8] + step2[9];
    step1[9] = step2[8] - step2[9];
    step1[10] = -step2[10] + step2[11];
    step1[11] = step2[10] + step2[11];
    step1[12] = step2[12] + step2[13];
    step1[13] = step2[12] - step2[13];
    step1[14] = -step2[14] + step2[15];
    step1[15] = step2[14] + step2[15];

    step2[8] = step1[8];
    step2[15] = step1[15];
    step2[9] = round(-step1[9] * COS[8] + step1[14] * COS[24]);
    step2[14] = round(step1[9] * COS[24] + step1[14] * COS[8]);
    step2[10] = round(-step1[10] * COS[24] - step1[13] * COS[8]);
    step2[13] = round(-step1[10] * COS[8] + step1[13] * COS[24]);
    step2[11] = step1[11];
    step2[12] = step1[12];

    step1[8] = step2[8] + step2[11];
    step1[9] = step2[9] + step2[10];
    step1[10] = step2[9] - step2[10];
    step1[11] = step2[8] - step2[11];
    step1[12] = -step2[12] + step2[15];
    step1[13] = -step2[13] + step2[14];
    step1[14] = step2[13] + step2[14];
    step1[15] = step2[12] + step2[15];

    step2[..8].copy_from_slice(&even);
    step2[8] = step1[8];
    step2[9] = step1[9];
    step2[10] = round((-step1[10] + step1[13]) * COS[16]);
    step2[13] = round((step1[10] + step1[13]) * COS[16]);
    step2[11] = round((-step1[11] + step1[12]) * COS[16]);
    step2[12] = round((step1[11] + step1[12]) * COS[16]);
    step2[14] = step1[14];
    step2[15] = step1[15];

    for index in 0..8 {
        io[index] = step2[index] + step2[15 - index];
        io[15 - index] = step2[index] - step2[15 - index];
    }
}

// A rotation of `a` and `b` by an angle of `first` and `second` 64ths of a half turn, rounded (VP9 8.7.1.1).
#[inline(always)]
fn rotate((a, b): (i64, i64), (first, second): (usize, usize)) -> (i64, i64) {
    (round(a * COS[first] - b * COS[second]), round(a * COS[second] + b * COS[first]))
}

fn idct32(io: &mut [i64]) {
    let input: [i64; 32] = core::array::from_fn(|index| io[index]);
    let mut even: [i64; 16] = core::array::from_fn(|index| input[2 * index]);
    idct16(&mut even);
    let mut step1 = [0i64; 32];
    for (index, (low, high, angles)) in [
        (1, 31, (31, 1)),
        (17, 15, (15, 17)),
        (9, 23, (23, 9)),
        (25, 7, (7, 25)),
        (5, 27, (27, 5)),
        (21, 11, (11, 21)),
        (13, 19, (19, 13)),
        (29, 3, (3, 29)),
    ]
    .into_iter()
    .enumerate()
    {
        (step1[16 + index], step1[31 - index]) = rotate((input[low], input[high]), angles);
    }

    let mut step2 = step1;
    for base in [16, 20, 24, 28] {
        step2[base] = step1[base] + step1[base + 1];
        step2[base + 1] = step1[base] - step1[base + 1];
        step2[base + 2] = -step1[base + 2] + step1[base + 3];
        step2[base + 3] = step1[base + 2] + step1[base + 3];
    }

    step1 = step2;
    step1[17] = round(-step2[17] * COS[4] + step2[30] * COS[28]);
    step1[30] = round(step2[17] * COS[28] + step2[30] * COS[4]);
    step1[18] = round(-step2[18] * COS[28] - step2[29] * COS[4]);
    step1[29] = round(-step2[18] * COS[4] + step2[29] * COS[28]);
    step1[21] = round(-step2[21] * COS[20] + step2[26] * COS[12]);
    step1[26] = round(step2[21] * COS[12] + step2[26] * COS[20]);
    step1[22] = round(-step2[22] * COS[12] - step2[25] * COS[20]);
    step1[25] = round(-step2[22] * COS[20] + step2[25] * COS[12]);

    step2 = step1;
    for base in [16, 24] {
        step2[base] = step1[base] + step1[base + 3];
        step2[base + 1] = step1[base + 1] + step1[base + 2];
        step2[base + 2] = step1[base + 1] - step1[base + 2];
        step2[base + 3] = step1[base] - step1[base + 3];
        step2[base + 4] = -step1[base + 4] + step1[base + 7];
        step2[base + 5] = -step1[base + 5] + step1[base + 6];
        step2[base + 6] = step1[base + 5] + step1[base + 6];
        step2[base + 7] = step1[base + 4] + step1[base + 7];
    }

    step1 = step2;
    step1[18] = round(-step2[18] * COS[8] + step2[29] * COS[24]);
    step1[29] = round(step2[18] * COS[24] + step2[29] * COS[8]);
    step1[19] = round(-step2[19] * COS[8] + step2[28] * COS[24]);
    step1[28] = round(step2[19] * COS[24] + step2[28] * COS[8]);
    step1[20] = round(-step2[20] * COS[24] - step2[27] * COS[8]);
    step1[27] = round(-step2[20] * COS[8] + step2[27] * COS[24]);
    step1[21] = round(-step2[21] * COS[24] - step2[26] * COS[8]);
    step1[26] = round(-step2[21] * COS[8] + step2[26] * COS[24]);

    step2 = step1;
    for index in 0..4 {
        step2[16 + index] = step1[16 + index] + step1[23 - index];
        step2[23 - index] = step1[16 + index] - step1[23 - index];
        step2[24 + index] = -step1[24 + index] + step1[31 - index];
        step2[31 - index] = step1[24 + index] + step1[31 - index];
    }

    step1 = step2;
    step1[..16].copy_from_slice(&even);
    for index in 0..4 {
        step1[20 + index] = round((-step2[20 + index] + step2[27 - index]) * COS[16]);
        step1[27 - index] = round((step2[20 + index] + step2[27 - index]) * COS[16]);
    }

    for index in 0..16 {
        io[index] = step1[index] + step1[31 - index];
        io[31 - index] = step1[index] - step1[31 - index];
    }
}

fn iadst4(io: &mut [i64]) {
    let [x0, x1, x2, x3] = [io[0], io[1], io[2], io[3]];
    let s0 = SINPI_1_9 * x0 + SINPI_4_9 * x2 + SINPI_2_9 * x3;
    let s1 = SINPI_2_9 * x0 - SINPI_1_9 * x2 - SINPI_4_9 * x3;
    let s2 = SINPI_3_9 * (x0 - x2 + x3);
    let s3 = SINPI_3_9 * x1;
    io[0] = round(s0 + s3);
    io[1] = round(s1 + s3);
    io[2] = round(s2);
    io[3] = round(s0 + s1 - s3);
}

fn iadst8(io: &mut [i64]) {
    let mut x: [i64; 8] = [io[7], io[0], io[5], io[2], io[3], io[4], io[1], io[6]];
    let (s0, s1) = (COS[2] * x[0] + COS[30] * x[1], COS[30] * x[0] - COS[2] * x[1]);
    let (s2, s3) = (COS[10] * x[2] + COS[22] * x[3], COS[22] * x[2] - COS[10] * x[3]);
    let (s4, s5) = (COS[18] * x[4] + COS[14] * x[5], COS[14] * x[4] - COS[18] * x[5]);
    let (s6, s7) = (COS[26] * x[6] + COS[6] * x[7], COS[6] * x[6] - COS[26] * x[7]);
    x = [
        round(s0 + s4),
        round(s1 + s5),
        round(s2 + s6),
        round(s3 + s7),
        round(s0 - s4),
        round(s1 - s5),
        round(s2 - s6),
        round(s3 - s7),
    ];
    let (s4, s5) = (COS[8] * x[4] + COS[24] * x[5], COS[24] * x[4] - COS[8] * x[5]);
    let (s6, s7) = (-COS[24] * x[6] + COS[8] * x[7], COS[8] * x[6] + COS[24] * x[7]);
    x = [
        x[0] + x[2],
        x[1] + x[3],
        x[0] - x[2],
        x[1] - x[3],
        round(s4 + s6),
        round(s5 + s7),
        round(s4 - s6),
        round(s5 - s7),
    ];
    let (x2, x3) = (round(COS[16] * (x[2] + x[3])), round(COS[16] * (x[2] - x[3])));
    let (x6, x7) = (round(COS[16] * (x[6] + x[7])), round(COS[16] * (x[6] - x[7])));
    io.copy_from_slice(&[x[0], -x[4], x6, -x2, x3, -x7, x[5], -x[1]]);
}

fn iadst16(io: &mut [i64]) {
    let order = [15, 0, 13, 2, 11, 4, 9, 6, 7, 8, 5, 10, 3, 12, 1, 14];
    let mut x: [i64; 16] = core::array::from_fn(|index| io[order[index]]);
    let mut s = [0i64; 16];
    for pair in 0..8 {
        let (first, second) = (1 + 4 * pair, 31 - 4 * pair);
        s[2 * pair] = x[2 * pair] * COS[first] + x[2 * pair + 1] * COS[second];
        s[2 * pair + 1] = x[2 * pair] * COS[second] - x[2 * pair + 1] * COS[first];
    }
    for index in 0..8 {
        x[index] = round(s[index] + s[index + 8]);
        x[index + 8] = round(s[index] - s[index + 8]);
    }

    s[..8].copy_from_slice(&x[..8]);
    s[8] = x[8] * COS[4] + x[9] * COS[28];
    s[9] = x[8] * COS[28] - x[9] * COS[4];
    s[10] = x[10] * COS[20] + x[11] * COS[12];
    s[11] = x[10] * COS[12] - x[11] * COS[20];
    s[12] = -x[12] * COS[28] + x[13] * COS[4];
    s[13] = x[12] * COS[4] + x[13] * COS[28];
    s[14] = -x[14] * COS[12] + x[15] * COS[20];
    s[15] = x[14] * COS[20] + x[15] * COS[12];
    for index in 0..4 {
        x[index] = s[index] + s[index + 4];
        x[index + 4] = s[index] - s[index + 4];
        x[index + 8] = round(s[index + 8] + s[index + 12]);
        x[index + 12] = round(s[index + 8] - s[index + 12]);
    }

    for base in [0, 8] {
        let (a, b, c, d) = (x[base + 4], x[base + 5], x[base + 6], x[base + 7]);
        let (s4, s5) = (a * COS[8] + b * COS[24], a * COS[24] - b * COS[8]);
        let (s6, s7) = (-c * COS[24] + d * COS[8], c * COS[8] + d * COS[24]);
        let (s0, s1, s2, s3) = (x[base], x[base + 1], x[base + 2], x[base + 3]);
        x[base] = s0 + s2;
        x[base + 1] = s1 + s3;
        x[base + 2] = s0 - s2;
        x[base + 3] = s1 - s3;
        x[base + 4] = round(s4 + s6);
        x[base + 5] = round(s5 + s7);
        x[base + 6] = round(s4 - s6);
        x[base + 7] = round(s5 - s7);
    }

    let x2 = round(-COS[16] * (x[2] + x[3]));
    let x3 = round(COS[16] * (x[2] - x[3]));
    let x6 = round(COS[16] * (x[6] + x[7]));
    let x7 = round(COS[16] * (-x[6] + x[7]));
    let x10 = round(COS[16] * (x[10] + x[11]));
    let x11 = round(COS[16] * (-x[10] + x[11]));
    let x14 = round(-COS[16] * (x[14] + x[15]));
    let x15 = round(COS[16] * (x[14] - x[15]));
    io.copy_from_slice(&[x[0], -x[8], x[12], -x[4], x6, x14, x10, x2, x3, x11, x15, x7, x[5], -x[13], x[9], -x[1]]);
}

// The inverse Walsh-Hadamard transform of a lossless block, its input shifted first (VP9 8.7.1.10).
fn iwht(io: &mut [i64], shift: u32) {
    let (mut a, mut c, mut d, mut b) = (io[0] >> shift, io[1] >> shift, io[2] >> shift, io[3] >> shift);
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    io[..4].copy_from_slice(&[a, b, c, d]);
}

// One dimension's transform of a block: the DCT or the ADST, or the WHT where the frame is lossless.
#[derive(Clone, Copy)]
enum Kind {
    Dct,
    Adst,
    Wht(u32),
}

fn transform(kind: Kind, log2: u32, io: &mut [i64]) {
    match (kind, log2) {
        (Kind::Wht(shift), _) => iwht(io, shift),
        (Kind::Dct, 2) => idct4(io),
        (Kind::Dct, 3) => idct8(io),
        (Kind::Dct, 4) => idct16(io),
        (Kind::Dct, _) => idct32(io),
        (Kind::Adst, 2) => iadst4(io),
        (Kind::Adst, 3) => iadst8(io),
        (Kind::Adst, _) => iadst16(io),
    }
}

// Adds the inverse transform of raster ordered coefficients, `last` places of them read, to the plane; `middle` is room for the rows transformed, whatever it holds (VP9 8.7.2).
pub(super) fn add(
    block: &[i32],
    middle: &mut [i64],
    (size, kind, is_lossless): (u8, u8, bool),
    last: usize,
    plane: &mut [u8],
    at: usize,
    stride: usize,
) {
    let log2 = u32::from(size) + LOG2_4X4;
    let side = 1usize << log2;
    let shift = if is_lossless { 0 } else { (log2 + 2).min(MOST_SHIFT) };
    let add_value = |plane: &mut [u8], place: usize, value: i64| {
        if let Some(pixel) = plane.get_mut(place) {
            let rounded = if shift == 0 { value } else { (value + (1 << (shift - 1))) >> shift };
            *pixel = (i64::from(*pixel) + rounded).clamp(0, 255) as u8;
        }
    };
    // A DCT whose only coefficient read is its DC adds one value throughout, as libvpx's idct_1 functions do.
    if !is_lossless && kind == DCT_DCT && last == 1 {
        let value = round(round(i64::from(block[0]) * COS[16]) * COS[16]);
        for line in 0..side {
            for column in 0..side {
                add_value(plane, at + line * stride + column, value);
            }
        }
        return;
    }
    let (rows, columns) = match (is_lossless, kind) {
        (true, _) => (Kind::Wht(WHT_ROW_SHIFT), Kind::Wht(0)),
        (false, DCT_DCT) => (Kind::Dct, Kind::Dct),
        (false, ADST_DCT) => (Kind::Dct, Kind::Adst),
        (false, DCT_ADST) => (Kind::Adst, Kind::Dct),
        _ => (Kind::Adst, Kind::Adst),
    };
    let middle = &mut middle[..side * side];
    middle.fill(0);
    for (row, out) in block.chunks_exact(side).zip(middle.chunks_exact_mut(side)) {
        if row.iter().all(|&value| value == 0) {
            continue;
        }
        for (out, &value) in out.iter_mut().zip(row) {
            *out = i64::from(value);
        }
        transform(rows, log2, out);
    }
    let mut column = [0i64; MOST_SIDE];
    for index in 0..side {
        for (line, value) in column.iter_mut().take(side).enumerate() {
            *value = middle[line * side + index];
        }
        transform(columns, log2, &mut column[..side]);
        for (line, &value) in column.iter().take(side).enumerate() {
            add_value(plane, at + line * stride + index, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::vp9::tables::{TX_4X4, TX_8X8, TX_32X32};
    use alloc::vec;

    // The inverse DCT by its definition: the DC weighed by the root of a half, each term by its cosine (VP9 5.8).
    fn dct_by_definition(input: &[i64]) -> alloc::vec::Vec<f64> {
        let side = input.len() as f64;
        (0..input.len())
            .map(|place| {
                input
                    .iter()
                    .enumerate()
                    .map(|(index, &value)| {
                        let weight = if index == 0 { core::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                        let turns = (2.0 * place as f64 + 1.0) * index as f64 / (4.0 * side);
                        weight * value as f64 * crate::base::math::sine_cosine(turns).1
                    })
                    .sum::<f64>()
            })
            .collect()
    }

    #[test]
    fn dcts_near_definition() {
        for log2 in 2..=5u32 {
            let side = 1usize << log2;
            let input: alloc::vec::Vec<i64> = (0..side).map(|index| ((index * 37 + 11) % 200) as i64 - 100).collect();
            let mut output = input.clone();
            transform(Kind::Dct, log2, &mut output);
            let expected = dct_by_definition(&input);
            for (place, (&got, &wanted)) in output.iter().zip(&expected).enumerate() {
                assert!((got as f64 - wanted).abs() < 4.0, "{side} points, place {place}: {got} against {wanted:.1}");
            }
        }
    }

    #[test]
    fn dc_alone_adds_one_value() {
        let mut block = vec![0i32; 64];
        block[0] = 1000;
        let (mut fast, mut full) = (vec![100u8; 64], vec![100u8; 64]);
        // Room that comes filled: nothing of it reaches the plane.
        let mut middle = [i64::MAX; MOST_SIDE * MOST_SIDE];
        add(&block, &mut middle, (TX_8X8, DCT_DCT, false), 1, &mut fast, 0, 8);
        add(&block, &mut middle, (TX_8X8, DCT_DCT, false), 2, &mut full, 0, 8);
        // round(round(1000 * 11585) * 11585) is 500, and (500 + 16) >> 5 is 16.
        assert!(fast.iter().all(|&pixel| pixel == 116), "a DC of 1000 adds 16 throughout: {fast:?}");
        assert_eq!(fast, full, "the shortcut adds what the full transform gives");
    }

    #[test]
    fn lossless_walsh_hadamard() {
        let mut block = vec![0i32; 16];
        block[0] = 4 * 8;
        let mut plane = vec![50u8; 16];
        add(&block, &mut [0; MOST_SIDE * MOST_SIDE], (TX_4X4, DCT_DCT, true), 1, &mut plane, 0, 4);
        // A DC of 32 shifted by 2 is 8: the first row halves it to 4 in each place, each column to 2 in each.
        assert!(plane.iter().all(|&pixel| pixel == 52), "the DC spread over the block: {plane:?}");
    }

    #[test]
    fn adst_rows_and_columns() {
        let mut block = vec![0i32; 1024];
        block[1] = 300;
        let mut rows = vec![128u8; 16];
        let mut middle = [0; MOST_SIDE * MOST_SIDE];
        add(&block[..16], &mut middle, (TX_4X4, DCT_ADST, false), 2, &mut rows, 0, 4);
        assert!(rows.chunks(4).all(|line| line == &rows[..4]), "ADST across the rows only: each line alike");
        let mut columns = vec![128u8; 16];
        add(&block[..16], &mut middle, (TX_4X4, ADST_DCT, false), 2, &mut columns, 0, 4);
        assert_ne!(rows, columns, "the ADST of the columns differs");
        let mut large = vec![0u8; 32 * 32];
        add(&block, &mut middle, (TX_32X32, DCT_DCT, false), 2, &mut large, 0, 32);
        assert!(large.iter().any(|&pixel| pixel > 0), "a 32x32 block's coefficient reaches the plane");
    }
}
