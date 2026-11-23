use super::fixed::{log2lin, smulwb};
use super::indices::{Indices, Signal, WHOLE_INTERPOLATION};
use super::lpc;
use super::tables::{
    LTP_SCALES, LTP_TAPS, OFFSETS, OFFSETS_NB, OFFSETS_NB_SHORT, OFFSETS_SHORT, TAPS_0, TAPS_1, TAPS_2,
};
use super::{Channel, Condition, Layout, MOST_ORDER, MOST_SUBFRAMES, NARROW};

// 4.2.7.4: an independent gain index falls by at most 16; one coded as a delta is the larger of 2 * delta - 16 and last + delta - 4; an index lies in 0 to 63 and gives the log gain 0x1D1C71 * index >> 16 plus 2090 in Q7.
const GAIN_FALL: i32 = 16;
const DELTA_OFFSET: i32 = 4;
const DOUBLED_OFFSET: i32 = 16;
const GAIN_LEVELS: i32 = 64;
const GAIN_SCALE: i32 = 0x1D_1C71;
const GAIN_LOG_OFFSET: i32 = 2090;
// 4.2.7.5.5: the interpolation factor is in Q2.
const INTERPOLATION_Q: u32 = 2;
// 4.2.7.6.2: LTP taps of Q7 in Q14.
const TAP_SHIFT: u32 = 7;
// The chirp of the LPC filters of the first frame after a loss (silk/define.h, BWE_AFTER_LOSS_Q16).
const LOSS_CHIRP: i32 = 63_570;

// What a frame's synthesis runs by: the gains in Q16, the LPC filters of the first and the second half in Q12, the pitch lags and the LTP taps in Q14 of each subframe, and the LTP scaling in Q14.
#[derive(Clone, Copy, Default)]
pub(super) struct Params {
    pub(super) gains: [i32; MOST_SUBFRAMES],
    pub(super) lpc: [[i16; MOST_ORDER]; 2],
    pub(super) is_interpolated: bool,
    pub(super) pitch: [i32; MOST_SUBFRAMES],
    pub(super) taps: [[i16; LTP_TAPS]; MOST_SUBFRAMES],
    pub(super) ltp_scale: i32,
}

pub(super) fn decode(channel: &mut Channel, layout: Layout, indices: &Indices, condition: Condition) -> Params {
    let mut params = Params::default();
    let subframes = layout.subframes;
    let is_conditional = condition == Condition::Conditional;
    for (subframe, (&index, gain)) in indices.gains[..subframes].iter().zip(&mut params.gains).enumerate() {
        channel.gain_index = log_gain(index, channel.gain_index, subframe == 0 && !is_conditional);
        *gain = log2lin(smulwb(GAIN_SCALE, channel.gain_index) + GAIN_LOG_OFFSET);
    }
    let order = layout.order();
    let nlsf = lpc::decode(indices.stage1, &indices.residuals[..order], layout.is_wide());
    params.lpc[1] = lpc::to_lpc(&nlsf[..order]);
    let weight = if channel.is_reset { WHOLE_INTERPOLATION } else { indices.interpolation };
    params.is_interpolated = weight < WHOLE_INTERPOLATION;
    params.lpc[0] = if params.is_interpolated {
        let mut mixed = [0; MOST_ORDER];
        for ((value, &now), &before) in mixed.iter_mut().zip(&nlsf[..order]).zip(&channel.nlsf) {
            *value = (i32::from(before) + ((weight * (i32::from(now) - i32::from(before))) >> INTERPOLATION_Q)) as i16;
        }
        lpc::to_lpc(&mixed[..order])
    } else {
        params.lpc[1]
    };
    channel.nlsf = nlsf;
    if channel.losses > 0 {
        for filter in &mut params.lpc {
            lpc::expand(&mut filter[..order], LOSS_CHIRP);
        }
    }
    if indices.signal == Signal::Voiced {
        params.pitch = lags(layout, indices.lag, indices.contour);
        let taps: &[[i8; LTP_TAPS]] = [&TAPS_0[..], &TAPS_1, &TAPS_2][indices.periodicity];
        for (filter, &index) in params.taps.iter_mut().zip(&indices.filters[..subframes]) {
            *filter = taps[index].map(|tap| i16::from(tap) << TAP_SHIFT);
        }
        params.ltp_scale = LTP_SCALES[indices.scaling];
    }
    params
}

// A subframe's gain index of its symbol (4.2.7.4).
fn log_gain(index: i32, last: i32, is_independent: bool) -> i32 {
    let gain = if is_independent {
        index.max(last - GAIN_FALL)
    } else {
        (2 * index - DOUBLED_OFFSET).max(last + index - DELTA_OFFSET)
    };
    gain.clamp(0, GAIN_LEVELS - 1)
}

// The pitch lag of each subframe (4.2.7.6.1).
fn lags(layout: Layout, lag: i32, contour: usize) -> [i32; MOST_SUBFRAMES] {
    let (least, most) = layout.lags();
    let offsets: &[i8] = match (layout.band, layout.subframes) {
        (NARROW, MOST_SUBFRAMES) => &OFFSETS_NB[contour],
        (NARROW, _) => &OFFSETS_NB_SHORT[contour],
        (_, MOST_SUBFRAMES) => &OFFSETS[contour],
        _ => &OFFSETS_SHORT[contour],
    };
    let mut lags = [0; MOST_SUBFRAMES];
    for (pitch, &offset) in lags.iter_mut().zip(offsets) {
        *pitch = (least + lag + i32::from(offset)).clamp(least, most);
    }
    lags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain(log_gain: i32) -> i32 {
        log2lin(smulwb(GAIN_SCALE, log_gain) + GAIN_LOG_OFFSET)
    }

    #[test]
    fn gain_end_points() {
        assert_eq!(gain(0), 81_920, "the lowest gain of 4.2.7.4, 1.25 in Q16");
        assert_eq!(gain(GAIN_LEVELS - 1), 1_686_110_208, "and the highest, 25728");
        assert!((1..GAIN_LEVELS).all(|index| gain(index) > gain(index - 1)), "gains rise with their index");
    }

    #[test]
    fn gain_indices() {
        assert_eq!(log_gain(5, 30, true), 14, "an independent gain falls by no more than 16");
        assert_eq!(log_gain(40, 30, true), 40);
        assert_eq!(log_gain(4, 20, false), 20, "a delta of 4 keeps the last index");
        assert_eq!(log_gain(0, 20, false), 16, "and one of 0 falls by 4");
        assert_eq!(log_gain(13, 20, false), 29, "up to 12 above the last a delta counts once");
        assert_eq!(log_gain(36, 20, false), 56, "past it double: 2 * 36 - 16");
        assert_eq!(log_gain(40, 50, false), 63, "an index stays at most 63");
        assert_eq!(log_gain(0, 2, false), 0, "and at least 0");
    }

    #[test]
    fn lags_by_contour() {
        let wide = Layout { band: super::super::WIDE, subframes: MOST_SUBFRAMES };
        assert_eq!(lags(wide, 100, 33), [132 - 9, 132 - 3, 132 + 3, 132 + 9], "Table 36's last vector about 32 + 100");
        assert_eq!(lags(wide, 0, 33), [32, 32, 35, 41], "lags no shorter than 2 ms");
        assert_eq!(lags(wide, 255, 32), [288, 288, 285, 280], "and no longer than 18 ms");
        let narrow = Layout { band: NARROW, subframes: 2 };
        assert_eq!(lags(narrow, 10, 1), [27, 26, 0, 0], "Table 33 at NB in 10 ms, two subframes");
    }
}
