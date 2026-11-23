use super::fixed::{div32_varq, inverse32_varq, lcg, rshift_round, sat16, smlabb, smlawb, smulwb, smulww};
use super::indices::{Indices, Signal};
use super::params::Params;
use super::tables::{EXCITATION_OFFSETS, LTP_TAPS};
use super::{Channel, Layout, MOST_FRAME, MOST_ORDER, MOST_SUBFRAME, MOST_SUBFRAMES, UNIT_GAIN_Q16};

// 4.2.7.8.6: a pulse is e_raw << 8 in Q23, less 20 toward none where it is not none; the synthesis runs in the Q14 of the reference decoder.
const PULSE_SHIFT: u32 = 8;
const PULSE_ADJUST: i32 = 20;
const Q23_TO_Q14: u32 = 6;
// The LTP prediction starts at 2 in Q13 against the bias of silk_SMLAWB's rounding down, and lies a bit below the residual in Q14 and its state a bit above.
const LTP_BIAS: i32 = 2;
const LTP_SHIFT: u32 = 1;
// The inverse of a gain is taken in Q47, the inverse that rewhitening scales by in Q31, and a gain adjustment in Q16.
const INVERSE_Q: i32 = 47;
const ADJUST_Q: i32 = 16;
pub(super) const GAIN_TO_Q10: u32 = 6;
const LTP_SCALE_SHIFT: u32 = 2;
const OUT_SHIFT: u32 = 8;
const LPC_SHIFT: u32 = 4;
const Q12: u32 = 12;
// A frame that is not voiced, the first after a loss behind a voiced one, is voiced in its first half, by one tap of 0.25 in Q14 at the last lag (silk/decode_core.c).
const LOSS_TAP: i16 = 1 << 12;
// The LTP filter is centred on the lag: 2 taps lie on each side.
pub(super) const LTP_HALF: usize = LTP_TAPS / 2;

// The residual of a whitening filter in Q12 over `input`, its first `order` values none (silk_LPC_analysis_filter).
pub(super) fn whiten(out: &mut [i16], input: &[i16], lpc: &[i16]) {
    let order = lpc.len();
    for at in order..input.len() {
        let mut prediction = 0;
        for (back, &coefficient) in lpc.iter().enumerate() {
            prediction = smlabb(prediction, i32::from(input[at - 1 - back]), i32::from(coefficient));
        }
        let residual = (i32::from(input[at]) << Q12).wrapping_sub(prediction);
        out[at] = sat16(rshift_round(residual, Q12));
    }
    out[..order].fill(0);
}

// The LTP filter's prediction of the sample at `at` from the five samples about a lag before it, in Q13 (silk_decode_core, silk_PLC_conceal).
pub(super) fn predict(ltp: &[i32], at: usize, lag: usize, taps: &[i16; LTP_TAPS]) -> i32 {
    let centre = at - lag + LTP_HALF;
    let mut prediction = LTP_BIAS;
    for (back, &tap) in taps.iter().enumerate() {
        prediction = smlawb(prediction, ltp[centre - back], i32::from(tap));
    }
    prediction
}

// The LPC synthesis filter over `state`, the filter's past in its first MOST_ORDER values and its input behind them, which its output in Q14 takes the place of; the samples are that output at the gain (silk_decode_core, silk_PLC_conceal).
pub(super) fn filter(state: &mut [i32], lpc: &[i16], gain_q10: i32, out: &mut [i16]) {
    for (i, sample) in out.iter_mut().enumerate() {
        let mut prediction = (lpc.len() >> 1) as i32;
        for (back, &coefficient) in lpc.iter().enumerate() {
            prediction = smlawb(prediction, state[MOST_ORDER + i - back - 1], i32::from(coefficient));
        }
        state[MOST_ORDER + i] = state[MOST_ORDER + i].wrapping_add(prediction << LPC_SHIFT);
        *sample = sat16(rshift_round(smulww(state[MOST_ORDER + i], gain_q10), OUT_SHIFT));
    }
}

// Reconstructs a frame's excitation (4.2.7.8.6) and runs it through the LTP and LPC synthesis filters (4.2.7.9), as silk_decode_core does in fixed point.
pub(super) fn synthesize(
    channel: &mut Channel,
    layout: Layout,
    indices: &Indices,
    params: &mut Params,
    pulses: &[i32; MOST_FRAME],
    out: &mut [i16],
) {
    let offset = EXCITATION_OFFSETS[indices.signal as usize][indices.offset];
    let mut seed = indices.seed;
    for (excitation, &pulse) in channel.excitation.iter_mut().zip(&pulses[..layout.frame()]) {
        seed = lcg(seed);
        let value = ((pulse << PULSE_SHIFT) - pulse.signum() * PULSE_ADJUST + offset) << Q23_TO_Q14;
        *excitation = if seed < 0 { -value } else { value };
        seed = seed.wrapping_add(pulse);
    }
    let (order, subframe, history) = (layout.order(), layout.subframe(), layout.history());
    let (least, most) = layout.lags();
    let mut lpc = [0; MOST_SUBFRAME + MOST_ORDER];
    lpc[..MOST_ORDER].copy_from_slice(&channel.lpc);
    let mut whitened = [0; MOST_FRAME];
    let mut ltp = [0; 2 * MOST_FRAME];
    let mut residual = [0; MOST_SUBFRAME];
    let mut at = history;
    for k in 0..layout.subframes {
        let coefficients = params.lpc[k >> 1];
        let coefficients = &coefficients[..order];
        let gain = params.gains[k];
        let mut inverse = inverse32_varq(gain, INVERSE_Q);
        let adjust = if gain != channel.prev_gain {
            let adjust = div32_varq(channel.prev_gain, gain, ADJUST_Q);
            for state in &mut lpc[..MOST_ORDER] {
                *state = smulww(adjust, *state);
            }
            adjust
        } else {
            UNIT_GAIN_Q16
        };
        channel.prev_gain = gain;
        let mut signal = indices.signal;
        if channel.losses > 0
            && channel.signal == Signal::Voiced
            && indices.signal != Signal::Voiced
            && k < MOST_SUBFRAMES / 2
        {
            params.taps[k] = [0; LTP_TAPS];
            params.taps[k][LTP_HALF] = LOSS_TAP;
            signal = Signal::Voiced;
            params.pitch[k] = channel.lag;
        }
        let excitation = &channel.excitation[k * subframe..][..subframe];
        let source: &[i32] = if signal == Signal::Voiced {
            let lag = params.pitch[k].clamp(least, most) as usize;
            if k == 0 || (k == 2 && params.is_interpolated) {
                let start = history - lag - order - LTP_HALF;
                if k == 2 {
                    channel.history[history..][..2 * subframe].copy_from_slice(&out[..2 * subframe]);
                }
                whiten(
                    &mut whitened[start..history],
                    &channel.history[start + k * subframe..][..history - start],
                    coefficients,
                );
                if k == 0 {
                    inverse = smulwb(inverse, params.ltp_scale) << LTP_SCALE_SHIFT;
                }
                for back in 1..=lag + LTP_HALF {
                    ltp[at - back] = smulwb(inverse, i32::from(whitened[history - back]));
                }
            } else if adjust != UNIT_GAIN_Q16 {
                for state in &mut ltp[at - lag - LTP_HALF..at] {
                    *state = smulww(adjust, *state);
                }
            }
            let taps = params.taps[k];
            for (value, &excitation) in residual[..subframe].iter_mut().zip(excitation) {
                *value = excitation.wrapping_add(predict(&ltp, at, lag, &taps) << LTP_SHIFT);
                ltp[at] = *value << LTP_SHIFT;
                at += 1;
            }
            &residual[..subframe]
        } else {
            excitation
        };
        lpc[MOST_ORDER..][..subframe].copy_from_slice(source);
        let samples = &mut out[k * subframe..][..subframe];
        filter(&mut lpc[..MOST_ORDER + subframe], coefficients, gain >> GAIN_TO_Q10, samples);
        lpc.copy_within(subframe..subframe + MOST_ORDER, 0);
    }
    channel.lpc.copy_from_slice(&lpc[..MOST_ORDER]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitening_residual() {
        let input = [100i16, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200];
        let lpc = [4096i16, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut out = [7i16; 12];
        whiten(&mut out, &input, &lpc);
        assert_eq!(
            out,
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 100],
            "a filter of one tap of 1.0 leaves the step between samples"
        );
        let mut out = [0i16; 12];
        whiten(&mut out, &[i16::MAX; 12], &[-4096, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(out[10..], [i16::MAX; 2], "and a residual past 16 bits saturates");
    }
}
