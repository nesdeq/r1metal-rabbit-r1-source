use super::WIDEST;
use crate::base::math::{floor, sine_cosine, sqrt};
use crate::codec::opus::range::RangeCoder;

// The most pulses a codebook holds: 8 more for each octave of the 40 pseudo-pulses of the pulse cache (MAX_PULSES).
const MOST_PULSES: usize = 128;
// f_r of each spread value above none (Table 59).
const SPREAD_FACTORS: [f32; 3] = [15.0, 10.0, 5.0];
// Blocks of this many bins or more are first turned at a stride near the root of their size too (4.3.4.3).
const ROTATION_BLOCK: usize = 8;
// What keeps a vector of noughts from being divided by nought (EPSILON).
const EPSILON: f32 = 1.0e-15;
// The search projects a vector onto the pyramid first where it has more pulses than half its bins, at K plus this much, which can never give more than K; a sum of magnitudes past the bound stands for one that cannot be projected (op_pvq_search_c).
const PROJECTION_MARGIN: f32 = 0.8;
const LARGEST_SUM: f32 = 64.0;
// Pulses the projection leaves past this many more than the bins go to the first bin at once.
const LEFT_OVER: usize = 3;

// The next row of a table whose entries are the sum of the one before, the one above and the one before that (unext).
fn next_row(row: &mut [u32], first: u32) {
    let mut carry = first;
    for at in 1..row.len() {
        let sum = row[at].wrapping_add(row[at - 1]).wrapping_add(carry);
        row[at - 1] = carry;
        carry = sum;
    }
    if let Some(last) = row.last_mut() {
        *last = carry;
    }
}

// The row before, as `next_row` made it (uprev).
fn previous_row(row: &mut [u32], first: u32) {
    let mut carry = first;
    for at in 1..row.len() {
        let difference = row[at].wrapping_sub(row[at - 1]).wrapping_sub(carry);
        row[at - 1] = carry;
        carry = difference;
    }
    if let Some(last) = row.last_mut() {
        *last = carry;
    }
}

// U(n, 0) to U(n, k + 1) into `row`, and V(n, k) = U(n, k) + U(n, k + 1), for n of 2 or more (ncwrs_urow, 4.3.4.2).
fn count(n: usize, k: usize, row: &mut [u32]) -> u32 {
    let row = &mut row[..k + 2];
    row[0] = 0;
    for (at, entry) in row.iter_mut().enumerate().skip(1) {
        *entry = (2 * at as u32).saturating_sub(1);
    }
    for _ in 2..n {
        next_row(&mut row[1..], 1);
    }
    row[k].wrapping_add(row[k + 1])
}

// The vector of `index` among those of k pulses over y's dimensions, with `row` holding U(n, 0) to U(n, k + 1) (cwrsi, 4.3.4.2).
fn vector(mut k: usize, mut index: u32, y: &mut [i32], row: &mut [u32]) {
    for value in y.iter_mut() {
        let is_negative = index >= row[k + 1];
        if is_negative {
            index -= row[k + 1];
        }
        let before = k;
        let mut below = row[k];
        while below > index {
            k -= 1;
            below = row[k];
        }
        index -= below;
        let size = (before - k) as i32;
        *value = if is_negative { -size } else { size };
        previous_row(&mut row[..k + 2], 0);
    }
}

// The index of y among the vectors of its k pulses and their count, V(n, k), with `row` holding U(m, 0) to U(m, k + 1) for each m on the way (icwrs, 4.3.4.2).
fn index_of(y: &[i32], k: usize, row: &mut [u32]) -> (u32, u32) {
    let n = y.len();
    let row = &mut row[..k + 2];
    row[0] = 0;
    for (at, entry) in row.iter_mut().enumerate().skip(1) {
        *entry = 2 * at as u32 - 1;
    }
    let mut index = u32::from(y[n - 1] < 0);
    let mut pulses = y[n - 1].unsigned_abs() as usize;
    for (at, &value) in y.iter().enumerate().take(n - 1).rev() {
        if at < n - 2 {
            next_row(row, 0);
        }
        index = index.wrapping_add(row[pulses]);
        pulses += value.unsigned_abs() as usize;
        if value < 0 {
            index = index.wrapping_add(row[pulses + 1]);
        }
    }
    (index, row[pulses].wrapping_add(row[pulses + 1]))
}

// The vector of k pulses over y's dimensions by its index among all such (4.3.4.2): read into y, or written from it.
fn code_vector<C: RangeCoder>(coder: &mut C, y: &mut [i32], k: usize, row: &mut [u32]) {
    if C::IS_ENCODER {
        let (index, total) = index_of(y, k, row);
        coder.uint(index, total);
    } else {
        let total = count(y.len(), k, row);
        let index = coder.uint(0, total);
        vector(k, index, y, row);
    }
}

// The vector of k pulses nearest x in direction, the signs x's, by projecting onto the pyramid and adding the pulses left one at a time where each adds the most correlation (op_pvq_search_c); x keeps its magnitudes alone.
fn search(x: &mut [f32], y: &mut [i32], k: usize) {
    let n = x.len();
    let mut doubled = [0.0f32; WIDEST];
    let mut is_negative = [false; WIDEST];
    for ((value, sign), pulses) in x.iter_mut().zip(&mut is_negative).zip(y.iter_mut()) {
        *sign = *value < 0.0;
        *value = value.abs();
        *pulses = 0;
    }
    let (mut correlation, mut energy) = (0.0f32, 0.0f32);
    let mut left = k;
    if k > n >> 1 {
        let mut sum: f32 = x.iter().sum();
        if !(sum > EPSILON && sum < LARGEST_SUM) {
            x.fill(0.0);
            x[0] = 1.0;
            sum = 1.0;
        }
        let scale = (k as f32 + PROJECTION_MARGIN) * (1.0 / sum);
        for ((&value, pulses), doubled) in x.iter().zip(y.iter_mut()).zip(&mut doubled) {
            *pulses = floor(scale * value) as i32;
            let projected = *pulses as f32;
            energy += projected * projected;
            correlation += value * projected;
            *doubled = 2.0 * projected;
            left -= *pulses as usize;
        }
    }
    if left > n + LEFT_OVER {
        let pulses = left as f32;
        energy += pulses * pulses;
        energy += pulses * doubled[0];
        y[0] += left as i32;
        left = 0;
    }
    for _ in 0..left {
        energy += 1.0;
        let (mut best, mut best_num, mut best_den) = (0, 0.0f32, 0.0f32);
        for (at, (&value, &twice)) in x.iter().zip(&doubled[..n]).enumerate() {
            let num = correlation + value;
            let (num, den) = (num * num, energy + twice);
            if at == 0 || best_den * num > den * best_num {
                (best, best_num, best_den) = (at, num, den);
            }
        }
        correlation += x[best];
        energy += doubled[best];
        doubled[best] += 2.0;
        y[best] += 1;
    }
    for (pulses, &sign) in y.iter_mut().zip(&is_negative) {
        if sign {
            *pulses = -*pulses;
        }
    }
}

// A unit vector of k pulses over x's dimensions, at `gain`, spread, and its collapse mask over `blocks`: an encoder's the nearest to x, which it then holds turned, a decoder's read (alg_quant, alg_unquant).
pub(super) fn code<C: RangeCoder>(
    coder: &mut C,
    x: &mut [f32],
    k: usize,
    spread: usize,
    blocks: usize,
    gain: f32,
) -> u32 {
    let n = x.len();
    let mut row = [0; MOST_PULSES + 2];
    let mut y = [0; WIDEST];
    let y = &mut y[..n];
    if C::IS_ENCODER {
        rotate(x, blocks, k, spread, true);
        search(x, y, k);
    }
    code_vector(coder, y, k, &mut row);
    if !C::IS_ENCODER {
        let energy: f32 = y.iter().map(|&value| (value * value) as f32).sum();
        let scale = gain / sqrt(energy);
        for (sample, &value) in x.iter_mut().zip(y.iter()) {
            *sample = scale * value as f32;
        }
        rotate(x, blocks, k, spread, false);
    }
    if blocks <= 1 {
        return 1;
    }
    let size = (n / blocks).max(1);
    y.chunks_exact(size)
        .take(blocks)
        .enumerate()
        .fold(0, |mask, (block, values)| mask | u32::from(values.iter().any(|&value| value != 0)) << block)
}

// The rotations of 4.3.4.3 by blocks of `stride`: forward the encoder's, which spreads a vector's energy before its pulses are found, else the decoder's, which undoes it.
fn rotate(x: &mut [f32], stride: usize, k: usize, spread: usize, is_forward: bool) {
    let len = x.len();
    if 2 * k >= len || spread == 0 {
        return;
    }
    let gain = len as f32 / (len as f32 + SPREAD_FACTORS[spread - 1] * k as f32);
    let theta = 0.5 * gain * gain;
    let (sine, cosine) = sine_cosine(f64::from(theta) / 4.0);
    let (sine, cosine) = (sine as f32, cosine as f32);
    let mut stride2 = 0;
    if len >= ROTATION_BLOCK * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    for block in x.chunks_exact_mut(len / stride) {
        if is_forward {
            turn(block, 1, cosine, -sine);
            if stride2 > 0 {
                turn(block, stride2, sine, -cosine);
            }
        } else {
            if stride2 > 0 {
                turn(block, stride2, sine, cosine);
            }
            turn(block, 1, cosine, sine);
        }
    }
}

// Pairs `stride` apart turned forward along the block and back (exp_rotation1).
fn turn(x: &mut [f32], stride: usize, cosine: f32, sine: f32) {
    let len = x.len();
    for at in 0..len.saturating_sub(stride) {
        let (first, second) = (x[at], x[at + stride]);
        x[at + stride] = cosine * second + sine * first;
        x[at] = cosine * first - sine * second;
    }
    for at in (0..len.saturating_sub(2 * stride)).rev() {
        let (first, second) = (x[at], x[at + stride]);
        x[at + stride] = cosine * second + sine * first;
        x[at] = cosine * first - sine * second;
    }
}

// Scales a vector to `gain` (renormalise_vector).
pub(super) fn renormalise(x: &mut [f32], gain: f32) {
    let energy: f32 = EPSILON + x.iter().map(|value| value * value).sum::<f32>();
    let scale = gain / sqrt(energy);
    x.iter_mut().for_each(|value| *value *= scale);
}

#[cfg(test)]
mod tests {
    use super::super::allocation::pulses;
    use super::super::tables::{CACHE_BITS, CACHE_INDEX, E_BANDS};
    use super::super::{BANDS, MAX_LM};
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    // V(n, k) by the recurrence of 4.3.4.2: V(n, k) = V(n - 1, k) + V(n, k - 1) + V(n - 1, k - 1), V(n, 0) = 1, V(0, k) = 0.
    fn recurrence(most_n: usize, most_k: usize) -> Vec<Vec<u64>> {
        let mut table = vec![vec![0u64; most_k + 1]; most_n + 1];
        table[0][0] = 1;
        for n in 1..=most_n {
            table[n][0] = 1;
            for k in 1..=most_k {
                table[n][k] = table[n - 1][k].saturating_add(table[n][k - 1]).saturating_add(table[n - 1][k - 1]);
            }
        }
        table
    }

    #[test]
    fn cwrs_counts() {
        let table = recurrence(40, 40);
        let mut row = [0; MOST_PULSES + 2];
        for (n, counts) in table.iter().enumerate().skip(2) {
            for (k, &expected) in counts.iter().enumerate().skip(1).filter(|&(_, &value)| value < 1 << 32) {
                assert_eq!(u64::from(count(n, k, &mut row)), expected, "V({n}, {k})");
            }
        }
        assert_eq!((table[9][9], table[4][5], table[2][3]), (864_146, 360, 12), "values cwrs.c lists");
    }

    // The vector of an index by the five steps of 4.3.4.2, from V alone.
    fn steps(table: &[Vec<u64>], n: usize, k: usize, mut index: u64) -> Vec<i32> {
        let mut k = k;
        (0..n)
            .map(|j| {
                let mut p = (table[n - j - 1][k] + table[n - j][k]) / 2;
                let sign = if index < p { 1 } else { -1 };
                if sign < 0 {
                    index -= p;
                }
                let k0 = k;
                p -= table[n - j - 1][k];
                while p > index {
                    k -= 1;
                    p -= table[n - j - 1][k];
                }
                index -= p;
                sign * (k0 - k) as i32
            })
            .collect()
    }

    #[test]
    fn cwrs_vectors_follow_the_steps() {
        let table = recurrence(12, 12);
        let mut row = [0; MOST_PULSES + 2];
        for n in 2..=8 {
            for k in 1..=6 {
                let total = count(n, k, &mut row);
                let mut seen = Vec::new();
                for index in 0..total {
                    count(n, k, &mut row);
                    let mut y = vec![0; n];
                    vector(k, index, &mut y, &mut row);
                    assert_eq!(y, steps(&table, n, k, u64::from(index)), "n {n} k {k} index {index}");
                    assert_eq!(y.iter().map(|value| value.unsigned_abs()).sum::<u32>(), k as u32, "k pulses");
                    seen.push(y);
                }
                seen.sort();
                seen.dedup();
                assert_eq!(seen.len() as u32, total, "n {n} k {k}: every index a vector of its own");
            }
        }
    }

    // log2 of a value in 2^-`frac` steps, rounded up (log2_frac of cwrs.c).
    fn log2_frac(value: u32, frac: u32) -> i32 {
        let whole = 32 - value.leading_zeros() as i32;
        if value & (value - 1) == 0 {
            return (whole - 1) << frac;
        }
        let mut value = if whole > 16 {
            let shift = whole - 16;
            (value >> shift) + (((value & ((1 << shift) - 1)) + (1 << shift) - 1) >> shift)
        } else {
            value << (16 - whole)
        };
        let mut log = (whole - 1) << frac;
        for step in (0..=frac).rev() {
            let bit = (value >> 16) as i32;
            log += bit << step;
            value = (value + bit as u32) >> bit;
            value = (value * value + 0x7FFF) >> 15;
        }
        log + i32::from(value > 0x8000)
    }

    #[test]
    fn cache_bits_are_codebook_sizes() {
        let mut row = [0; MOST_PULSES + 2];
        for depth in 0..=MAX_LM + 1 {
            for band in 0..BANDS {
                let n = (E_BANDS[band + 1] - E_BANDS[band]) << depth >> 1;
                let Ok(at) = usize::try_from(CACHE_INDEX[depth * BANDS + band]) else {
                    assert_eq!(n, 0, "only a band of no bins has no codebooks");
                    continue;
                };
                let cache = &CACHE_BITS[at..];
                for (pseudo, &cost) in cache.iter().enumerate().take(usize::from(cache[0]) + 1).skip(1) {
                    let k = pulses(pseudo);
                    let bits = if n == 1 { 8 } else { log2_frac(count(n, k, &mut row), 3) };
                    assert_eq!(i32::from(cost), bits - 1, "n {n} k {k}: the cache holds log2 V(n, k) in eighths");
                    assert!(k + 2 <= row.len() && n <= WIDEST, "n {n} k {k} fit the decoder's room");
                }
            }
        }
    }

    #[test]
    fn rotation_keeps_energy() {
        let original: Vec<f32> = (0..48).map(|at| ((at * 7 % 11) as f32 - 5.0) / 7.0).collect();
        for (spread, blocks, k) in [(1, 1, 3), (2, 2, 5), (3, 4, 2), (3, 1, 10)] {
            let mut x = original.clone();
            rotate(&mut x, blocks, k, spread, false);
            let energy = |x: &[f32]| x.iter().map(|value| value * value).sum::<f32>();
            assert!((energy(&x) - energy(&original)).abs() < 1.0e-4, "spread {spread}: a rotation keeps energy");
            assert!(x.iter().zip(&original).any(|(a, b)| (a - b).abs() > 1.0e-3), "spread {spread}: and moves");
        }
        let mut few = original.clone();
        rotate(&mut few, 1, 24, 2, false);
        assert_eq!(few, original, "no rotation where 2K is the dimension or more");
        rotate(&mut few, 1, 3, 0, false);
        assert_eq!(few, original, "nor with a spread of none");
    }

    #[test]
    fn rotations_forward_and_back() {
        let original: Vec<f32> = (0..48).map(|at| ((at * 7 % 11) as f32 - 5.0) / 7.0).collect();
        let energy = |x: &[f32]| x.iter().map(|value| value * value).sum::<f32>();
        for (spread, blocks, k) in [(1, 1, 3), (2, 2, 5), (3, 4, 2), (3, 1, 10)] {
            let mut x = original.clone();
            rotate(&mut x, blocks, k, spread, true);
            assert!(
                (energy(&x) - energy(&original)).abs() < 1.0e-4,
                "spread {spread}: the forward rotation keeps energy"
            );
            rotate(&mut x, blocks, k, spread, false);
            let error: f32 = x.iter().zip(&original).map(|(a, b)| (a - b) * (a - b)).sum();
            // Measured at most 1.7e-12 of an energy of 10.2; the bound lies near 600 times over.
            assert!(
                error < 1.0e-10 * energy(&original),
                "spread {spread}: and the decoder's turns it back, {error} off"
            );
        }
    }

    #[test]
    fn index_of_each_vector() {
        let mut row = [0; MOST_PULSES + 2];
        for n in 2..=9 {
            for k in 1..=7 {
                let total = count(n, k, &mut row);
                for index in (0..total).step_by((total as usize / 200).max(1)) {
                    count(n, k, &mut row);
                    let mut y = vec![0; n];
                    vector(k, index, &mut y, &mut row);
                    assert_eq!(
                        index_of(&y, k, &mut row),
                        (index, total),
                        "n {n} k {k}: the index of the vector of {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn search_places_every_pulse() {
        let mut state = 0x5EED_u32;
        for case in 0..300 {
            let n = 2 + (crate::base::math::next_random(&mut state) >> 20) as usize % 40;
            let k = 1 + (crate::base::math::next_random(&mut state) >> 20) as usize % 60;
            let x: Vec<f32> =
                (0..n).map(|_| (crate::base::math::next_random(&mut state) >> 16) as f32 / 32_768.0 - 1.0).collect();
            let (mut work, mut y) = (x.clone(), vec![0; n]);
            search(&mut work, &mut y, k);
            assert_eq!(y.iter().map(|value| value.unsigned_abs()).sum::<u32>(), k as u32, "case {case}: k pulses");
            assert!(
                y.iter().zip(&x).all(|(&pulses, &value)| pulses == 0 || (pulses < 0) == (value < 0.0)),
                "case {case}: each of the sign of its bin"
            );
        }
        let (mut silent, mut y) = (vec![0.0; 8], vec![0; 8]);
        search(&mut silent, &mut y, 6);
        assert_eq!(y, [6, 0, 0, 0, 0, 0, 0, 0], "a vector of noughts takes its pulses in the first bin");
        let (mut one, mut y) = (vec![0.0, 0.1, -0.9, 0.2], vec![0; 4]);
        search(&mut one, &mut y, 1);
        assert_eq!(y, [0, 0, -1, 0], "one pulse where the vector is largest");
    }

    #[test]
    fn renormalised_to_gain() {
        let mut x = [3.0, -4.0, 0.0];
        renormalise(&mut x, 2.0);
        assert!((x[0] - 1.2).abs() < 1.0e-6 && (x[1] + 1.6).abs() < 1.0e-6 && x[2] == 0.0, "{x:?}");
        let mut none = [0.0; 4];
        renormalise(&mut none, 1.0);
        assert_eq!(none, [0.0; 4], "noughts stay noughts");
    }
}
