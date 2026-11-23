use super::super::range::RangeDecoder;
use super::tables::{
    ACTIVE_TYPE, ACTIVE_TYPE_OFFSET, CONTOUR, CONTOUR_NB, CONTOUR_NB_SHORT, CONTOUR_SHORT, FILTER_0, FILTER_1,
    FILTER_2, GAIN_DELTA, GAIN_HIGH, GAIN_LOW, INACTIVE_TYPE, LAG_DELTA, LAG_HIGH, LAG_LOW_MB, LAG_LOW_NB, LAG_LOW_WB,
    LSF_EXTENSION, LSF_INTERPOLATION, LSF_NB_FIRST_BOOK, LSF_NB_SELECT, LSF_NB_STAGE2, LSF_STAGE1, LSF_WB_FIRST_BOOK,
    LSF_WB_SELECT, LSF_WB_STAGE2, LTP_SCALING, MID_ONLY, PDF_BITS, PERIODICITY, SEED, STEREO_STAGE1, STEREO_STAGE2,
    STEREO_STAGE3,
};
use super::{Condition, Layout, MOST_ORDER, MOST_SUBFRAMES, NARROW, Parse};

// The 3 low bits of an independent gain index (4.2.7.4).
const GAIN_LOW_BITS: u32 = 3;
// A stage-2 index runs from -4 to 4, and the extension carries it on from either end (4.2.7.5.2).
const STAGE2_REACH: i32 = 4;
// The LSF interpolation factor of a frame that takes its own LSFs whole (4.2.7.5.5).
pub(super) const WHOLE_INTERPOLATION: i32 = 4;
// A lag change of 0 falls back to absolute coding; any other is told 9 up (4.2.7.6.1).
const LAG_DELTA_ZERO: i32 = 9;

// The signal type of a frame (Table 10).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Signal {
    #[default]
    Inactive,
    Unvoiced,
    Voiced,
}

const SIGNALS: [Signal; 3] = [Signal::Inactive, Signal::Unvoiced, Signal::Voiced];

// The symbols of a SILK frame up to its excitation (4.2.7.3 to 4.2.7.7).
pub(super) struct Indices {
    pub(super) signal: Signal,
    pub(super) offset: usize,
    pub(super) gains: [i32; MOST_SUBFRAMES],
    pub(super) stage1: usize,
    pub(super) residuals: [i32; MOST_ORDER],
    pub(super) interpolation: i32,
    pub(super) lag: i32,
    pub(super) contour: usize,
    pub(super) periodicity: usize,
    pub(super) filters: [usize; MOST_SUBFRAMES],
    pub(super) scaling: usize,
    pub(super) seed: i32,
}

// Reads the stereo prediction weights (4.2.7.1), which a decoder of one channel of output has no use for.
pub(super) fn skip_weights(range: &mut RangeDecoder) {
    range.icdf(&STEREO_STAGE1, PDF_BITS);
    // i0 and i1 of the first weight, then i2 and i3 of the second.
    for _ in 0..2 {
        range.icdf(&STEREO_STAGE2, PDF_BITS);
        range.icdf(&STEREO_STAGE3, PDF_BITS);
    }
}

// 4.2.7.2
pub(super) fn read_mid_only(range: &mut RangeDecoder) -> bool {
    range.icdf(&MID_ONLY, PDF_BITS) == 1
}

pub(super) fn read(
    range: &mut RangeDecoder,
    parse: &mut Parse,
    layout: Layout,
    index: usize,
    is_lbrr: bool,
    condition: Condition,
) -> Indices {
    let kind = if is_lbrr || parse.flags.vad[index] {
        range.icdf(&ACTIVE_TYPE, PDF_BITS) + ACTIVE_TYPE_OFFSET
    } else {
        range.icdf(&INACTIVE_TYPE, PDF_BITS)
    };
    let (signal, offset) = (SIGNALS[kind >> 1], kind & 1);
    let mut gains = [0; MOST_SUBFRAMES];
    for (subframe, gain) in gains[..layout.subframes].iter_mut().enumerate() {
        *gain = if subframe == 0 && condition != Condition::Conditional {
            let high = range.icdf(&GAIN_HIGH[signal as usize], PDF_BITS);
            (high << GAIN_LOW_BITS | range.icdf(&GAIN_LOW, PDF_BITS)) as i32
        } else {
            range.icdf(&GAIN_DELTA, PDF_BITS) as i32
        };
    }
    let is_voiced = signal == Signal::Voiced;
    // Table 14: inactive or unvoiced, then voiced, for NB and MB and then for WB.
    let book = 2 * usize::from(layout.is_wide()) + usize::from(is_voiced);
    let stage1 = range.icdf(&LSF_STAGE1[book], PDF_BITS);
    let mut residuals = [0; MOST_ORDER];
    for (coefficient, residual) in residuals[..layout.order()].iter_mut().enumerate() {
        let pdf = if layout.is_wide() {
            &LSF_WB_STAGE2[usize::from(LSF_WB_SELECT[stage1][coefficient] - LSF_WB_FIRST_BOOK)]
        } else {
            &LSF_NB_STAGE2[usize::from(LSF_NB_SELECT[stage1][coefficient] - LSF_NB_FIRST_BOOK)]
        };
        let value = range.icdf(pdf, PDF_BITS) as i32 - STAGE2_REACH;
        *residual = match value {
            STAGE2_REACH => value + range.icdf(&LSF_EXTENSION, PDF_BITS) as i32,
            reach if reach == -STAGE2_REACH => value - range.icdf(&LSF_EXTENSION, PDF_BITS) as i32,
            _ => value,
        };
    }
    let interpolation = if layout.subframes == MOST_SUBFRAMES {
        range.icdf(&LSF_INTERPOLATION, PDF_BITS) as i32
    } else {
        WHOLE_INTERPOLATION
    };
    let mut indices = Indices {
        signal,
        offset,
        gains,
        stage1,
        residuals,
        interpolation,
        lag: 0,
        contour: 0,
        periodicity: 0,
        filters: [0; MOST_SUBFRAMES],
        scaling: 0,
        seed: 0,
    };
    if is_voiced {
        read_ltp(range, parse, layout, condition, &mut indices);
    }
    parse.signal = signal;
    indices.seed = range.icdf(&SEED, PDF_BITS) as i32;
    indices
}

// The pitch lag, its contour, the LTP filters and their scaling (4.2.7.6).
fn read_ltp(range: &mut RangeDecoder, parse: &mut Parse, layout: Layout, condition: Condition, indices: &mut Indices) {
    let mut lag = None;
    if condition == Condition::Conditional && parse.signal == Signal::Voiced {
        let delta = range.icdf(&LAG_DELTA, PDF_BITS) as i32;
        if delta > 0 {
            lag = Some(parse.lag + delta - LAG_DELTA_ZERO);
        }
    }
    // Table 30: the high part of a lag counts in steps of as many values as the low part has.
    let low: &[u8] = [&LAG_LOW_NB[..], &LAG_LOW_MB, &LAG_LOW_WB][layout.band];
    indices.lag = lag.unwrap_or_else(|| {
        let high = range.icdf(&LAG_HIGH, PDF_BITS) * low.len();
        (high + range.icdf(low, PDF_BITS)) as i32
    });
    parse.lag = indices.lag;
    let contour: &[u8] = match (layout.band, layout.subframes) {
        (NARROW, MOST_SUBFRAMES) => &CONTOUR_NB,
        (NARROW, _) => &CONTOUR_NB_SHORT,
        (_, MOST_SUBFRAMES) => &CONTOUR,
        _ => &CONTOUR_SHORT,
    };
    indices.contour = range.icdf(contour, PDF_BITS);
    indices.periodicity = range.icdf(&PERIODICITY, PDF_BITS);
    let filter: &[u8] = [&FILTER_0[..], &FILTER_1, &FILTER_2][indices.periodicity];
    for index in &mut indices.filters[..layout.subframes] {
        *index = range.icdf(filter, PDF_BITS);
    }
    if condition == Condition::Independent {
        indices.scaling = range.icdf(&LTP_SCALING, PDF_BITS);
    }
}
