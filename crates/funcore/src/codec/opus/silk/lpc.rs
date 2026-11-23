use super::MOST_ORDER;
use super::fixed::{inverse32_varq, rshift_round, rshift_round64, sat16, smlawb, smmul, smulbb, smulww, sqrt_approx};
use super::tables::{
    COSINES, LSF_NB_FIRST_LIST, LSF_NB_ORDER, LSF_NB_SPACING, LSF_NB_VECTORS, LSF_NB_WEIGHT_SELECT, LSF_NB_WEIGHTS,
    LSF_WB_FIRST_LIST, LSF_WB_ORDER, LSF_WB_SPACING, LSF_WB_VECTORS, LSF_WB_WEIGHT_SELECT, LSF_WB_WEIGHTS,
};

// 4.2.7.5.2: the stage-2 step in Q16 for NB and MB and for WB, and the level adjustment of an index in Q10.
const NB_STEP: i32 = 11_796;
const WB_STEP: i32 = 9_830;
const LEVEL_ADJUST: i32 = 102;
const RESIDUAL_Q: u32 = 10;
const WEIGHT_Q: u32 = 8;
// 4.2.7.5.3: the stage-1 codebook in Q8 is placed in Q15, a weight's square is 1024 over each gap in Q18, and the residual is divided by the weight from Q24.
const CODEBOOK_SHIFT: u32 = 7;
const CODEBOOK_TOP: i32 = 256;
const WEIGHT_NUMERATOR: i32 = 1024;
const SQUARE_SHIFT: u32 = 16;
const RESIDUAL_SHIFT: u32 = 14;
const ONE_Q15: i32 = 1 << 15;
// 4.2.7.5.4
const STABILIZE_ROUNDS: usize = 20;
// 4.2.7.5.6: the top 7 bits of a coefficient index the cosine table, the next 8 interpolate; the polynomials count in Q16.
const COSINE_FRACTION_BITS: u32 = 8;
const COSINE_ROUND: i32 = 4;
const COSINE_SHIFT: u32 = 3;
const POLYNOMIAL_Q: u32 = 16;
// 4.2.7.5.7: up to 10 rounds of bandwidth expansion, from Q17 to Q12, and the bound and chirp of a round.
const RANGE_ROUNDS: usize = 10;
const Q17_TO_Q12: u32 = 5;
const MOST_MAXABS: i32 = 163_838;
const RANGE_CHIRP: i32 = 65_470;
const CHIRP_SHIFT: u32 = 14;
const CHIRP_DIVISOR_SHIFT: u32 = 2;
// 4.2.7.5.8: up to 16 rounds, each of a chirp of 65536 less 2 << round, until the inverse prediction gain is 1/10000 in Q30.
const GAIN_ROUNDS: usize = 16;
const ONE_Q16: i32 = 1 << 16;
const LEAST_INVERSE_GAIN: i32 = 107_374;
const DC_LIMIT: i32 = 4096;
const Q12_TO_Q24: u32 = 12;
const COEFFICIENT_LIMIT: i32 = 16_773_022;
const Q24_TO_Q31: u32 = 7;
const ONE_Q30: i32 = 1 << 30;
const GAIN_Q: i32 = 30;
const RC_Q: u32 = 31;
const INVERSE_SHIFT: u32 = 2;

// The normalized LSFs in Q15 of a frame's stage-1 index and stage-2 residual indices, stabilized (4.2.7.5.2 to 4.2.7.5.4).
pub(super) fn decode(stage1: usize, indices: &[i32], is_wide: bool) -> [i16; MOST_ORDER] {
    let order = indices.len();
    let (vector, step, spacing): (&[u8], i32, &[i32]) = if is_wide {
        (&LSF_WB_VECTORS[stage1], WB_STEP, &LSF_WB_SPACING)
    } else {
        (&LSF_NB_VECTORS[stage1], NB_STEP, &LSF_NB_SPACING)
    };
    let weight = |coefficient: usize| {
        i32::from(if is_wide {
            LSF_WB_WEIGHTS[usize::from(LSF_WB_WEIGHT_SELECT[stage1][coefficient] - LSF_WB_FIRST_LIST)][coefficient]
        } else {
            LSF_NB_WEIGHTS[usize::from(LSF_NB_WEIGHT_SELECT[stage1][coefficient] - LSF_NB_FIRST_LIST)][coefficient]
        })
    };
    let mut residuals = [0; MOST_ORDER];
    for coefficient in (0..order).rev() {
        let prediction = if coefficient + 1 < order {
            smulbb(residuals[coefficient + 1], weight(coefficient)) >> WEIGHT_Q
        } else {
            0
        };
        let index = indices[coefficient];
        let level = (index << RESIDUAL_Q) - index.signum() * LEVEL_ADJUST;
        residuals[coefficient] = i32::from(smlawb(prediction, level, step) as i16);
    }
    let mut nlsf = [0; MOST_ORDER];
    for coefficient in 0..order {
        let below = if coefficient == 0 { 0 } else { i32::from(vector[coefficient - 1]) };
        let above = vector.get(coefficient + 1).map_or(CODEBOOK_TOP, |&value| i32::from(value));
        let here = i32::from(vector[coefficient]);
        let square = (WEIGHT_NUMERATOR / (here - below) + WEIGHT_NUMERATOR / (above - here)) << SQUARE_SHIFT;
        let value = (here << CODEBOOK_SHIFT) + (residuals[coefficient] << RESIDUAL_SHIFT) / sqrt_approx(square);
        nlsf[coefficient] = value.clamp(0, ONE_Q15 - 1) as i16;
    }
    stabilize(&mut nlsf[..order], spacing);
    nlsf
}

// Moves coefficients apart until each lies its least spacing from the one below, the first from 0 and the last from 1 (4.2.7.5.4, with RFC 8251's saturation).
fn stabilize(nlsf: &mut [i16], spacing: &[i32]) {
    let order = nlsf.len();
    for _ in 0..STABILIZE_ROUNDS {
        let (mut least, mut at) = (i32::from(nlsf[0]) - spacing[0], 0);
        for coefficient in 1..order {
            let gap = i32::from(nlsf[coefficient]) - i32::from(nlsf[coefficient - 1]) - spacing[coefficient];
            if gap < least {
                (least, at) = (gap, coefficient);
            }
        }
        let gap = ONE_Q15 - i32::from(nlsf[order - 1]) - spacing[order];
        if gap < least {
            (least, at) = (gap, order);
        }
        if least >= 0 {
            return;
        }
        if at == 0 {
            nlsf[0] = spacing[0] as i16;
        } else if at == order {
            nlsf[order - 1] = (ONE_Q15 - spacing[order]) as i16;
        } else {
            let half = spacing[at] >> 1;
            let low = spacing[..at].iter().sum::<i32>() + half;
            let high = ONE_Q15 - spacing[at + 1..].iter().sum::<i32>() - half;
            let center = rshift_round(i32::from(nlsf[at - 1]) + i32::from(nlsf[at]), 1).clamp(low, high);
            nlsf[at - 1] = (center - half) as i16;
            nlsf[at] = (i32::from(nlsf[at - 1]) + spacing[at]) as i16;
        }
    }
    nlsf.sort_unstable();
    nlsf[0] = nlsf[0].max(spacing[0] as i16);
    for coefficient in 1..order {
        nlsf[coefficient] = nlsf[coefficient].max(sat16(i32::from(nlsf[coefficient - 1]) + spacing[coefficient]));
    }
    nlsf[order - 1] = nlsf[order - 1].min((ONE_Q15 - spacing[order]) as i16);
    for coefficient in (0..order - 1).rev() {
        nlsf[coefficient] = nlsf[coefficient].min((i32::from(nlsf[coefficient + 1]) - spacing[coefficient + 1]) as i16);
    }
}

// The coefficients of P or Q in Q16, of the cosines of every other coefficient (4.2.7.5.6).
fn polynomial(cosines: &[i32]) -> [i32; MOST_ORDER / 2 + 1] {
    let half = cosines.len().div_ceil(2);
    let mut terms = [0; MOST_ORDER / 2 + 1];
    terms[0] = 1 << POLYNOMIAL_Q;
    terms[1] = -cosines[0];
    for k in 1..half {
        let cosine = i64::from(cosines[2 * k]);
        let product = |term: i32| rshift_round64(cosine * i64::from(term), POLYNOMIAL_Q) as i32;
        terms[k + 1] = (terms[k - 1] << 1).wrapping_sub(product(terms[k]));
        for j in (2..=k).rev() {
            terms[j] = terms[j].wrapping_add(terms[j - 2]).wrapping_sub(product(terms[j - 1]));
        }
        terms[1] = terms[1].wrapping_sub(cosines[2 * k]);
    }
    terms
}

// Expands the bandwidth of a filter in Q17 by a chirp in Q16 (silk_bwexpander_32).
fn expand_wide(lpc: &mut [i32], chirp: i32) {
    let step = chirp - ONE_Q16;
    let mut chirp = chirp;
    let last = lpc.len() - 1;
    for coefficient in &mut lpc[..last] {
        *coefficient = smulww(chirp, *coefficient);
        chirp += rshift_round(chirp.wrapping_mul(step), POLYNOMIAL_Q);
    }
    lpc[last] = smulww(chirp, lpc[last]);
}

// Expands the bandwidth of a filter in Q12 by a chirp in Q16 (silk_bwexpander).
pub(super) fn expand(lpc: &mut [i16], chirp: i32) {
    let step = chirp - ONE_Q16;
    let mut chirp = chirp;
    let last = lpc.len() - 1;
    for coefficient in &mut lpc[..last] {
        *coefficient = rshift_round(chirp.wrapping_mul(i32::from(*coefficient)), POLYNOMIAL_Q) as i16;
        chirp += rshift_round(chirp.wrapping_mul(step), POLYNOMIAL_Q);
    }
    lpc[last] = rshift_round(chirp.wrapping_mul(i32::from(lpc[last])), POLYNOMIAL_Q) as i16;
}

// The LPC filter in Q12 of normalized LSFs, its range limited and its prediction gain bounded (4.2.7.5.6 to 4.2.7.5.8).
pub(super) fn to_lpc(nlsf: &[i16]) -> [i16; MOST_ORDER] {
    let order = nlsf.len();
    let ordering: &[usize] = if order == MOST_ORDER { &LSF_WB_ORDER } else { &LSF_NB_ORDER };
    let mut cosines = [0; MOST_ORDER];
    for (&value, &place) in nlsf.iter().zip(ordering) {
        let value = i32::from(value.max(0));
        let (index, fraction) = ((value >> COSINE_FRACTION_BITS) as usize, value & ((1 << COSINE_FRACTION_BITS) - 1));
        let (low, high) = (COSINES[index], COSINES[index + 1]);
        cosines[place] = ((low << COSINE_FRACTION_BITS) + (high - low) * fraction + COSINE_ROUND) >> COSINE_SHIFT;
    }
    let (p, q) = (polynomial(&cosines[..order]), polynomial(&cosines[1..order]));
    let mut wide = [0; MOST_ORDER];
    for k in 0..order / 2 {
        let (sum, difference) = (p[k + 1].wrapping_add(p[k]), q[k + 1].wrapping_sub(q[k]));
        wide[k] = difference.wrapping_neg().wrapping_sub(sum);
        wide[order - k - 1] = difference.wrapping_sub(sum);
    }
    let wide = &mut wide[..order];
    let mut rounds = 0;
    while rounds < RANGE_ROUNDS {
        let (mut maxabs, mut at) = (0, 0);
        for (coefficient, &value) in wide.iter().enumerate() {
            if value.wrapping_abs() > maxabs {
                (maxabs, at) = (value.wrapping_abs(), coefficient);
            }
        }
        let maxabs = rshift_round(maxabs, Q17_TO_Q12);
        if maxabs <= i32::from(i16::MAX) {
            break;
        }
        let maxabs = maxabs.min(MOST_MAXABS);
        let excess = (maxabs - i32::from(i16::MAX)) << CHIRP_SHIFT;
        expand_wide(wide, RANGE_CHIRP - excess / ((maxabs * (at as i32 + 1)) >> CHIRP_DIVISOR_SHIFT));
        rounds += 1;
    }
    let mut lpc = [0; MOST_ORDER];
    for (coefficient, value) in lpc.iter_mut().zip(wide.iter_mut()) {
        if rounds == RANGE_ROUNDS {
            *coefficient = sat16(rshift_round(*value, Q17_TO_Q12));
            *value = i32::from(*coefficient) << Q17_TO_Q12;
        } else {
            *coefficient = rshift_round(*value, Q17_TO_Q12) as i16;
        }
    }
    for round in 0..GAIN_ROUNDS {
        if inverse_gain(&lpc[..order]) >= LEAST_INVERSE_GAIN {
            break;
        }
        expand_wide(wide, ONE_Q16 - (2 << round));
        for (coefficient, &value) in lpc.iter_mut().zip(wide.iter()) {
            *coefficient = rshift_round(value, Q17_TO_Q12) as i16;
        }
    }
    lpc
}

// The inverse of a filter's prediction gain in Q30, none where it is unstable (4.2.7.5.8, with RFC 8251's check of 32 bits).
pub(super) fn inverse_gain(lpc: &[i16]) -> i32 {
    let order = lpc.len();
    let mut row = [0; MOST_ORDER];
    let mut dc = 0;
    for (wide, &coefficient) in row.iter_mut().zip(lpc) {
        dc += i32::from(coefficient);
        *wide = i32::from(coefficient) << Q12_TO_Q24;
    }
    if dc >= DC_LIMIT {
        return 0;
    }
    let mut inverse = ONE_Q30;
    for k in (0..order).rev() {
        if !(-COEFFICIENT_LIMIT..=COEFFICIENT_LIMIT).contains(&row[k]) {
            return 0;
        }
        let rc = -(row[k] << Q24_TO_Q31);
        let divisor = ONE_Q30 - smmul(rc, rc);
        inverse = smmul(inverse, divisor) << INVERSE_SHIFT;
        if k == 0 {
            break;
        }
        let bits = i32::BITS - divisor.leading_zeros();
        let gain = inverse32_varq(divisor, bits as i32 + GAIN_Q);
        let before = row;
        for n in 0..k {
            let product = rshift_round64(i64::from(before[k - n - 1]) * i64::from(rc), RC_Q) as i32;
            let numerator = before[n].saturating_sub(product);
            let next = rshift_round64(i64::from(numerator) * i64::from(gain), bits);
            row[n] = match i32::try_from(next) {
                Ok(value) => value,
                Err(_) => return 0,
            };
        }
    }
    inverse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residual_prediction() {
        // Codebook vector 0 of NB and MB with an I2 of 1 at the last coefficient alone, which reaches each one below by the weights of row 0 of Table 21, A but for coefficient 1.
        let mut indices = [0; 10];
        indices[9] = 1;
        let nlsf = decode(0, &indices, false);
        let mut residuals = [0; 10];
        residuals[9] = ((1024 - 102) * NB_STEP) >> 16;
        for k in (0..9).rev() {
            residuals[k] = (residuals[k + 1] * i32::from(LSF_NB_WEIGHTS[usize::from(k == 1)][k])) >> 8;
        }
        assert_eq!(residuals[..3], [0, 0, 3], "the prediction fades below the third coefficient");
        let vector = LSF_NB_VECTORS[0].map(i32::from);
        for k in 0..10 {
            let below = if k == 0 { 0 } else { vector[k - 1] };
            let above = if k == 9 { 256 } else { vector[k + 1] };
            let square = (1024 / (vector[k] - below) + 1024 / (above - vector[k])) << 16;
            let expected = (vector[k] << 7) + (residuals[k] << 14) / sqrt_approx(square);
            assert_eq!(i32::from(nlsf[k]), expected, "res_Q10 and w_Q9 of 4.2.7.5.2 and 4.2.7.5.3 at coefficient {k}");
        }
    }

    #[test]
    fn stabilized() {
        let mut close = [100i16, 101, 6000, 6001, 6002, 12000, 18000, 24000, 30000, 32700];
        stabilize(&mut close, &LSF_NB_SPACING);
        let gaps: alloc::vec::Vec<i32> = core::iter::once(i32::from(close[0]))
            .chain(close.windows(2).map(|pair| i32::from(pair[1]) - i32::from(pair[0])))
            .chain(core::iter::once(ONE_Q15 - i32::from(close[9])))
            .collect();
        assert!(gaps.iter().zip(&LSF_NB_SPACING).all(|(gap, least)| gap >= least), "{close:?}: {gaps:?}");
        let mut first = [10i16, 3000, 6000, 9000, 12000, 15000, 18000, 21000, 24000, 27000];
        stabilize(&mut first, &LSF_NB_SPACING);
        assert_eq!(first[0], 250, "a first coefficient too low moves up to its spacing");
        let mut pair = [1000i16, 3000, 6000, 9000, 9001, 15000, 18000, 21000, 24000, 27000];
        stabilize(&mut pair, &LSF_NB_SPACING);
        assert_eq!(
            (pair[3], pair[4]),
            (9000, 9003),
            "two too close move apart about their center, (9000 + 9001 + 1) >> 1"
        );
        let mut sorted = [30000i16, 20000, 10000, 5000, 2000, 1000, 500, 400, 300, 200];
        stabilize(&mut sorted, &LSF_NB_SPACING);
        assert!(sorted.windows(2).all(|pair| pair[0] < pair[1]), "the fallback sorts: {sorted:?}");
        let mut top = [32767i16; 16];
        stabilize(&mut top, &LSF_WB_SPACING);
        assert!(top.windows(2).all(|pair| pair[0] < pair[1]) && i32::from(top[15]) <= ONE_Q15 - 347, "{top:?}");
    }

    // The cosine of 4.2.7.5.6 in Q17 of a coefficient in Q15.
    fn cosine(value: i32) -> i32 {
        let (index, fraction) = ((value >> 8) as usize, value & 255);
        (COSINES[index] * 256 + (COSINES[index + 1] - COSINES[index]) * fraction + 4) >> 3
    }

    #[test]
    fn lpc_of_worked_lsfs() {
        // Evenly spaced LSFs: the polynomials' recurrence of 4.2.7.5.6, in i64 and in the RFC's order of terms.
        let nlsf: [i16; 10] = core::array::from_fn(|k| (32768 * (k as i32 + 1) / 11) as i16);
        let mut c = [0i64; 10];
        for (k, &value) in nlsf.iter().enumerate() {
            c[LSF_NB_ORDER[k]] = i64::from(cosine(i32::from(value)));
        }
        // A row's terms below 0 are none, and the one past its end is the one before its last, by symmetry.
        let rows = |first: usize| {
            let mut row = alloc::vec![65536i64, -c[first]];
            for k in 1..5 {
                let before = row.clone();
                let at = |j: i64| match j {
                    ..0 => 0,
                    j if j as usize == k + 1 => before[k - 1],
                    j => before[j as usize],
                };
                let next = |j: i64| at(j) + at(j - 2) - ((c[2 * k + first] * at(j - 1) + 32768) >> 16);
                row = (0..=k as i64 + 1).map(next).collect();
            }
            row
        };
        let (p, q) = (rows(0), rows(1));
        let lpc = to_lpc(&nlsf);
        for k in 0..5 {
            let (sum, difference) = (p[k + 1] + p[k], q[k + 1] - q[k]);
            assert_eq!(i64::from(lpc[k]), (-difference - sum + 16) >> 5, "a32_Q17 of coefficient {k} in Q12");
            assert_eq!(i64::from(lpc[9 - k]), (difference - sum + 16) >> 5, "and of coefficient {}", 9 - k);
        }
        assert!(inverse_gain(&lpc[..10]) >= LEAST_INVERSE_GAIN, "a stable filter needs no expansion");
    }

    #[test]
    fn unstable_filters_expanded() {
        assert_eq!(inverse_gain(&[4096, 0, 0, 0, 0, 0, 0, 0, 0, 0]), 0, "a DC response of 4096 is unstable");
        assert_eq!(inverse_gain(&[0; 10]), ONE_Q30, "no prediction has a gain of one");
        let one = inverse_gain(&[2048, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(one, ONE_Q30 - ONE_Q30 / 4, "a reflection of a half leaves 3/4 of the energy");
        // LSFs packed close in pairs make filters of a high gain, which the rounds of 4.2.7.5.8 bound.
        let tight: [i16; 16] = core::array::from_fn(|k| (2000 * (k as i32 / 2 + 1) + 3 * (k as i32 % 2)) as i16);
        let lpc = to_lpc(&tight);
        assert!(lpc.iter().any(|&value| value != 0) && inverse_gain(&lpc) >= LEAST_INVERSE_GAIN, "{lpc:?}");
        let mut chirped = [4096i16, -2048, 1024, 512];
        expand(&mut chirped, 32768);
        assert_eq!(
            chirped,
            [2048, -512, 128, 32],
            "a chirp of a half halves each coefficient once more than the one before"
        );
    }
}
