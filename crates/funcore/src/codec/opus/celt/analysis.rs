// What an encoder finds in a frame's sound to choose what it codes: a tone, a transient, the resolution of each band in time and frequency, the boosts, the spreading and the trim (celt_encoder.c, bands.c).

use super::allocation::BIT;
use super::bands::haar;
use super::tables::{E_BANDS, E_MEANS, LOG_N, TF_SELECT};
use super::{BANDS, FRAME, OVERLAP, SHORT, SPREAD_NORMAL, WIDEST};
use crate::base::math::{atan2_turns, dot, floor, pow2_f64, sqrt};
use core::f32::consts::PI;
use core::f64::consts::TAU;

// The tone detection fits a two-pole predictor at a delay doubled while it fails or resonates near DC and is no more than a third of a millisecond at 48 kHz, and finds a tone where its poles are complex; a hair under 4 keeps a double pole from counting (tone_detect).
const TONE_MOST_DELAY: usize = 16;
const SINGULAR: f32 = 0.001;
const POLE_BOUND: f32 = 1.999_999;
const DISCRIMINANT: f32 = 3.999_999;
const NO_TONE: Tone = Tone { frequency: -1.0, toneishness: 0.0 };
// The transient detection: its high-pass filter's first samples, which it does not trust; its forward masking of 6.7 dB/ms and backward masking of 13.9 dB/ms; the mask metric above which a frame is transient; a tone of this purity and lower frequency, which is no transient; and how its metric maps to the tf estimate (transient_analysis).
const TRANSIENT_SKIP: usize = 12;
const FORWARD_DECAY: f32 = 0.0625;
const BACKWARD_KEEP: f32 = 0.875;
const BACKWARD_TAKE: f32 = 0.125;
const TRANSIENT_METRIC: i32 = 200;
const TONE_PURITY: f32 = 0.98;
const TONE_LOWEST: f32 = 0.026;
const METRIC_SCALE: f32 = 27.0;
const METRIC_FLOOR: f32 = 42.0;
const METRIC_MOST: f32 = 163.0;
const ESTIMATE_SLOPE: f32 = 0.0069;
const ESTIMATE_OFFSET: f32 = 0.139;
const EPSILON: f32 = 1.0e-15;
// The masked energy is sampled every 4th value from the skip to 5 before the half's end, in 64 steps of its mean's inverse, which the table holds 6 times the inverse of (transient_analysis).
const MASK_STRIDE: usize = 4;
const MASK_TAIL: usize = 5;
const MASK_STEPS: f32 = 64.0;
const TABLE_SCALE: i32 = 6;
// 6*64/x for each step of the masked energy, trained on real data to minimise the average error (transient_analysis).
#[rustfmt::skip]
const INVERSE: [u8; 128] = [
    255, 255, 156, 110, 86, 70, 59, 51, 45, 40, 37, 33, 31, 28, 26, 25,
    23, 22, 21, 20, 19, 18, 17, 16, 16, 15, 15, 14, 13, 13, 12, 12,
    12, 12, 11, 11, 11, 10, 10, 10, 9, 9, 9, 9, 9, 9, 8, 8,
    8, 8, 8, 7, 7, 7, 7, 7, 7, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2,
];
// A band whose energy rose more than a step of the coarse energy, on average, over the frame before spread by a step a band either way, makes the frame transient after all (patch_transient_decision).
const RISE: f32 = 1.0;
const FIRST_RISE_BAND: usize = 2;
// The tf analysis's bias toward resolution in frequency (tf_analysis).
const TF_BIAS: f32 = 0.04;
const TF_BIAS_FROM: f32 = 0.5;
const TF_BIAS_LEAST: f32 = -0.25;
// The boosts: the noise floor by each band's width, the pre-emphasis and the depth of 16-bit samples; the masking slopes in steps of the energy a band up and down; the signal to mask ratio's shift of the spread weights; the bytes from which boosts are made; the median filter's offset; the follower's slopes; each band's most; the share of the frame boosts may take at a constant rate (dynalloc_analysis).
const FLOOR_WIDTH: f32 = 0.0625;
const FLOOR_OFFSET: f32 = 0.5;
const FLOOR_DEPTH: i32 = 9;
const SAMPLE_DEPTH: i32 = 16;
const FLOOR_EMPHASIS: f32 = 0.0062;
const FLOOR_EMPHASIS_BAND: usize = 5;
const DEPTH_LEAST: f32 = -31.9;
const DEPTH_RANGE: f32 = 12.0;
const MASK_UP: f32 = 2.0;
const MASK_DOWN: f32 = 3.0;
const WEIGHT_FULL: i32 = 32;
const WEIGHT_MOST_SHIFT: i32 = 5;
const BOOST_LEAST_BYTES: i32 = 30;
const BOOST_BYTES_PER_LM: i32 = 5;
const FOLLOW_RISE: f32 = 0.5;
const FOLLOW_UP: f32 = 1.5;
const FOLLOW_DOWN: f32 = 2.0;
const MEDIAN_OFFSET: f32 = 1.0;
const IMPORTANCE_SCALE: f32 = 13.0;
const IMPORTANCE_DEFAULT: i32 = 13;
const FOLLOW_MOST: f32 = 4.0;
const LOW_BANDS: usize = 8;
const HIGH_BANDS: usize = 12;
const NARROW_BOOST: usize = 6;
const WIDE_BOOST: usize = 48;
const BOOST_STEPS: f32 = 8.0;
// Boosts of a tone's bands, the nearest bands most, where the tone is this pure (dynalloc_analysis).
const TONE_BOOST_PURITY: f32 = 0.98;
const TONE_BOOSTS: [(usize, f32); 4] = [(0, 2.0), (1, 1.0), (2, 1.0), (3, 0.5)];
// The spreading decision's thresholds of a bin's share of its band's energy, the bands it counts the high frequencies by, and its hysteresis (spreading_decision).
const SPREAD_LEAST_BINS: usize = 8;
const SPREAD_SHARES: [f32; 3] = [0.25, 0.0625, 0.015_625];
const HIGH_BANDS_COUNTED: usize = 4;
const TAPSET_STEP: i32 = 4;
const TAPSET_THRESHOLDS: [i32; 2] = [18, 22];
const SPREAD_THRESHOLDS: [i32; 3] = [80, 256, 384];
const AVERAGE_START: i32 = 256;
// The decision's average is in Q8 of a band's level, and leans a quarter of the way to the middle of the last decision's range, each 128 wide; the high bands count in 32nds (spreading_decision).
const AVERAGE_SHIFT: i32 = 8;
const RANGE_SHIFT: i32 = 7;
const HIGH_SCALE: usize = 32;
// The trim: 4 below 64 kb/s and rising to 5 at 80 kb/s, tilted by the spectrum and the tf estimate (alloc_trim_analysis).
const TRIM_LOW: f32 = 4.0;
const TRIM_HIGH: f32 = 5.0;
const TRIM_HIGH_RATE: i32 = 80_000;
const TRIM_LOW_RATE: i32 = 64_000;
const TRIM_RATE_SHIFT: i32 = 10;
const TRIM_RATE_STEP: f32 = 1.0 / 16.0;
const TRIM_TILT_MOST: f32 = 2.0;
const TRIM_TILT_SCALE: f32 = 6.0;
const TRIM_MOST: i32 = 10;

// What the tone detection finds of a frame: the frequency in radians a sample of the one tone that fills it, -1 for none, and how nearly the frame is that tone, 0 to 1.
#[derive(Clone, Copy)]
pub(super) struct Tone {
    pub(super) frequency: f32,
    pub(super) toneishness: f32,
}

// A two-pole predictor of x at `delay`, fitted forward and backward by least squares; none where that is singular (tone_lpc).
fn predictor(x: &[f32], delay: usize) -> Option<[f32; 2]> {
    let len = x.len();
    let head = &x[..len - 2 * delay];
    let (r00, r01, r02) = (dot(head, head), dot(head, &x[delay..]), dot(head, &x[2 * delay..]));
    let (mut first, mut second, mut cross) = (0.0f32, 0.0f32, 0.0f32);
    for at in 0..delay {
        first += x[len + at - 2 * delay] * x[len + at - 2 * delay] - x[at] * x[at];
        second += x[len + at - delay] * x[len + at - delay] - x[at + delay] * x[at + delay];
        cross += x[len + at - 2 * delay] * x[len + at - delay] - x[at] * x[at + delay];
    }
    let r11 = r00 + first;
    let r22 = r11 + second;
    let r12 = r01 + cross;
    let (r00, r01, r11, r02, r12) = (r00 + r22, r01 + r12, 2.0 * r11, 2.0 * r02, r12 + r01);
    let den = r00 * r11 - r01 * r01;
    if den < SINGULAR * (r00 * r11) {
        return None;
    }
    let num1 = r02 * r11 - r01 * r12;
    let second = if num1 >= den {
        1.0
    } else if num1 <= -den {
        -1.0
    } else {
        num1 / den
    };
    let num0 = r00 * r12 - r02 * r01;
    let first = if 0.5 * num0 >= den {
        POLE_BOUND
    } else if 0.5 * num0 <= -den {
        -POLE_BOUND
    } else {
        num0 / den
    };
    Some([first, second])
}

// The one tone a frame's samples hold, if they hold one (tone_detect).
pub(super) fn tone(x: &[f32]) -> Tone {
    let mut delay = 1;
    let mut fit = predictor(x, delay);
    while delay <= TONE_MOST_DELAY && fit.is_none_or(|[first, second]| first > 1.0 && second < 0.0) {
        delay *= 2;
        fit = predictor(x, delay);
    }
    match fit {
        Some([first, second]) if first * first + DISCRIMINANT * second < 0.0 => {
            let cosine = f64::from(0.5 * first);
            let sine = f64::from(sqrt((1.0 - cosine * cosine).max(0.0) as f32));
            let angle = TAU * atan2_turns(sine, cosine);
            Tone { frequency: (angle / delay as f64) as f32, toneishness: -second }
        }
        _ => NO_TONE,
    }
}

// Whether a frame's samples, the overlap before it first, hold a transient, and the tf estimate: how much a transient wants resolution in time, 0 to 1 (transient_analysis).
pub(super) fn transient(x: &[f32], tone: Tone) -> (bool, f32) {
    let len = x.len();
    let half = len / 2;
    let mut filtered = [0.0f32; FRAME + OVERLAP];
    let (mut mem0, mut mem1) = (0.0f32, 0.0f32);
    for (out, &sample) in filtered.iter_mut().zip(x) {
        // (1 - 2/z + 1/z^2) / (1 - 1/z + 1/(2 z^2)), as libopus runs it.
        let y = mem0 + sample;
        let before = mem0;
        mem0 = mem0 - sample + 0.5 * mem1;
        mem1 = sample - before;
        *out = y;
    }
    filtered[..TRANSIENT_SKIP].fill(0.0);
    let pairs = &filtered[..2 * half];
    let mean = dot(pairs, pairs);
    let mut memory = 0.0f32;
    for at in 0..half {
        let energy = filtered[2 * at] * filtered[2 * at] + filtered[2 * at + 1] * filtered[2 * at + 1];
        memory = energy + (1.0 - FORWARD_DECAY) * memory;
        filtered[at] = FORWARD_DECAY * memory;
    }
    let (mut memory, mut most) = (0.0f32, 0.0f32);
    for value in filtered[..half].iter_mut().rev() {
        memory = *value + BACKWARD_KEEP * memory;
        *value = BACKWARD_TAKE * memory;
        most = most.max(BACKWARD_TAKE * memory);
    }
    let mean = sqrt(mean * most * 0.5 * half as f32);
    let norm = half as f32 / (EPSILON + mean);
    let last_step = (INVERSE.len() - 1) as f32;
    let unmask: i32 = (TRANSIENT_SKIP..half - MASK_TAIL)
        .step_by(MASK_STRIDE)
        .map(|at| {
            i32::from(INVERSE[floor(MASK_STEPS * norm * (filtered[at] + EPSILON)).clamp(0.0, last_step) as usize])
        })
        .sum();
    let span = (half - TRANSIENT_SKIP - MASK_TAIL) as i32;
    let mut metric = MASK_STEPS as i32 * unmask * MASK_STRIDE as i32 / (TABLE_SCALE * span);
    let mut is_transient = metric > TRANSIENT_METRIC;
    if tone.toneishness > TONE_PURITY && tone.frequency < TONE_LOWEST {
        is_transient = false;
        metric = 0;
    }
    let tf_max = (sqrt(METRIC_SCALE * metric as f32) - METRIC_FLOOR).max(0.0);
    (is_transient, sqrt((ESTIMATE_SLOPE * tf_max.min(METRIC_MOST) - ESTIMATE_OFFSET).max(0.0)))
}

// Whether the bands' energies rose so much over the frame before's that the frame is transient after all (patch_transient_decision).
pub(super) fn has_risen(energies: &[f32; BANDS], before: &[f32; BANDS], end: usize) -> bool {
    let mut spread = *before;
    for band in 1..end {
        spread[band] = (spread[band - 1] - RISE).max(before[band]);
    }
    for band in (0..end - 1).rev() {
        spread[band] = spread[band].max(spread[band + 1] - RISE);
    }
    let rise: f32 =
        (FIRST_RISE_BAND..end - 1).map(|band| (energies[band].max(0.0) - spread[band].max(0.0)).max(0.0)).sum();
    rise / (end - 1 - FIRST_RISE_BAND) as f32 > RISE
}

// A band's L1 norm, made larger the more it is split, so that resolution in frequency wins where they tie (l1_metric).
fn spread_norm(x: &[f32], splits: usize, bias: f32) -> f32 {
    let norm: f32 = x.iter().map(|value| value.abs()).sum();
    norm + splits as f32 * bias * norm
}

// Each band's change of resolution flag, 0 or 1, and tf_select: the path through the bands of least cost in importance-weighted distance from each band's best resolution and `lambda` for each change (tf_analysis).
pub(super) fn tf(
    x: &[f32],
    end: usize,
    lm: usize,
    is_transient: bool,
    tf_estimate: f32,
    lambda: i32,
    importance: &[i32; BANDS],
) -> ([usize; BANDS], bool) {
    let bias = TF_BIAS * (TF_BIAS_FROM - tf_estimate).max(TF_BIAS_LEAST);
    let mut metric = [0i32; BANDS];
    let (mut work, mut split) = ([0.0f32; WIDEST], [0.0f32; WIDEST]);
    for band in 0..end {
        let width = E_BANDS[band + 1] - E_BANDS[band];
        let n = width << lm;
        let is_narrow = width == 1;
        let work = &mut work[..n];
        work.copy_from_slice(&x[E_BANDS[band] << lm..][..n]);
        let mut best = spread_norm(work, if is_transient { lm } else { 0 }, bias);
        let mut level = 0i32;
        if is_transient && !is_narrow {
            let split = &mut split[..n];
            split.copy_from_slice(work);
            haar(split, n >> lm, 1 << lm);
            let norm = spread_norm(split, lm + 1, bias);
            if norm < best {
                (best, level) = (norm, -1);
            }
        }
        for k in 0..lm + usize::from(!(is_transient || is_narrow)) {
            let splits = if is_transient { lm - k - 1 } else { k + 1 };
            haar(work, n >> k, 1 << k);
            let norm = spread_norm(work, splits, bias);
            if norm < best {
                (best, level) = (norm, k as i32 + 1);
            }
        }
        metric[band] = if is_transient { 2 * level } else { -2 * level };
        if is_narrow && (metric[band] == 0 || metric[band] == -2 * lm as i32) {
            metric[band] -= 1;
        }
    }
    let row = &TF_SELECT[lm];
    let base = 4 * usize::from(is_transient);
    let cost = |select: usize, band: usize, flag: usize| {
        importance[band] * (metric[band] - 2 * i32::from(row[base + 2 * select + flag])).abs()
    };
    let first_change = if is_transient { 0 } else { lambda };
    let mut costs = [0; 2];
    for (select, total) in costs.iter_mut().enumerate() {
        let (mut kept, mut changed) = (cost(select, 0, 0), cost(select, 0, 1) + first_change);
        for band in 1..end {
            let (to_kept, to_changed) = (kept.min(changed + lambda), (kept + lambda).min(changed));
            (kept, changed) = (to_kept + cost(select, band, 0), to_changed + cost(select, band, 1));
        }
        *total = kept.min(changed);
    }
    // tf_select is chosen for transients alone, conservatively.
    let select = usize::from(costs[1] < costs[0] && is_transient);
    // Each band's flag the band before had on the cheapest way to it, of each flag it may have.
    let mut paths = [[0usize; 2]; BANDS];
    let (mut kept, mut changed) = (cost(select, 0, 0), cost(select, 0, 1) + first_change);
    for (band, path) in paths.iter_mut().enumerate().take(end).skip(1) {
        let (from_kept, from_changed) = (kept, changed + lambda);
        path[0] = usize::from(from_kept >= from_changed);
        let to_kept = from_kept.min(from_changed);
        let (from_kept, from_changed) = (kept + lambda, changed);
        path[1] = usize::from(from_kept >= from_changed);
        let to_changed = from_kept.min(from_changed);
        (kept, changed) = (to_kept + cost(select, band, 0), to_changed + cost(select, band, 1));
    }
    let mut flags = [0usize; BANDS];
    flags[end - 1] = usize::from(kept >= changed);
    for band in (0..end - 1).rev() {
        flags[band] = paths[band + 1][flags[band + 1]];
    }
    (flags, select == 1)
}

// The boosts of the dynamic allocation a frame wants, in quanta, each band's importance to the tf analysis and its weight to the spreading decision (dynalloc_analysis).
pub(super) struct Boosts {
    pub(super) offsets: [i32; BANDS],
    pub(super) importance: [i32; BANDS],
    pub(super) spread_weight: [i32; BANDS],
}

fn median_of_5(x: &[f32]) -> f32 {
    let (t0, mut t1) = if x[0] > x[1] { (x[1], x[0]) } else { (x[0], x[1]) };
    let t2 = x[2];
    let (mut t3, mut t4) = if x[3] > x[4] { (x[4], x[3]) } else { (x[3], x[4]) };
    if t0 > t3 {
        t3 = t0;
        (t1, t4) = (t4, t1);
    }
    if t2 > t1 {
        if t1 < t3 { t2.min(t3) } else { t4.min(t1) }
    } else if t2 < t3 {
        t1.min(t3)
    } else {
        t2.min(t4)
    }
}

fn median_of_3(x: &[f32]) -> f32 {
    let (t0, t1) = if x[0] > x[1] { (x[1], x[0]) } else { (x[0], x[1]) };
    let t2 = x[2];
    if t1 < t2 {
        t1
    } else if t0 < t2 {
        t2
    } else {
        t0
    }
}

// The boosts a frame of one channel at a constant rate wants for the bands that stand out of a smoothed envelope of the spectrum: `energies` its bands', `long` those of its long block, `bytes` its size (dynalloc_analysis).
pub(super) fn boosts(
    energies: &[f32; BANDS],
    long: &[f32; BANDS],
    end: usize,
    lm: usize,
    is_transient: bool,
    bytes: i32,
    tone: Tone,
) -> Boosts {
    let floor_of = |band: usize| {
        let emphasis = ((band + FLOOR_EMPHASIS_BAND) * (band + FLOOR_EMPHASIS_BAND)) as f32;
        FLOOR_WIDTH * LOG_N[band] as f32 + FLOOR_OFFSET + (FLOOR_DEPTH - SAMPLE_DEPTH) as f32 - E_MEANS[band]
            + FLOOR_EMPHASIS * emphasis
    };
    let noise_floor: [f32; BANDS] = core::array::from_fn(floor_of);
    let depth = (0..end).map(|band| energies[band] - noise_floor[band]).fold(DEPTH_LEAST, f32::max);
    let mut mask: [f32; BANDS] = core::array::from_fn(|band| energies[band] - noise_floor[band]);
    let signal = mask;
    for band in 1..end {
        mask[band] = mask[band].max(mask[band - 1] - MASK_UP);
    }
    for band in (0..end - 1).rev() {
        mask[band] = mask[band].max(mask[band + 1] - MASK_DOWN);
    }
    let mut spread_weight = [0; BANDS];
    for band in 0..end {
        let ratio = signal[band] - (depth - DEPTH_RANGE).max(0.0).max(mask[band]);
        let shift = (-(floor(0.5 + ratio) as i32)).clamp(0, WEIGHT_MOST_SHIFT);
        spread_weight[band] = WEIGHT_FULL >> shift;
    }
    let mut boosts = Boosts { offsets: [0; BANDS], importance: [IMPORTANCE_DEFAULT; BANDS], spread_weight };
    if bytes < BOOST_LEAST_BYTES + BOOST_BYTES_PER_LM * lm as i32 {
        return boosts;
    }
    let mut follower = [0.0f32; BANDS];
    let mut last = 0;
    follower[0] = long[0];
    for band in 1..end {
        if long[band] > long[band - 1] + FOLLOW_RISE {
            last = band;
        }
        follower[band] = (follower[band - 1] + FOLLOW_UP).min(long[band]);
    }
    for band in (0..last).rev() {
        follower[band] = follower[band].min((follower[band + 1] + FOLLOW_DOWN).min(long[band]));
    }
    for band in 2..end - 2 {
        follower[band] = follower[band].max(median_of_5(&long[band - 2..]) - MEDIAN_OFFSET);
    }
    let low = median_of_3(&long[..]) - MEDIAN_OFFSET;
    follower[0] = follower[0].max(low);
    follower[1] = follower[1].max(low);
    let high = median_of_3(&long[end - 3..]) - MEDIAN_OFFSET;
    follower[end - 2] = follower[end - 2].max(high);
    follower[end - 1] = follower[end - 1].max(high);
    for band in 0..end {
        follower[band] = (energies[band] - follower[band].max(noise_floor[band])).max(0.0);
        boosts.importance[band] =
            floor(0.5 + IMPORTANCE_SCALE * pow2_f64(f64::from(follower[band].min(FOLLOW_MOST))) as f32) as i32;
        if !is_transient {
            // A frame at a constant rate that is no transient takes half the boost.
            follower[band] *= 0.5;
        }
        // The low bands take twice the boost, the high ones half.
        if band < LOW_BANDS {
            follower[band] *= 2.0;
        }
        if band >= HIGH_BANDS {
            follower[band] *= 0.5;
        }
    }
    if tone.toneishness > TONE_BOOST_PURITY {
        // Opus allocates too little to a pure tone: the bands nearest it get more.
        let bin = floor(0.5 + tone.frequency * SHORT as f32 / PI) as i32;
        for band in 0..end {
            for (reach, boost) in TONE_BOOSTS {
                let reach = reach as i32;
                if bin >= E_BANDS[band] as i32 - reach && bin <= E_BANDS[band + 1] as i32 + reach {
                    follower[band] += boost;
                }
            }
        }
        if bin >= E_BANDS[end] as i32 {
            follower[end - 1] += TONE_BOOSTS[0].1;
            follower[end - 2] += TONE_BOOSTS[1].1;
        }
    }
    // At a constant rate, boosts take no more than two thirds of the frame.
    let most = 2 * bytes / 3;
    let mut total = 0;
    for band in 0..end {
        let follower = follower[band].min(FOLLOW_MOST);
        let width = (E_BANDS[band + 1] - E_BANDS[band]) << lm;
        let (boost, bits) = if width < NARROW_BOOST {
            let boost = follower as i32;
            (boost, boost * width as i32 * BIT)
        } else if width > WIDE_BOOST {
            let boost = (follower * BOOST_STEPS) as i32;
            (boost, boost * width as i32 * BIT / BOOST_STEPS as i32)
        } else {
            let boost = (follower * width as f32 / NARROW_BOOST as f32) as i32;
            (boost, boost * NARROW_BOOST as i32 * BIT)
        };
        if (total + bits) / BIT / u8::BITS as i32 > most {
            // What is left of the two thirds, in eighths of a bit, stands for the band's count of quanta as libopus has it.
            boosts.offsets[band] = most * u8::BITS as i32 * BIT - total;
            break;
        }
        boosts.offsets[band] = boost;
        total += bits;
    }
    boosts
}

// The spreading decision of the frames before and what it keeps: the average of their spread, of their high bands', and the tapset it chose (spreading_decision).
pub(super) struct Spreading {
    average: i32,
    high_average: i32,
    pub(super) tapset: usize,
    pub(super) decision: usize,
}

impl Default for Spreading {
    fn default() -> Spreading {
        Spreading { average: AVERAGE_START, high_average: 0, tapset: 0, decision: SPREAD_NORMAL }
    }
}

impl Spreading {
    // How much a frame's shapes want their pulses spread, from how many of each band's bins hold a small share of its energy, weighed by `weights`; where `does_update` also the post-filter's tapset, from the high bands' (spreading_decision).
    pub(super) fn decide(
        &mut self,
        x: &[f32],
        end: usize,
        lm: usize,
        weights: &[i32; BANDS],
        does_update: bool,
    ) -> usize {
        let m = 1 << lm;
        if m * (E_BANDS[end] - E_BANDS[end - 1]) <= SPREAD_LEAST_BINS {
            return 0;
        }
        let (mut sum, mut weight, mut high) = (0, 0, 0);
        for band in 0..end {
            let n = m * (E_BANDS[band + 1] - E_BANDS[band]);
            if n <= SPREAD_LEAST_BINS {
                continue;
            }
            let mut counts = [0usize; 3];
            for &value in &x[m * E_BANDS[band]..][..n] {
                let share = value * value * n as f32;
                for (count, &threshold) in counts.iter_mut().zip(&SPREAD_SHARES) {
                    *count += usize::from(share < threshold);
                }
            }
            if band > BANDS - HIGH_BANDS_COUNTED {
                high += (HIGH_SCALE * (counts[1] + counts[0]) / n) as i32;
            }
            let level = counts.iter().filter(|&&count| 2 * count >= n).count() as i32;
            sum += level * weights[band];
            weight += weights[band];
        }
        if does_update {
            if high > 0 {
                high /= (HIGH_BANDS_COUNTED + end - BANDS) as i32;
            }
            self.high_average = (self.high_average + high) >> 1;
            let mut high = self.high_average;
            match self.tapset {
                2 => high += TAPSET_STEP,
                0 => high -= TAPSET_STEP,
                _ => {}
            }
            self.tapset = TAPSET_THRESHOLDS.iter().filter(|&&threshold| high > threshold).count();
        }
        let sum = (sum << AVERAGE_SHIFT) / weight;
        let sum = (sum + self.average) >> 1;
        self.average = sum;
        let levels = SPREAD_THRESHOLDS.len();
        let middle = (((levels - self.decision) as i32) << RANGE_SHIFT) + (1 << RANGE_SHIFT >> 1);
        let sum = (3 * sum + middle + 2) >> 2;
        levels - SPREAD_THRESHOLDS.iter().filter(|&&threshold| sum >= threshold).count()
    }
}

// The allocation trim, 0 to 10, of a frame of one channel: lower at low rates, for a spectrum that rises, and for a transient (alloc_trim_analysis).
pub(super) fn trim(energies: &[f32; BANDS], end: usize, tf_estimate: f32, rate: i32) -> i32 {
    let mut trim = if rate < TRIM_LOW_RATE {
        TRIM_LOW
    } else if rate < TRIM_HIGH_RATE {
        TRIM_LOW + TRIM_RATE_STEP * ((rate - TRIM_LOW_RATE) >> TRIM_RATE_SHIFT) as f32
    } else {
        TRIM_HIGH
    };
    let tilt: f32 = (0..end - 1).map(|band| energies[band] * (2 + 2 * band as i32 - end as i32) as f32).sum();
    let tilt = tilt / (end - 1) as f32;
    trim -= ((tilt + 1.0) / TRIM_TILT_SCALE).clamp(-TRIM_TILT_MOST, TRIM_TILT_MOST);
    trim -= 2.0 * tf_estimate;
    (floor(0.5 + trim) as i32).clamp(0, TRIM_MOST)
}
